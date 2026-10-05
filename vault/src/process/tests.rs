//! Every test that calls `run` holds `#[serial(process)]`: one of them calls
//! `kill_registered`, which would SIGKILL a sibling test's child.
//!
//! Output is generated with `head -c N /dev/zero | tr '\0' a`, not bare
//! `head -c`: on this kernel bare `head` splices into the pipe and passes even
//! against a loop that never drains.

use super::*;
use serial_test::serial;
use std::os::unix::process::ExitStatusExt;
use std::path::Path;

const MIB: usize = 1024 * 1024;
const GENEROUS: Duration = Duration::from_secs(30);
const PROMPT: Duration = Duration::from_secs(5);

fn sh(script: &str) -> Command {
    let mut cmd = Command::new("sh");
    cmd.arg("-c").arg(script);
    cmd
}

fn exited(outcome: Outcome) -> (ExitStatus, Vec<u8>, Vec<u8>) {
    match outcome {
        Outcome::Exited { status, stdout, stderr } => (status, stdout, stderr),
        Outcome::TimedOut { after } => panic!("expected the child to exit, it timed out after {after:?}"),
    }
}

/// `(state, pgrp)` from `/proc/<pid>/stat`, or `None` when the pid is gone.
fn proc_state(pid: i32) -> Option<(char, i32)> {
    let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    // comm may contain spaces; every field after it follows the last ')'.
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

/// Non-zombie processes whose process group is `pgid`.
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

/// Polls `cond` until it holds or `limit` passes; returns whether it held.
fn eventually(limit: Duration, mut cond: impl FnMut() -> bool) -> bool {
    let end = Instant::now() + limit;
    while Instant::now() < end {
        if cond() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    cond()
}

/// `(pgid, grandchild pid)` written by a script as `echo $$ $! > file`.
fn read_pids(path: &Path) -> Option<(i32, i32)> {
    let text = std::fs::read_to_string(path).ok()?;
    let mut it = text.split_whitespace().map(str::parse::<i32>);
    match (it.next(), it.next()) {
        (Some(Ok(pgid)), Some(Ok(grandchild))) => Some((pgid, grandchild)),
        _ => None,
    }
}

/// Replaces this process's fd 0 with the read end of a pipe whose write end
/// stays open, the condition that hung an inherited-stdin `fabric --version`:
/// a child that inherited it would block reading forever. Restores fd 0 on
/// drop. Only for `#[serial(process)]` tests: fd 0 is process-wide.
pub(crate) struct ParentStdinIsAnOpenPipe {
    saved: i32,
    write_end: i32,
}

impl ParentStdinIsAnOpenPipe {
    pub(crate) fn install() -> Self {
        let mut fds = [0i32; 2];
        // SAFETY: plain fd syscalls on fds this guard owns; every return code
        // is checked. CLOEXEC keeps the write end and the saved copy out of
        // spawned children; dup2 onto 0 leaves fd 0 inheritable on purpose.
        unsafe {
            assert_eq!(libc::pipe2(fds.as_mut_ptr(), libc::O_CLOEXEC), 0, "pipe2");
            let saved = libc::fcntl(0, libc::F_DUPFD_CLOEXEC, 3);
            assert!(saved >= 0, "save fd 0");
            assert_eq!(libc::dup2(fds[0], 0), 0, "dup2 onto fd 0");
            libc::close(fds[0]);
            Self {
                saved,
                write_end: fds[1],
            }
        }
    }
}

impl Drop for ParentStdinIsAnOpenPipe {
    fn drop(&mut self) {
        // SAFETY: restores the fd 0 saved in `install` and closes the guard's fds.
        unsafe {
            libc::dup2(self.saved, 0);
            libc::close(self.saved);
            libc::close(self.write_end);
        }
    }
}

/// SIGKILLs a test's process group on drop, so a failed assertion cannot leave
/// a `sleep 30` behind.
struct GroupReaper(Option<i32>);

impl Drop for GroupReaper {
    fn drop(&mut self) {
        if let Some(pgid) = self.0 {
            kill_group(pgid);
        }
    }
}

#[test]
#[serial(process)]
fn one_mib_of_stdout_is_drained_in_full() {
    let start = Instant::now();
    let outcome = run(
        sh(&format!("head -c {MIB} /dev/zero | tr '\\0' a")),
        None,
        GENEROUS,
        "stdout-1mib",
    )
    .expect("run");
    let (status, stdout, stderr) = exited(outcome);
    assert!(start.elapsed() < PROMPT, "took {:?}", start.elapsed());
    assert!(status.success());
    assert_eq!(stdout.len(), MIB);
    assert!(stdout.iter().all(|&b| b == b'a'));
    assert!(stderr.is_empty());
}

#[test]
#[serial(process)]
fn one_mib_of_stderr_is_drained_in_full() {
    let start = Instant::now();
    let outcome = run(
        sh(&format!("head -c {MIB} /dev/zero | tr '\\0' a >&2")),
        None,
        GENEROUS,
        "stderr-1mib",
    )
    .expect("run");
    let (status, stdout, stderr) = exited(outcome);
    assert!(start.elapsed() < PROMPT, "took {:?}", start.elapsed());
    assert!(status.success());
    assert!(stdout.is_empty());
    assert_eq!(stderr.len(), MIB);
}

/// `cat | tr x x`, not bare `cat`: uutils cat raises its stdout pipe to
/// `pipe-max-size` (1 MiB here) with F_SETPIPE_SZ, so 1 MiB fits in an
/// undrained pipe and bare `cat` passes against a loop that never drains.
/// `tr` writes into a default 64 KiB pipe.
#[test]
#[serial(process)]
fn one_mib_of_stdin_round_trips_through_cat() {
    let input: Vec<u8> = (0..MIB).map(|i| (i % 251) as u8).collect();
    let start = Instant::now();
    let outcome = run(sh("cat | tr x x"), Some(input.clone()), GENEROUS, "cat-1mib").expect("run");
    let (status, stdout, _) = exited(outcome);
    assert!(start.elapsed() < PROMPT, "took {:?}", start.elapsed());
    assert!(status.success());
    assert_eq!(stdout, input);
}

/// The child's stdin must be /dev/null, never the parent's: an inherited
/// stdin that is a never-closing pipe hung `fabric --version` for 37 minutes.
/// fd 0 is made such a pipe here, so an inherited stdin blocks `cat`.
#[test]
#[serial(process)]
fn a_child_reading_stdin_exits_promptly_when_given_none() {
    let _stdin = ParentStdinIsAnOpenPipe::install();
    let start = Instant::now();
    let outcome = run(Command::new("cat"), None, PROMPT, "cat-no-stdin").expect("run");
    let (status, stdout, _) = exited(outcome);
    assert!(start.elapsed() < PROMPT, "took {:?}", start.elapsed());
    assert!(status.success());
    assert!(stdout.is_empty());
}

#[test]
#[serial(process)]
fn timeout_kills_a_backgrounded_grandchild() {
    let dir = tempfile::tempdir().expect("tempdir");
    let pids = dir.path().join("pids");
    let script = format!("sleep 30 & echo $$ $! > {}; wait", pids.display());
    let start = Instant::now();
    let outcome = run(sh(&script), None, Duration::from_secs(1), "timeout-grandchild").expect("run");
    let (pgid, grandchild) = read_pids(&pids).expect("script wrote its pids");
    let _reaper = GroupReaper(Some(pgid));
    assert!(matches!(outcome, Outcome::TimedOut { .. }), "got {outcome:?}");
    assert!(start.elapsed() < PROMPT, "took {:?}", start.elapsed());
    assert!(
        eventually(PROMPT, || !alive(grandchild)),
        "grandchild {grandchild} survived the timeout"
    );
    assert!(
        eventually(PROMPT, || live_group_members(pgid).is_empty()),
        "group {pgid} still has {:?}",
        live_group_members(pgid)
    );
}

/// The pipe-holding grandchild hazard: the leader has exited, but a backgrounded
/// grandchild holds stdout open, so draining to end-of-file would block for
/// the grandchild's whole life. The deadline bounds it and kills the group.
#[test]
#[serial(process)]
fn timeout_bounds_a_grandchild_holding_the_pipe_after_the_leader_exits() {
    let dir = tempfile::tempdir().expect("tempdir");
    let pids = dir.path().join("pids");
    let script = format!("sleep 30 & echo $$ $! > {}", pids.display());
    let start = Instant::now();
    let outcome = run(sh(&script), None, Duration::from_secs(1), "leader-exited").expect("run");
    let (pgid, grandchild) = read_pids(&pids).expect("script wrote its pids");
    let _reaper = GroupReaper(Some(pgid));
    assert!(matches!(outcome, Outcome::TimedOut { .. }), "got {outcome:?}");
    assert!(start.elapsed() < PROMPT, "took {:?}", start.elapsed());
    assert!(
        eventually(PROMPT, || !alive(grandchild)),
        "grandchild {grandchild} survived"
    );
}

#[test]
#[serial(process)]
fn kill_registered_kills_a_grandchild_of_a_live_call() {
    let dir = tempfile::tempdir().expect("tempdir");
    let pids = dir.path().join("pids");
    let script = format!("sleep 30 & echo $$ $! > {}; wait", pids.display());
    let (tx, rx) = std::sync::mpsc::channel();
    let caller = std::thread::spawn(move || {
        let _ = tx.send(run(sh(&script), None, GENEROUS, "live-call"));
    });
    assert!(
        eventually(PROMPT, || read_pids(&pids).is_some()),
        "script never wrote its pids"
    );
    let (pgid, grandchild) = read_pids(&pids).expect("pids");
    let _reaper = GroupReaper(Some(pgid));
    assert!(alive(grandchild), "precondition: grandchild running before the kill");

    assert_eq!(kill_registered(), 1, "exactly the one live call is registered");

    let outcome = rx
        .recv_timeout(PROMPT)
        .expect("run returns promptly once its group is killed")
        .expect("run");
    caller.join().expect("caller thread");
    let (status, _, _) = exited(outcome);
    assert_eq!(status.signal(), Some(libc::SIGKILL));
    assert!(
        eventually(PROMPT, || !alive(grandchild)),
        "grandchild {grandchild} survived"
    );
    assert!(eventually(PROMPT, || live_group_members(pgid).is_empty()));
}

#[test]
#[serial(process)]
fn a_finished_call_leaves_nothing_registered() {
    let outcome = run(sh("exit 0"), None, GENEROUS, "deregister").expect("run");
    assert!(exited(outcome).0.success());
    assert_eq!(kill_registered(), 0, "a finished call's group must not stay registered");
}

#[test]
#[serial(process)]
fn nonzero_exit_carries_status_and_stderr() {
    let outcome = run(sh("echo boom >&2; exit 3"), None, GENEROUS, "fails").expect("run");
    let (status, _, stderr) = exited(outcome);
    assert_eq!(status.code(), Some(3));
    assert_eq!(String::from_utf8_lossy(&stderr).trim(), "boom");
}

#[test]
#[serial(process)]
fn a_child_that_ignores_its_stdin_is_not_an_error() {
    let outcome =
        run(sh("exit 0"), Some(vec![b'x'; MIB]), GENEROUS, "ignores-stdin").expect("broken pipe is not an error");
    assert!(exited(outcome).0.success());
}

#[test]
#[serial(process)]
fn spawn_failure_is_an_error_naming_the_label() {
    let err = run(
        Command::new("/nonexistent/vault-process-test-binary"),
        None,
        GENEROUS,
        "missing-binary",
    )
    .expect_err("spawning a missing binary must fail");
    assert!(format!("{err:#}").contains("missing-binary"), "error: {err:#}");
}
