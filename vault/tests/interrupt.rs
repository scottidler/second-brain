//! Interrupt harness for `vault::process::install_interrupt_handler`, the
//! handler sb installs for interactive commands.
//!
//! `harness = false`: the test re-executes its own binary as the harness
//! process (`--harness-child <pidfile>`), which installs the handler and runs a
//! call whose child backgrounds a `sleep 30` grandchild. The parent signals the
//! harness and asserts it exits 130 and that nothing in the call's process
//! group, the grandchild included, is left alive. Liveness is read from
//! `/proc/<pid>/stat` by pid and process group, never by name.

use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

const CHILD_FLAG: &str = "--harness-child";
const LIMIT: Duration = Duration::from_secs(10);

fn main() {
    let args: Vec<String> = std::env::args().collect();
    if let Some(i) = args.iter().position(|a| a == CHILD_FLAG) {
        harness_child(Path::new(&args[i + 1]));
    }
    if args.iter().any(|a| a == "--list") {
        println!("interrupt_handler_kills_the_grandchild: test");
        return;
    }
    for (name, signal) in [("SIGINT", libc::SIGINT), ("SIGTERM", libc::SIGTERM)] {
        signal_leaves_no_grandchild(name, signal);
        println!("interrupt harness: {name} ok");
    }
}

/// Runs inside the re-executed binary. Never returns: the handler `_exit`s.
fn harness_child(pidfile: &Path) -> ! {
    vault::process::install_interrupt_handler().expect("install handler");
    let mut sh = Command::new("sh");
    sh.arg("-c")
        .arg(format!("sleep 30 & echo $$ $! > {}; wait", pidfile.display()));
    let outcome = vault::process::run(sh, None, Duration::from_secs(60), "interrupt-harness");
    eprintln!("harness child: run returned instead of being interrupted: {outcome:?}");
    std::process::exit(1);
}

fn signal_leaves_no_grandchild(name: &str, signal: libc::c_int) {
    let dir = tempfile::tempdir().expect("tempdir");
    let pidfile = dir.path().join("pids");
    let harness = Command::new(std::env::current_exe().expect("current_exe"))
        .arg(CHILD_FLAG)
        .arg(&pidfile)
        .stdin(Stdio::null())
        .spawn()
        .expect("spawn harness");
    let mut guard = Cleanup { harness, pgid: None };

    let (pgid, grandchild) =
        wait_for_pids(&pidfile).unwrap_or_else(|| panic!("{name}: harness never started its call"));
    guard.pgid = Some(pgid);
    assert!(
        alive(grandchild),
        "{name}: precondition: grandchild {grandchild} running"
    );

    let harness_pid = i32::try_from(guard.harness.id()).expect("pid");
    // SAFETY: kill(2) with a pid we spawned and a valid signal number.
    assert_eq!(unsafe { libc::kill(harness_pid, signal) }, 0, "{name}: kill failed");

    let code = wait_exit(&mut guard.harness).unwrap_or_else(|| panic!("{name}: harness did not exit within {LIMIT:?}"));
    assert_eq!(code, Some(130), "{name}: harness exit code");
    assert!(
        eventually(|| !alive(grandchild)),
        "{name}: grandchild {grandchild} survived the signal"
    );
    assert!(
        eventually(|| live_group_members(pgid).is_empty()),
        "{name}: group {pgid} still has {:?}",
        live_group_members(pgid)
    );
}

/// Kills whatever a failed assertion would otherwise leave behind.
struct Cleanup {
    harness: Child,
    pgid: Option<i32>,
}

impl Drop for Cleanup {
    fn drop(&mut self) {
        if let Some(pgid) = self.pgid {
            // SAFETY: killpg with plain integers; ESRCH on a dead group is harmless.
            unsafe { libc::killpg(pgid, libc::SIGKILL) };
        }
        let _ = self.harness.kill();
        let _ = self.harness.wait();
    }
}

fn wait_for_pids(path: &PathBuf) -> Option<(i32, i32)> {
    let end = Instant::now() + LIMIT;
    while Instant::now() < end {
        if let Ok(text) = std::fs::read_to_string(path) {
            let mut it = text.split_whitespace().map(str::parse::<i32>);
            if let (Some(Ok(pgid)), Some(Ok(grandchild))) = (it.next(), it.next()) {
                return Some((pgid, grandchild));
            }
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    None
}

fn wait_exit(child: &mut Child) -> Option<Option<i32>> {
    let end = Instant::now() + LIMIT;
    while Instant::now() < end {
        if let Ok(Some(status)) = child.try_wait() {
            return Some(status.code());
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    None
}

fn eventually(mut cond: impl FnMut() -> bool) -> bool {
    let end = Instant::now() + LIMIT;
    while Instant::now() < end {
        if cond() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    cond()
}

/// `(state, pgrp)` from `/proc/<pid>/stat`, or `None` when the pid is gone.
fn proc_state(pid: i32) -> Option<(char, i32)> {
    let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    let rest = &stat[stat.rfind(')')? + 2..];
    let mut fields = rest.split_whitespace();
    let state = fields.next()?.chars().next()?;
    let _ppid = fields.next()?;
    let pgrp = fields.next()?.parse().ok()?;
    Some((state, pgrp))
}

fn alive(pid: i32) -> bool {
    matches!(proc_state(pid), Some((state, _)) if state != 'Z')
}

fn live_group_members(pgid: i32) -> Vec<i32> {
    let mut members = Vec::new();
    for entry in std::fs::read_dir("/proc").expect("read /proc").flatten() {
        let Ok(pid) = entry.file_name().to_string_lossy().parse::<i32>() else {
            continue;
        };
        if let Some((state, pgrp)) = proc_state(pid)
            && pgrp == pgid
            && state != 'Z'
        {
            members.push(pid);
        }
    }
    members
}
