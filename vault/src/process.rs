//! The one subprocess primitive: [`run`] spawns a child in its own process
//! group, feeds its stdin and drains stdout and stderr on their own threads,
//! and bounds the whole call (exit AND end-of-output) by one deadline. On the
//! deadline it SIGKILLs the group, so a grandchild that inherited the pipes
//! (fabric -> yt-dlp, `sh -c 'x &'`) cannot hold a drain open.
//!
//! A child in its own group no longer receives the terminal's Ctrl-C, so every
//! live group is recorded in a lock-free registry; [`kill_registered`] SIGKILLs
//! them, and [`install_interrupt_handler`] wires that to SIGINT/SIGTERM for
//! interactive commands.

use eyre::{Context, Result, eyre};
use std::io::{ErrorKind, Read, Write};
use std::os::unix::process::CommandExt;
use std::process::{Child, Command, ExitStatus, Stdio};
use std::sync::atomic::{AtomicI32, Ordering};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

/// How a [`run`] call ended.
#[derive(Debug)]
pub enum Outcome {
    /// The child exited and both output pipes reached end-of-file.
    Exited {
        status: ExitStatus,
        stdout: Vec<u8>,
        stderr: Vec<u8>,
    },
    /// The deadline passed first; the child's process group was SIGKILLed.
    TimedOut { after: Duration },
}

/// Exit code of an interactive command stopped by SIGINT or SIGTERM.
pub const INTERRUPTED_EXIT_CODE: i32 = 130;

/// Upper bound on concurrently live process groups. A slot array (not a
/// `Mutex<HashSet>`) because the signal handler must read it without locking.
const MAX_LIVE_GROUPS: usize = 1024;

static LIVE_GROUPS: [AtomicI32; MAX_LIVE_GROUPS] = [const { AtomicI32::new(0) }; MAX_LIVE_GROUPS];

const POLL_INTERVAL: Duration = Duration::from_millis(10);

/// Run `cmd` to completion with a wall-clock `timeout`.
///
/// `run` owns the child's stdio: stdin is a pipe fed `stdin` (or `/dev/null`
/// when `None`, never the parent's stdin, which may be a pipe that never
/// closes), stdout and stderr are pipes drained concurrently. `label` names
/// the call in logs and errors.
///
/// Spawn failures, pipe read errors, stdin write errors other than a broken
/// pipe (a child that exits without reading its input), and drain-thread
/// panics are `Err`, never empty output.
pub fn run(mut cmd: Command, stdin: Option<Vec<u8>>, timeout: Duration, label: &str) -> Result<Outcome> {
    log::debug!(
        "process::run: label={label} timeout={timeout:?} stdin_len={:?}",
        stdin.as_ref().map(Vec::len)
    );
    cmd.stdin(if stdin.is_some() { Stdio::piped() } else { Stdio::null() })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .process_group(0);

    let start = Instant::now();
    let mut child = cmd.spawn().with_context(|| format!("{label}: failed to spawn"))?;
    // process_group(0) makes the child its own group leader: pgid == pid.
    let pgid = i32::try_from(child.id()).map_err(|e| eyre!("{label}: child pid out of range: {e}"))?;
    let registration = match Registration::new(pgid) {
        Ok(r) => r,
        Err(e) => {
            kill_group(pgid);
            let _ = child.wait();
            return Err(e.wrap_err(format!("{label}: not started")));
        }
    };

    let feeder = spawn_feeder(&mut child, stdin);
    let stdout_drain = spawn_drain(child.stdout.take());
    let stderr_drain = spawn_drain(child.stderr.take());

    let deadline = start + timeout;
    let mut status: Option<ExitStatus> = None;
    loop {
        if status.is_none() {
            match child.try_wait() {
                Ok(s) => status = s,
                Err(e) => {
                    kill_group(pgid);
                    return Err(eyre!("{label}: failed to wait for child: {e}"));
                }
            }
        }
        let fed = feeder.as_ref().is_none_or(JoinHandle::is_finished);
        if status.is_some() && fed && stdout_drain.is_finished() && stderr_drain.is_finished() {
            break;
        }
        if Instant::now() >= deadline {
            kill_group(pgid);
            let _ = child.wait();
            // The group is dead, so every write end is closed and the drains
            // return; join them so no thread outlives the call.
            let _ = stdout_drain.join();
            let _ = stderr_drain.join();
            if let Some(f) = feeder {
                let _ = f.join();
            }
            drop(registration);
            let after = start.elapsed();
            log::warn!(
                "process::run: label={label} timed out after {after:?} (limit {timeout:?}); killed group {pgid}"
            );
            return Ok(Outcome::TimedOut { after });
        }
        std::thread::sleep(POLL_INTERVAL);
    }
    drop(registration);

    let status = status.ok_or_else(|| eyre!("{label}: exit status missing after wait"))?;
    let stdout = join_drain(stdout_drain, label, "stdout")?;
    let stderr = join_drain(stderr_drain, label, "stderr")?;
    if let Some(f) = feeder {
        match f.join() {
            Ok(Ok(())) => {}
            Ok(Err(e)) => return Err(eyre!("{label}: writing stdin failed: {e}")),
            Err(_) => return Err(eyre!("{label}: stdin writer thread panicked")),
        }
    }
    log::debug!(
        "process::run: label={label} status={status} stdout_bytes={} stderr_bytes={} elapsed={:?}",
        stdout.len(),
        stderr.len(),
        start.elapsed()
    );
    Ok(Outcome::Exited { status, stdout, stderr })
}

/// SIGKILL every process group a live [`run`] call has registered. Returns
/// how many groups were signalled. Async-signal-safe: no allocation, no lock,
/// no logging, so [`install_interrupt_handler`]'s handler calls it directly.
pub fn kill_registered() -> usize {
    let mut killed = 0;
    for slot in &LIVE_GROUPS {
        let pgid = slot.load(Ordering::SeqCst);
        if pgid > 0 {
            kill_group(pgid);
            killed += 1;
        }
    }
    killed
}

/// Install a SIGINT and SIGTERM handler that kills every registered process
/// group ([`kill_registered`]) and exits the process with
/// [`INTERRUPTED_EXIT_CODE`]. For interactive commands: a long-running daemon
/// keeps its own graceful-shutdown handler instead.
pub fn install_interrupt_handler() -> Result<()> {
    log::debug!("process::install_interrupt_handler: SIGINT SIGTERM");
    for signal in [libc::SIGINT, libc::SIGTERM] {
        // SAFETY: `on_interrupt` is an `extern "C" fn(c_int)` that only calls
        // async-signal-safe functions (atomic loads, killpg, _exit). The
        // sigaction struct is zero-initialized, then its mask emptied, before
        // being passed to sigaction(2).
        let rc = unsafe {
            let mut action: libc::sigaction = std::mem::zeroed();
            action.sa_sigaction = on_interrupt as *const () as libc::sighandler_t;
            libc::sigemptyset(&mut action.sa_mask);
            libc::sigaction(signal, &action, std::ptr::null_mut())
        };
        if rc != 0 {
            return Err(eyre!("sigaction({signal}) failed: {}", std::io::Error::last_os_error()));
        }
    }
    Ok(())
}

extern "C" fn on_interrupt(_signal: libc::c_int) {
    kill_registered();
    // SAFETY: _exit is async-signal-safe and does not return.
    unsafe { libc::_exit(INTERRUPTED_EXIT_CODE) }
}

fn kill_group(pgid: i32) {
    // SAFETY: killpg takes plain integers; a stale or already-dead group
    // yields ESRCH, which is harmless.
    unsafe {
        libc::killpg(pgid, libc::SIGKILL);
    }
}

/// A process group's slot in [`LIVE_GROUPS`], cleared on drop.
struct Registration {
    slot: usize,
    pgid: i32,
}

impl Registration {
    fn new(pgid: i32) -> Result<Self> {
        for (slot, cell) in LIVE_GROUPS.iter().enumerate() {
            if cell
                .compare_exchange(0, pgid, Ordering::SeqCst, Ordering::SeqCst)
                .is_ok()
            {
                return Ok(Self { slot, pgid });
            }
        }
        Err(eyre!(
            "process registry full: {MAX_LIVE_GROUPS} live process groups; refusing to start an unkillable child"
        ))
    }
}

impl Drop for Registration {
    fn drop(&mut self) {
        let _ = LIVE_GROUPS[self.slot].compare_exchange(self.pgid, 0, Ordering::SeqCst, Ordering::SeqCst);
    }
}

type Drain = JoinHandle<std::io::Result<Vec<u8>>>;

fn spawn_drain<R: Read + Send + 'static>(pipe: Option<R>) -> Drain {
    std::thread::spawn(move || {
        let mut buf = Vec::new();
        if let Some(mut pipe) = pipe {
            pipe.read_to_end(&mut buf)?;
        }
        Ok(buf)
    })
}

fn join_drain(drain: Drain, label: &str, stream: &str) -> Result<Vec<u8>> {
    match drain.join() {
        Ok(Ok(buf)) => Ok(buf),
        Ok(Err(e)) => Err(eyre!("{label}: reading {stream} failed: {e}")),
        Err(_) => Err(eyre!("{label}: {stream} drain thread panicked")),
    }
}

/// Write `input` to the child's stdin from its own thread, then close it (EOF).
/// A broken pipe means the child exited without reading all of it, which is
/// the child's choice, not an error.
fn spawn_feeder(child: &mut Child, input: Option<Vec<u8>>) -> Option<JoinHandle<std::io::Result<()>>> {
    let input = input?;
    let mut pipe = child.stdin.take()?;
    Some(std::thread::spawn(move || match pipe.write_all(&input) {
        Err(e) if e.kind() == ErrorKind::BrokenPipe => Ok(()),
        other => other,
    }))
}

#[cfg(test)]
pub(crate) mod tests;
