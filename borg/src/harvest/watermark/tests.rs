#![allow(clippy::unwrap_used)]
use super::*;
use tempfile::TempDir;

fn entry(n_msgs: i64, hash: &str) -> PublishedEntry {
    PublishedEntry {
        note_path: "inbox/note.md".to_string(),
        n_msgs,
        body_hash: hash.to_string(),
        trace: Some("hv-test01".to_string()),
    }
}

#[test]
fn load_absent_is_default_not_error() {
    let dir = TempDir::new().unwrap();
    let state = WatermarkState::load(&dir.path().join("harvest-state.json")).unwrap();
    assert_eq!(state, WatermarkState::default());
    assert_eq!(state.cursor, None);
}

#[test]
fn save_then_load_round_trips() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("harvest-state.json");
    let mut state = WatermarkState {
        cursor: Some(1500),
        ..Default::default()
    };
    state.published.insert("s1".to_string(), entry(486, "deadbeef"));
    state.save(&path).unwrap();

    let reloaded = WatermarkState::load(&path).unwrap();
    assert_eq!(reloaded, state);
}

#[test]
fn save_uses_atomic_write_with_no_leftover_temp_files() {
    // Phase 2: `save` now goes through `vault::note::write_atomic` (fsync temp
    // + parent, then rename into place) instead of the prior unfsynced
    // `fs::write` + `fs::rename`. Same observable proof `vault::note::tests`
    // uses for `write_atomic` itself: no temp file of EITHER naming scheme
    // (the new `.sb-tmp-*` prefix, or the old `<name>.json.tmp` suffix)
    // survives a save, and the content round-trips.
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("harvest-state.json");
    let mut state = WatermarkState {
        cursor: Some(99),
        ..Default::default()
    };
    state.published.insert("s1".to_string(), entry(10, "abc"));
    state.save(&path).unwrap();

    let leftovers: Vec<_> = std::fs::read_dir(dir.path())
        .unwrap()
        .filter_map(|e| e.ok())
        .filter(|e| e.file_name() != std::ffi::OsStr::new("harvest-state.json"))
        .collect();
    assert!(
        leftovers.is_empty(),
        "atomic write left a temp file behind: {leftovers:?}"
    );

    let reloaded = WatermarkState::load(&path).unwrap();
    assert_eq!(reloaded, state, "content durably round-trips through the atomic write");
}

#[test]
fn published_entry_trace_defaults_to_none_for_legacy_state_without_the_key() {
    // Existing on-disk state files predate `PublishedEntry.trace` and carry no
    // `trace` key at all - deserialization must keep working, reading back as
    // `None` rather than erroring.
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("harvest-state.json");
    let legacy = br#"{
  "cursor": 1500,
  "published": {
    "s1": {
      "note-path": "inbox/2dda6936.md",
      "n-msgs": 486,
      "body-hash": "deadbeef"
    }
  }
}"#;
    std::fs::write(&path, legacy).unwrap();

    let state = WatermarkState::load(&path).unwrap();
    assert_eq!(
        state.published["s1"].trace, None,
        "legacy state with no trace key must still deserialize"
    );
    assert_eq!(state.published["s1"].n_msgs, 486);
}

#[test]
fn corrupt_state_is_a_loud_error() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("harvest-state.json");
    std::fs::write(&path, b"{ not valid json").unwrap();
    let err = WatermarkState::load(&path).unwrap_err();
    assert!(format!("{err:#}").contains("corrupt"), "{err:#}");
}

#[test]
fn exclusive_lock_blocks_a_second_holder() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("harvest-state.json");
    let _held = acquire_lock(&path).expect("first lock");
    let err = acquire_lock(&path).expect_err("second lock must fail loudly");
    assert!(format!("{err:#}").contains("lock held"), "{err:#}");
}

#[test]
fn lock_releases_on_drop() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("harvest-state.json");
    {
        let _held = acquire_lock(&path).expect("first lock");
    }
    // Dropped -> a new acquisition succeeds.
    let _again = acquire_lock(&path).expect("lock after release");
}

#[test]
fn body_hash_is_stable_and_content_sensitive() {
    let a = vec![BodyMessage {
        role: Some("user".into()),
        text: Some("hello".into()),
        subagent: false,
    }];
    let b = vec![BodyMessage {
        role: Some("user".into()),
        text: Some("hello world".into()),
        subagent: false,
    }];
    let ha = body_hash(&canonical_body_text(&a));
    let hb = body_hash(&canonical_body_text(&b));
    assert_eq!(ha, body_hash(&canonical_body_text(&a)));
    assert_ne!(ha, hb);
}

#[test]
fn subagent_flag_changes_the_hash() {
    let plain = vec![BodyMessage {
        role: Some("assistant".into()),
        text: Some("x".into()),
        subagent: false,
    }];
    let sub = vec![BodyMessage {
        role: Some("assistant".into()),
        text: Some("x".into()),
        subagent: true,
    }];
    assert_ne!(
        body_hash(&canonical_body_text(&plain)),
        body_hash(&canonical_body_text(&sub))
    );
}

#[test]
fn thread_body_distinguishes_member_splits() {
    let m = vec![BodyMessage {
        role: Some("user".into()),
        text: Some("one".into()),
        subagent: false,
    }];
    let n = vec![BodyMessage {
        role: Some("user".into()),
        text: Some("two".into()),
        subagent: false,
    }];
    // [a: one][b: two] must not hash like [a: one two] (same concatenated text,
    // different member structure).
    let split = thread_body_text(&[("a".into(), m.clone()), ("b".into(), n.clone())]);
    let merged = thread_body_text(&[(
        "a".into(),
        vec![
            BodyMessage {
                role: Some("user".into()),
                text: Some("one".into()),
                subagent: false,
            },
            BodyMessage {
                role: Some("user".into()),
                text: Some("two".into()),
                subagent: false,
            },
        ],
    )]);
    assert_ne!(body_hash(&split), body_hash(&merged));
}

#[test]
fn needs_body_fetch_only_on_changed_published_nonforce() {
    let e = entry(70, "h1");
    assert!(!needs_body_fetch(None, 70, false), "never-published -> no fetch");
    assert!(
        !needs_body_fetch(Some(&e), 70, false),
        "unchanged n-msgs -> cheap skip, no fetch"
    );
    assert!(
        needs_body_fetch(Some(&e), 90, false),
        "changed n-msgs -> deep check fetch"
    );
    assert!(
        !needs_body_fetch(Some(&e), 90, true),
        "force -> no fetch needed (redistill regardless)"
    );
}

#[test]
fn classify_new_note_when_unpublished() {
    assert_eq!(classify_reappearance(None, 40, None, false), Reappearance::NewNote);
}

#[test]
fn classify_force_is_follow_up() {
    let e = entry(70, "h1");
    assert_eq!(
        classify_reappearance(Some(&e), 70, None, true),
        Reappearance::FollowUp { prior: e.clone() }
    );
}

#[test]
fn classify_cheap_skip_when_unchanged() {
    let e = entry(70, "h1");
    // No fresh hash (cheap filter matched) -> Skip, no snapshot change.
    assert_eq!(
        classify_reappearance(Some(&e), 70, None, false),
        Reappearance::Skip { snapshot_update: None }
    );
}

#[test]
fn classify_follow_up_when_hash_changed() {
    let e = entry(70, "h1");
    assert_eq!(
        classify_reappearance(Some(&e), 90, Some("h2"), false),
        Reappearance::FollowUp { prior: e.clone() }
    );
}

#[test]
fn classify_skip_advances_snapshot_when_hash_unchanged_but_msgs_grew() {
    let e = entry(70, "h1");
    let decision = classify_reappearance(Some(&e), 90, Some("h1"), false);
    match decision {
        Reappearance::Skip {
            snapshot_update: Some(updated),
        } => {
            assert_eq!(updated.n_msgs, 90, "snapshot advances so it never re-checks");
            assert_eq!(updated.body_hash, "h1");
            assert_eq!(
                updated.note_path, "inbox/note.md",
                "note path unchanged (notes are immutable)"
            );
            assert_eq!(
                updated.trace, e.trace,
                "trace carries forward unchanged - this is not a new publish"
            );
        }
        other => panic!("expected Skip with snapshot advance, got {other:?}"),
    }
}

// ---- harvest-completion Phase 6: gap-fill. `BodyMessage.role`/`text` are the
// one defensive Option Phase 0/1 never exercised through the identity-hash
// path - a body element with a null role/text (a future-malformed element on
// the `--with-body` path) must degrade to an empty string in the canonical
// body render, never panic the hash.
#[test]
fn canonical_body_text_degrades_gracefully_on_null_role_and_text() {
    let messages = vec![
        BodyMessage {
            role: Some("user".into()),
            text: Some("hello".into()),
            subagent: false,
        },
        BodyMessage {
            role: None,
            text: None,
            subagent: false,
        },
    ];
    let text = canonical_body_text(&messages);
    assert_eq!(
        text, "user: hello\n: \n",
        "a null role/text degrades to an empty string, not a panic"
    );
    // Stable + hashable like any other body text.
    assert_eq!(body_hash(&text), body_hash(&canonical_body_text(&messages)));
}
