use super::*;

#[test]
fn test_generate_format() {
    let id = generate(Method::Telegram);
    let re = regex::Regex::new(r"^[a-z]{2}-[0-9a-f]{6}$").expect("valid regex");
    assert!(re.is_match(&id), "trace ID '{id}' does not match expected format");
}

#[test]
fn test_method_prefixes() {
    assert_eq!(method_prefix(Method::Telegram), "tg");
    assert_eq!(method_prefix(Method::Discord), "dc");
    assert_eq!(method_prefix(Method::Http), "ht");
    assert_eq!(method_prefix(Method::Clipboard), "cb");
    assert_eq!(method_prefix(Method::Cli), "cl");
    assert_eq!(method_prefix(Method::Ntfy), "nf");
    assert_eq!(method_prefix(Method::Signal), "sg");
    assert_eq!(method_prefix(Method::Manual), "mn");
}

#[test]
fn test_sequential_uniqueness() {
    let id1 = generate(Method::Cli);
    let id2 = generate(Method::Cli);
    assert_ne!(id1, id2, "two sequential trace IDs should differ");
}

#[test]
fn test_different_methods_different_prefix() {
    let tg = generate(Method::Telegram);
    let dc = generate(Method::Discord);
    assert!(tg.starts_with("tg-"));
    assert!(dc.starts_with("dc-"));
}
