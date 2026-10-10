//! Shutdown harness for `cortex::shutdown::Shutdown`, the listener the cortex
//! daemon loop polls.
//!
//! `harness = false`: the test re-executes its own binary as the child
//! (`--loop-child`), which runs the daemon loop's shape: a multi-thread runtime,
//! a `tokio::select!` of `shutdown.recv()` against an interval tick whose arm
//! does ~300 ms of `block_in_place` work. The parent signals the child DURING
//! that work and asserts the child exits 0 promptly after it. A lost signal
//! shows as the child still running at the deadline, never as a dead runner.
//!
//! Regression: the daemon built a fresh listener inside the `select!` each
//! iteration, so a SIGTERM delivered mid-tick had no receiver and was dropped;
//! systemd SIGKILLed cortex after `TimeoutStopSec` (desk, 2026-10-05).
//!
//! The second child (`--work-child`) drives `cortex::shutdown::StopHandle`: its
//! tick arm is 30 s of `block_in_place` work that polls `is_stopped()` every
//! 100 ms, as a check point does. The child must exit 0 within 2 s of the signal,
//! long before the work would end on its own. Regression: a tick's work never
//! looked at the signal, so a stop waited out the whole tick and systemd
//! SIGKILLed cortex (ripr and desk, 2026-10-10).

use std::io::{BufRead, BufReader};
use std::process::{Child, Command, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant};

const CHILD_FLAG: &str = "--loop-child";
const WORK_CHILD_FLAG: &str = "--work-child";
const TICK_WORK: Duration = Duration::from_millis(300);
const SIGNAL_INTO_WORK: Duration = Duration::from_millis(100);
const EXIT_AFTER_WORK: Duration = Duration::from_secs(2);
const LONG_WORK: Duration = Duration::from_secs(30);
const CHECK_POINT_EVERY: Duration = Duration::from_millis(100);
const EXIT_AFTER_SIGNAL: Duration = Duration::from_secs(2);

fn main() {
    let args: Vec<String> = std::env::args().collect();
    if args.iter().any(|a| a == CHILD_FLAG) {
        loop_child();
    }
    if args.iter().any(|a| a == WORK_CHILD_FLAG) {
        work_child();
    }
    if args.iter().any(|a| a == "--list") {
        println!("signal_during_tick_work_stops_the_loop: test");
        println!("signal_during_long_work_stops_at_a_check_point: test");
        return;
    }
    for (name, signal) in [("SIGTERM", libc::SIGTERM), ("SIGINT", libc::SIGINT)] {
        signal_during_tick_work_stops_the_loop(name, signal);
        println!("shutdown harness: {name} ok");
    }
    for (name, signal) in [("SIGTERM", libc::SIGTERM), ("SIGINT", libc::SIGINT)] {
        let elapsed = signal_during_long_work_stops_at_a_check_point(name, signal);
        println!("shutdown harness: {name} during 30s work ok (exited {elapsed:?} after the signal)");
    }
}

/// Runs inside the re-executed binary: the daemon loop's shape, nothing else.
fn loop_child() -> ! {
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .expect("runtime");
    runtime.block_on(async {
        let mut shutdown = cortex::shutdown::Shutdown::listen();
        let mut tick = tokio::time::interval(Duration::from_millis(10));
        loop {
            tokio::select! {
                biased;
                () = shutdown.recv() => break,
                _ = tick.tick() => {
                    println!("tick");
                    tokio::task::block_in_place(|| std::thread::sleep(TICK_WORK));
                }
            }
        }
    });
    std::process::exit(0);
}

/// Runs inside the re-executed binary: the daemon loop's shape with a
/// `StopHandle`, and one tick of work far longer than any stop budget.
fn work_child() -> ! {
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .expect("runtime");
    runtime.block_on(async {
        let stop = cortex::shutdown::StopHandle::listen();
        let mut tick = tokio::time::interval(Duration::from_millis(10));
        loop {
            tokio::select! {
                biased;
                () = stop.stopped() => break,
                _ = tick.tick() => {
                    println!("tick");
                    tokio::task::block_in_place(|| {
                        let end = Instant::now() + LONG_WORK;
                        while Instant::now() < end && !stop.is_stopped() {
                            std::thread::sleep(CHECK_POINT_EVERY);
                        }
                    });
                }
            }
        }
    });
    std::process::exit(0);
}

fn signal_during_tick_work_stops_the_loop(name: &str, signal: libc::c_int) {
    let deadline = (TICK_WORK - SIGNAL_INTO_WORK) + EXIT_AFTER_WORK;
    signal_child_during_work(
        name,
        signal,
        CHILD_FLAG,
        deadline,
        "after the tick's work ended: the signal was lost",
    );
}

/// Returns how long after the signal the child exited.
fn signal_during_long_work_stops_at_a_check_point(name: &str, signal: libc::c_int) -> Duration {
    signal_child_during_work(
        name,
        signal,
        WORK_CHILD_FLAG,
        EXIT_AFTER_SIGNAL,
        "after the signal: the work never saw the stop flag",
    )
}

/// Spawn the child in `mode`, signal it `SIGNAL_INTO_WORK` into its first
/// tick's work, and assert it exits 0 within `deadline` of the signal.
fn signal_child_during_work(name: &str, signal: libc::c_int, mode: &str, deadline: Duration, lost: &str) -> Duration {
    let mut child = Command::new(std::env::current_exe().expect("current_exe"))
        .arg(mode)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .spawn()
        .expect("spawn loop child");
    let stdout = child.stdout.take().expect("child stdout");
    let mut guard = Cleanup(child);

    // Drain stdout on a thread so a chatty child never blocks on a full pipe.
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        for line in BufReader::new(stdout).lines().map_while(Result::ok) {
            if tx.send(line).is_err() {
                return;
            }
        }
    });
    let first = rx
        .recv_timeout(Duration::from_secs(10))
        .unwrap_or_else(|_| panic!("{name}: child never started a tick"));
    assert_eq!(first, "tick", "{name}: child's first line");

    std::thread::sleep(SIGNAL_INTO_WORK);
    let pid = i32::try_from(guard.0.id()).expect("pid");
    // SAFETY: kill(2) with a pid we spawned and a valid signal number.
    assert_eq!(unsafe { libc::kill(pid, signal) }, 0, "{name}: kill failed");
    let signalled = Instant::now();

    let status = loop {
        if let Some(status) = guard.0.try_wait().expect("try_wait") {
            break status;
        }
        assert!(
            signalled.elapsed() < deadline,
            "{name} ({mode}): loop still running {deadline:?} {lost}"
        );
        std::thread::sleep(Duration::from_millis(20));
    };
    let elapsed = signalled.elapsed();
    assert_eq!(
        status.code(),
        Some(0),
        "{name} ({mode}): child must break the loop and exit 0, not die by the signal ({status})"
    );
    elapsed
}

/// Kills the child when an assertion fails before it exits.
struct Cleanup(Child);

impl Drop for Cleanup {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}
