use super::*;

#[test]
fn test_extract_markdown_nonexistent_file() {
    let path = Path::new("/tmp/obsidian-borg-test-nonexistent-file.pdf");
    let result = extract_markdown(path, 30);
    assert!(result.is_err());
    let err = format!("{}", result.expect_err("should fail"));
    assert!(err.contains("does not exist"), "got: {err}");
}

fn sh_command(script: &str) -> Command {
    let mut cmd = Command::new("sh");
    cmd.args(["-c", script]);
    cmd
}

#[test]
fn one_mib_of_stdout_is_returned_whole() {
    let started = std::time::Instant::now();
    let text = run_extraction(
        sh_command("head -c 1048576 /dev/zero | tr '\\0' a"),
        Duration::from_secs(30),
        "big.pdf",
    )
    .expect("run");
    assert_eq!(text.len(), 1_048_576);
    assert!(
        started.elapsed() < Duration::from_secs(5),
        "took {:?}",
        started.elapsed()
    );
}

#[test]
fn timeout_is_err_naming_the_file() {
    let err = run_extraction(sh_command("sleep 30"), Duration::from_millis(300), "slow.pdf").expect_err("timeout");
    let msg = format!("{err:#}");
    assert!(msg.contains("timed out") && msg.contains("slow.pdf"), "{msg}");
}

#[test]
fn non_zero_exit_and_empty_output_are_err() {
    let failed = run_extraction(sh_command("echo bad >&2; exit 2"), Duration::from_secs(10), "a.pdf")
        .expect_err("non-zero exit");
    assert!(format!("{failed:#}").contains("failed for a.pdf"), "{failed:#}");
    let empty = run_extraction(sh_command("true"), Duration::from_secs(10), "a.pdf").expect_err("empty");
    assert!(format!("{empty:#}").contains("no output"), "{empty:#}");
}

#[test]
fn the_command_passes_the_path_to_markitdown() {
    let cmd = markitdown_command(Path::new("/x/y.pdf"));
    assert_eq!(cmd.get_program(), "markitdown");
    assert_eq!(cmd.get_args().collect::<Vec<_>>(), ["/x/y.pdf"]);
}
