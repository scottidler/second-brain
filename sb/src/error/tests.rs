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

#[track_caller]
fn caller_location() -> &'static Location<'static> {
    Location::caller()
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
    let handler = Handler {
        verbose: false,
        location: None,
    };
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
    let handler = Handler {
        verbose: false,
        location: None,
    };
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

#[test]
fn compact_mode_hides_location_even_when_captured() {
    let handler = Handler {
        verbose: false,
        location: Some(caller_location()),
    };
    let err = StubError {
        msg: "boom",
        source: None,
    };
    let out = render(&handler, &err);
    assert!(!out.contains("Location"), "compact mode must not leak Location: {out}");
}

#[test]
fn verbose_mode_prints_location_when_captured() {
    let handler = Handler {
        verbose: true,
        location: Some(caller_location()),
    };
    let err = StubError {
        msg: "boom",
        source: None,
    };
    let out = render(&handler, &err);
    assert!(out.contains("Location:"), "verbose mode must emit Location: {out}");
    assert!(
        out.contains("tests.rs:"),
        "Location should point at the test file: {out}"
    );
}

#[test]
fn verbose_mode_without_captured_location_is_silent() {
    let handler = Handler {
        verbose: true,
        location: None,
    };
    let err = StubError {
        msg: "boom",
        source: None,
    };
    let out = render(&handler, &err);
    assert_eq!(out, "boom");
    assert!(!out.contains("Location"));
}

#[test]
fn track_caller_method_stores_location() {
    let mut handler = Handler {
        verbose: true,
        location: None,
    };
    let location = caller_location();
    handler.track_caller(location);
    assert!(handler.location.is_some());
}
