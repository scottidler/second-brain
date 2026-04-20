#![allow(clippy::unwrap_used)]

use super::*;
use crate::stages::artifact::new_envelope;
use crate::types::{IngestKind, IngestMethod, RawCapture};
use std::collections::HashMap;

fn raw_with_body(body: &[u8]) -> RawCapture {
    let envelope = new_envelope("tg-test", IngestKind::Idea, IngestMethod::Telegram);
    RawCapture {
        envelope,
        body: body.to_vec(),
        attachments: HashMap::new(),
        fetched: None,
    }
}

#[test]
fn passthrough_returns_body_as_utf8() {
    let raw = raw_with_body(b"idea: hello world");
    let t = PassthroughExtractor.extract(&raw).unwrap();
    assert_eq!(t.text, "idea: hello world");
    assert_eq!(t.meta.extractor, "passthrough");
}

#[test]
fn passthrough_handles_empty_body() {
    let raw = raw_with_body(b"");
    let t = PassthroughExtractor.extract(&raw).unwrap();
    assert_eq!(t.text, "");
}
