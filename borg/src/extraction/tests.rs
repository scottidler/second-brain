use super::*;

#[test]
fn test_extract_markdown_nonexistent_file() {
    let path = Path::new("/tmp/obsidian-borg-test-nonexistent-file.pdf");
    let result = extract_markdown(path, 30);
    assert!(result.is_err());
    let err = format!("{}", result.expect_err("should fail"));
    assert!(err.contains("does not exist"), "got: {err}");
}

#[test]
fn test_is_available() {
    // Just ensure it doesn't panic - result depends on environment
    let _ = is_available();
}
