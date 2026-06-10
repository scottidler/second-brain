use super::*;

#[test]
fn strips_yaml_fence() {
    let raw = "```yaml\nsummary: hi\n```";
    assert_eq!(strip_fences(raw), "summary: hi");
}

#[test]
fn strips_bare_fence() {
    let raw = "```\nsummary: hi\n```";
    assert_eq!(strip_fences(raw), "summary: hi");
}

#[test]
fn passes_through_unfenced() {
    let raw = "summary: hi\nclaims: []";
    assert_eq!(strip_fences(raw), "summary: hi\nclaims: []");
}

#[test]
fn unfenced_with_embedded_fence_is_not_truncated() {
    // Regression for the truncation bug: unfenced YAML whose content contains
    // an embedded code fence must NOT be cut at that fence.
    let raw = "summary: see the snippet\nclaims:\n  - text: \"```rust let x = 1; ```\"";
    let out = strip_fences(raw);
    assert!(
        out.contains("let x = 1;"),
        "embedded fence content was truncated: {out:?}"
    );
}

#[test]
fn fenced_with_trailing_prose_strips_to_close() {
    let raw = "```yaml\nsummary: hi\n```\nignored trailing";
    assert_eq!(strip_fences(raw), "summary: hi");
}

#[test]
fn strips_fence_with_surrounding_whitespace() {
    // Leading/trailing whitespace around the fence (LLMs add blank lines) is
    // trimmed before the fence is detected.
    let raw = "\n\n  ```yaml\nsummary: hi\n```  \n\n";
    assert_eq!(strip_fences(raw), "summary: hi");
}

#[test]
fn strips_fence_with_multiline_yaml_body() {
    // A multi-line body inside the fence is returned intact (only the fence
    // markers are removed), so every consumer's serde_yaml parse sees clean
    // YAML.
    let raw = "```yaml\nsummary: hi\nclaims:\n  - text: one\n  - text: two\n```";
    assert_eq!(strip_fences(raw), "summary: hi\nclaims:\n  - text: one\n  - text: two");
}

#[test]
fn preserves_colons_inside_unfenced_body() {
    // Colons inside values must survive untouched (no fence present).
    let raw = "summary: \"ratio is 3:1 at 12:00\"";
    assert_eq!(strip_fences(raw), "summary: \"ratio is 3:1 at 12:00\"");
}

#[test]
fn approx_tokens_uses_four_char_rule() {
    assert_eq!(approx_tokens(0), 0);
    assert_eq!(approx_tokens(4), 1);
    assert_eq!(approx_tokens(401), 100);
}
