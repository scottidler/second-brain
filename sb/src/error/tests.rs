#![allow(clippy::unwrap_used)]

use super::*;
use std::fmt::Write;

fn render(handler: &Handler, err: &(dyn Error + 'static)) -> String {
    struct Wrap<'a> {
        handler: &'a Handler,
        err: &'a (dyn Error + 'static),
    }
    impl fmt::Display for Wrap<'_> {
        fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
            self.handler.debug(self.err, f)
        }
    }
    let mut out = String::new();
    write!(&mut out, "{}", Wrap { handler, err }).unwrap();
    out
}

#[derive(Debug)]
struct StubError {
    msg: &'static str,
    source: Option<Box<StubError>>,
}

impl fmt::Display for StubError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.msg)
    }
}

impl Error for StubError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        self.source.as_deref().map(|e| e as &(dyn Error + 'static))
    }
}

#[test]
fn compact_mode_hides_chain_when_no_source() {
    let handler = Handler { verbose: false };
    let err = StubError {
        msg: "top message",
        source: None,
    };
    let out = render(&handler, &err);
    assert_eq!(out, "top message");
    assert!(!out.contains("Location"));
}

#[test]
fn compact_mode_shows_caused_by_chain() {
    let handler = Handler { verbose: false };
    let err = StubError {
        msg: "top",
        source: Some(Box::new(StubError {
            msg: "middle",
            source: Some(Box::new(StubError {
                msg: "leaf",
                source: None,
            })),
        })),
    };
    let out = render(&handler, &err);
    assert!(out.starts_with("top\n\nCaused by:"), "{out}");
    assert!(out.contains("1: middle"), "{out}");
    assert!(out.contains("2: leaf"), "{out}");
    assert!(!out.contains("Location"));
}
