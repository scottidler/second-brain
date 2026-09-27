use std::path::Path;
use std::process::Command;

fn main() {
    let git_describe = Command::new("git")
        .args(["describe", "--tags", "--always"])
        .output()
        .and_then(|output| {
            if output.status.success() {
                Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
            } else {
                Err(std::io::Error::other("git describe failed"))
            }
        })
        .unwrap_or_else(|_| env!("CARGO_PKG_VERSION").to_string());

    println!("cargo:rustc-env=GIT_DESCRIBE={}", git_describe);

    // Ask git where HEAD/refs/packed-refs live instead of guessing `../.git/...`:
    // in a worktree `.git` is a file, HEAD sits under `<bare>/worktrees/<name>/`
    // and refs are shared under `<bare>/`. Only existing paths are emitted,
    // because Cargo treats a missing watched path as always-stale and would
    // recompile sb on every build.
    let watched = Command::new("git")
        .args([
            "rev-parse",
            "--path-format=absolute",
            "--git-path",
            "HEAD",
            "--git-path",
            "refs",
            "--git-path",
            "packed-refs",
        ])
        .output()
        .ok()
        .filter(|output| output.status.success())
        .map(|output| String::from_utf8_lossy(&output.stdout).into_owned())
        .unwrap_or_default();
    for path in watched.lines().filter(|p| Path::new(p).exists()) {
        println!("cargo:rerun-if-changed={path}");
    }
}
