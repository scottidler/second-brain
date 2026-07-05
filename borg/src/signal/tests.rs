//! Phase 6 tests for the Signal transport. Privacy filter, rate gate, and
//! classify outcomes per `docs/design/2026-05-24-signal-as-borg-transport.md`.

use super::*;
use signal_rs::{AttachmentPointer, Envelope, ReadReceipt, Recipient, SyncMessage};

const ALLOWED_ACI: &str = "11111111-1111-1111-1111-111111111111";
const STRANGER_ACI: &str = "22222222-2222-2222-2222-222222222222";
const OTHER_ACI: &str = "33333333-3333-3333-3333-333333333333";
const PNI: &str = "44444444-4444-4444-4444-444444444444";

fn allowed() -> Vec<String> {
    vec![ALLOWED_ACI.to_string()]
}

fn mk_attachment(content_type: Option<&str>, file_name: Option<&str>, voice_note: bool) -> AttachmentPointer {
    AttachmentPointer {
        cdn_id: 1,
        cdn_key: None,
        cdn_number: 0,
        content_type: content_type.map(|s| s.to_string()),
        size: Some(1024),
        digest: vec![],
        key: vec![],
        file_name: file_name.map(|s| s.to_string()),
        caption: None,
        width: None,
        height: None,
        voice_note,
        borderless: false,
        gif: false,
        upload_timestamp: None,
        blurhash: None,
    }
}

fn mk_sent(
    destination: Option<Recipient>,
    group_id: Option<Vec<u8>>,
    body: Option<&str>,
    attachments: Vec<AttachmentPointer>,
) -> Envelope {
    Envelope::SyncMessage(SyncMessage::Sent {
        destination,
        group_id,
        timestamp: 0,
        body: body.map(|s| s.to_string()),
        attachments,
        edit_of_timestamp: None,
        expire_in_seconds: None,
    })
}

fn mk_data(
    source: Recipient,
    group_id: Option<Vec<u8>>,
    body: Option<&str>,
    attachments: Vec<AttachmentPointer>,
) -> Envelope {
    Envelope::DataMessage {
        source,
        source_device: 1,
        timestamp: 0,
        group_id,
        body: body.map(|s| s.to_string()),
        attachments,
        quote: None,
        edit_of_timestamp: None,
        expire_in_seconds: None,
    }
}

// ---------------- Privacy filter: positive cases ----------------

#[test]
fn accepts_note_to_self_with_no_group() {
    let env = mk_sent(Some(Recipient::SelfSync), None, Some("hi"), vec![]);
    assert_eq!(accepted_envelope(&env, &allowed()), Some(AcceptedSource::SelfSync));
}

#[test]
fn accepts_allowed_peer_dm_with_no_group() {
    let env = mk_data(Recipient::Aci(ALLOWED_ACI.to_string()), None, Some("hi"), vec![]);
    assert_eq!(
        accepted_envelope(&env, &allowed()),
        Some(AcceptedSource::Peer {
            aci: ALLOWED_ACI.to_string()
        })
    );
}

// ---------------- Allowed-senders enforcement ----------------

#[test]
fn rejects_stranger_dm_even_without_group() {
    let env = mk_data(Recipient::Aci(STRANGER_ACI.to_string()), None, Some("spam"), vec![]);
    assert!(accepted_envelope(&env, &allowed()).is_none());
}

#[test]
fn rejects_dm_when_allowed_list_is_empty() {
    let env = mk_data(Recipient::Aci(ALLOWED_ACI.to_string()), None, Some("hi"), vec![]);
    assert!(accepted_envelope(&env, &[]).is_none());
}

// ---------------- Group filter (mandatory; group shares variant with 1:1) ----------------

#[test]
fn rejects_note_to_self_shaped_group_fanout() {
    let env = mk_sent(
        Some(Recipient::SelfSync),
        Some(vec![0u8; 16]),
        Some("group msg"),
        vec![],
    );
    assert!(accepted_envelope(&env, &allowed()).is_none());
}

#[test]
fn rejects_allowed_peer_group_chatter() {
    let env = mk_data(
        Recipient::Aci(ALLOWED_ACI.to_string()),
        Some(vec![1u8; 16]),
        Some("group chatter"),
        vec![],
    );
    assert!(accepted_envelope(&env, &allowed()).is_none());
}

// ---------------- Failure-direction filter (pattern-loosening regressions) ----------------

#[test]
fn rejects_sent_with_non_selfsync_destination_aci() {
    let env = mk_sent(
        Some(Recipient::Aci(OTHER_ACI.to_string())),
        None,
        Some("not self"),
        vec![],
    );
    assert!(accepted_envelope(&env, &allowed()).is_none());
}

#[test]
fn rejects_sent_with_none_destination() {
    let env = mk_sent(None, None, Some("body"), vec![]);
    assert!(accepted_envelope(&env, &allowed()).is_none());
}

#[test]
fn rejects_pni_source_even_if_string_matches_allowed_aci() {
    let mut allowlist = allowed();
    allowlist.push(PNI.to_string());
    let env = mk_data(Recipient::Pni(PNI.to_string()), None, Some("pni"), vec![]);
    assert!(
        accepted_envelope(&env, &allowlist).is_none(),
        "allowlist comparison must be Aci-typed, not string-on-any-variant"
    );
}

// ---------------- Unknown variants stay rejected ----------------

#[test]
fn rejects_sync_message_read() {
    let env = Envelope::SyncMessage(SyncMessage::Read { reads: vec![] });
    assert!(accepted_envelope(&env, &allowed()).is_none());
}

#[test]
fn rejects_sync_message_read_with_payload() {
    let env = Envelope::SyncMessage(SyncMessage::Read {
        reads: vec![ReadReceipt {
            sender: Recipient::Aci(ALLOWED_ACI.to_string()),
            timestamp: 1,
        }],
    });
    assert!(accepted_envelope(&env, &allowed()).is_none());
}

#[test]
fn rejects_receipt() {
    let env = Envelope::Receipt {
        receipt_kind: signal_rs::ReceiptKind::Delivery,
        source: Recipient::Aci(ALLOWED_ACI.to_string()),
        timestamps: vec![1, 2, 3],
    };
    assert!(accepted_envelope(&env, &allowed()).is_none());
}

#[test]
fn rejects_typing() {
    let env = Envelope::Typing {
        source: Recipient::Aci(ALLOWED_ACI.to_string()),
        group_id: None,
        started: true,
        timestamp: 0,
    };
    assert!(accepted_envelope(&env, &allowed()).is_none());
}

#[test]
fn rejects_edit() {
    let env = Envelope::Edit {
        source: Recipient::Aci(ALLOWED_ACI.to_string()),
        timestamp: 1,
        target_sent_timestamp: 0,
        body: Some("edited".to_string()),
    };
    assert!(accepted_envelope(&env, &allowed()).is_none());
}

#[test]
fn rejects_call() {
    let env = Envelope::Call {
        source: Recipient::Aci(ALLOWED_ACI.to_string()),
        raw: vec![0; 4],
    };
    assert!(accepted_envelope(&env, &allowed()).is_none());
}

#[test]
fn rejects_unknown() {
    let env = Envelope::Unknown {
        type_tag: "future-variant".to_string(),
        raw: vec![],
    };
    assert!(accepted_envelope(&env, &allowed()).is_none());
}

// ---------------- Rate gate (mandatory) ----------------

#[test]
fn rate_gate_trips_on_threshold_plus_one() {
    let gate = NoteToSelfRateGate::new(3);
    assert!(gate.check_and_record());
    assert!(gate.check_and_record());
    assert!(gate.check_and_record());
    assert!(!gate.check_and_record(), "(N+1)-th call must trip the gate");
    assert!(gate.is_paused());
}

#[test]
fn rate_gate_stays_tripped_after_first_overflow() {
    let gate = NoteToSelfRateGate::new(1);
    assert!(gate.check_and_record());
    assert!(!gate.check_and_record());
    // Further submissions stay locked out; the gate persists until restart.
    assert!(!gate.check_and_record());
    assert!(!gate.check_and_record());
    assert!(gate.is_paused());
}

#[test]
fn rate_gate_alert_slot_fires_exactly_once() {
    // The outbound alert must be sent once per trip, not once per dropped
    // envelope - otherwise the alert path floods Note-to-Self in exactly the
    // flood the gate guards against.
    let gate = NoteToSelfRateGate::new(1);
    assert!(gate.take_alert_slot(), "first claim succeeds");
    assert!(!gate.take_alert_slot(), "second claim is denied");
    assert!(!gate.take_alert_slot(), "and stays denied");
}

#[test]
fn rate_gate_reset_helper_reopens_for_tests() {
    let gate = NoteToSelfRateGate::new(1);
    assert!(gate.check_and_record());
    assert!(!gate.check_and_record());
    gate.reset();
    assert!(!gate.is_paused());
    assert!(gate.check_and_record());
}

#[test]
fn rate_gate_does_not_affect_privacy_filter_for_peer_dms() {
    // Even though the gate is paused, accepted_envelope (the privacy filter)
    // is gate-independent: it still classifies an allowed-peer DM as
    // Some(Peer). The gate is only consulted by dispatch for SelfSync.
    let env = mk_data(Recipient::Aci(ALLOWED_ACI.to_string()), None, Some("peer"), vec![]);
    assert_eq!(
        accepted_envelope(&env, &allowed()),
        Some(AcceptedSource::Peer {
            aci: ALLOWED_ACI.to_string()
        })
    );
}

// ---------------- classify_signal_envelope cases ----------------

#[test]
fn classify_empty_envelope() {
    let outcome = classify_signal_envelope(None, &[]);
    assert!(matches!(outcome, ClassifyOutcome::Empty));
}

#[test]
fn classify_blank_body_with_no_attachments_is_empty() {
    let outcome = classify_signal_envelope(Some("   "), &[]);
    assert!(matches!(outcome, ClassifyOutcome::Empty));
}

#[test]
fn classify_url_body() {
    let outcome = classify_signal_envelope(Some("check this https://example.com/x"), &[]);
    match outcome {
        ClassifyOutcome::Single { kind, preview } => {
            assert_eq!(kind, crate::intake::Kind::Url);
            assert_eq!(preview, "https://example.com/x");
        }
        other => panic!("expected Single(Url), got {other:?}"),
    }
}

#[test]
fn classify_plain_text_body() {
    let outcome = classify_signal_envelope(Some("just words no link"), &[]);
    match outcome {
        ClassifyOutcome::Single { kind, .. } => assert_eq!(kind, crate::intake::Kind::Text),
        other => panic!("expected Single(Text), got {other:?}"),
    }
}

#[test]
fn classify_single_photo_attachment() {
    let att = mk_attachment(Some("image/jpeg"), Some("pic.jpg"), false);
    let outcome = classify_signal_envelope(None, &[att]);
    match outcome {
        ClassifyOutcome::Single { kind, .. } => assert_eq!(kind, crate::intake::Kind::Photo),
        other => panic!("expected Single(Photo), got {other:?}"),
    }
}

#[test]
fn classify_single_voice_attachment() {
    let att = mk_attachment(Some("audio/ogg"), Some("vn.ogg"), true);
    let outcome = classify_signal_envelope(None, &[att]);
    match outcome {
        ClassifyOutcome::Single { kind, .. } => assert_eq!(kind, crate::intake::Kind::Voice),
        other => panic!("expected Single(Voice), got {other:?}"),
    }
}

#[test]
fn classify_single_document_attachment() {
    let att = mk_attachment(Some("application/pdf"), Some("paper.pdf"), false);
    let outcome = classify_signal_envelope(None, &[att]);
    match outcome {
        ClassifyOutcome::Single { kind, .. } => assert_eq!(kind, crate::intake::Kind::Document),
        other => panic!("expected Single(Document), got {other:?}"),
    }
}

#[test]
fn classify_two_photos_is_partial() {
    let a = mk_attachment(Some("image/jpeg"), Some("a.jpg"), false);
    let b = mk_attachment(Some("image/png"), Some("b.png"), false);
    let outcome = classify_signal_envelope(None, &[a, b]);
    match outcome {
        ClassifyOutcome::PartialMultiAttachment {
            kind,
            dropped_count,
            dropped_summary,
            ..
        } => {
            assert_eq!(kind, crate::intake::Kind::Photo);
            assert_eq!(dropped_count, 1);
            assert_eq!(dropped_summary.len(), 1);
            assert!(dropped_summary[0].contains("b.png"));
        }
        other => panic!("expected PartialMultiAttachment, got {other:?}"),
    }
}

#[test]
fn classify_photo_plus_document_is_partial_keeping_first() {
    let a = mk_attachment(Some("image/jpeg"), Some("a.jpg"), false);
    let b = mk_attachment(Some("application/pdf"), Some("paper.pdf"), false);
    let outcome = classify_signal_envelope(None, &[a, b]);
    match outcome {
        ClassifyOutcome::PartialMultiAttachment {
            kind, dropped_count, ..
        } => {
            assert_eq!(kind, crate::intake::Kind::Photo);
            assert_eq!(dropped_count, 1);
        }
        other => panic!("expected PartialMultiAttachment, got {other:?}"),
    }
}

#[test]
fn signal_prose_and_url_captures_note() {
    // Phase 8 (signal transport capture-note fixture): the signal URL arm
    // builds its content via `router::url_content_from_text`.
    let (content, _display) =
        crate::router::url_content_from_text("this rebuts the linker post https://example.com/x").expect("url present");
    match content {
        crate::types::ContentKind::Url { url, note } => {
            assert_eq!(url, "https://example.com/x");
            assert_eq!(note.as_deref(), Some("this rebuts the linker post"));
        }
        other => panic!("expected Url, got {other:?}"),
    }
}

#[test]
fn signal_attachment_caption_migrates_to_capture_note() {
    // Phase 8: the Signal attachment caption (formerly a mangled `caption:` tag)
    // now travels as the capture note.
    assert_eq!(
        attachment_caption(Some("  a screenshot of the bug  ")),
        Some("a screenshot of the bug".to_string())
    );
    // No caption -> no note.
    assert_eq!(attachment_caption(None), None);
    assert_eq!(attachment_caption(Some("   ")), None);
}
