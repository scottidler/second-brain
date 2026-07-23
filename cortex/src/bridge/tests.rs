use super::*;
use crate::vault::Note;

/// Deterministic offline detector: returns a fixed repo set per session id (no
/// LLM). `None` for an id it doesn't know → an empty detection (no bridge).
struct MockDetector {
    by_session: std::collections::HashMap<String, Vec<String>>,
}

impl MockDetector {
    fn new(pairs: &[(&str, &[&str])]) -> Self {
        Self {
            by_session: pairs
                .iter()
                .map(|(id, repos)| (id.to_string(), repos.iter().map(|r| r.to_string()).collect()))
                .collect(),
        }
    }
}

impl BridgeDetector for MockDetector {
    fn detect(&self, session_id: &str, _transcript: &str) -> Result<Vec<String>> {
        Ok(self.by_session.get(session_id).cloned().unwrap_or_default())
    }
}

/// A detector that ALWAYS fails — the forced-failure boundary. Offline: it never
/// reaches fabric, it just returns `Err`.
struct FailingDetector;

impl BridgeDetector for FailingDetector {
    fn detect(&self, session_id: &str, _transcript: &str) -> Result<Vec<String>> {
        eyre::bail!("simulated LLM failure for session {session_id}")
    }
}

fn session(id: &str, note_path: &str, primary_repo: &str) -> BackfillSession {
    BackfillSession {
        session_id: id.to_string(),
        note_path: note_path.to_string(),
        primary_repo: primary_repo.to_string(),
        transcript: format!("transcript body for {id}"),
    }
}

fn harvest_note(path: &str, source: Option<&str>, repo: Option<&str>, repos_touched: Option<Vec<String>>) -> Note {
    let fm = vault::frontmatter::Frontmatter {
        source: source.map(|s| s.to_string()),
        repo: repo.map(|s| s.to_string()),
        repos_touched,
        ..Default::default()
    };
    Note {
        path: std::path::PathBuf::from(path),
        frontmatter: fm,
        body: String::new(),
        raw: String::new(),
    }
}

// ---------------------------------------------------------------------------
// The three biting tests the design doc / task require.
// ---------------------------------------------------------------------------

#[test]
fn forced_llm_failure_yields_zero_proposals_and_a_visible_error() {
    // Two real candidate sessions; the detector fails on the first. Fail-closed:
    // the whole pass must return Err with ZERO proposals — never a partial set.
    let sessions = vec![
        session("s1", "notes/a.md", "scottidler/loopr"),
        session("s2", "notes/b.md", "scottidler/otto"),
    ];
    let result = backfill(&sessions, &FailingDetector);
    assert!(
        result.is_err(),
        "a detector failure must surface as an Err, not silent partial output"
    );
    let msg = format!("{:#}", result.unwrap_err());
    assert!(
        msg.contains("simulated LLM failure"),
        "the error must be visible/traceable, got: {msg}"
    );
}

#[test]
fn every_proposal_carries_session_provenance() {
    // s1's session touched a secondary repo (tatari-tv/okta-auth-rs) beyond its
    // note's primary (scottidler/loopr) → one bridge proposal, provenance = s1.
    let sessions = vec![session("s1", "notes/a.md", "scottidler/loopr")];
    let detector = MockDetector::new(&[("s1", &["scottidler/loopr", "tatari-tv/okta-auth-rs"])]);

    let proposals = backfill(&sessions, &detector).expect("backfill");
    assert_eq!(proposals.len(), 1, "one secondary repo → one bridge proposal");
    let p = &proposals[0];
    assert_eq!(p.member, "notes/a.md");
    assert_eq!(p.repo, "tatari-tv/okta-auth-rs");
    assert_eq!(p.hub_path, repo_hub_path("tatari-tv/okta-auth-rs"));
    assert_eq!(
        p.sessions,
        vec!["s1".to_string()],
        "proposal traces to its driving session"
    );
    assert!(!p.sessions.is_empty(), "provenance is never empty");
}

#[test]
fn apply_never_mutates_the_landed_member_note() {
    let dir = tempfile::tempdir().expect("tmp");
    let vault_root = dir.path();

    // A landed member note with known bytes.
    let member_rel = "notes/review-okta.md";
    let member_abs = vault_root.join(member_rel);
    std::fs::create_dir_all(member_abs.parent().unwrap()).unwrap();
    let member_bytes = "---\ntitle: review\nrepo: scottidler/loopr\n---\n\n# review\n\nbody untouched\n";
    std::fs::write(&member_abs, member_bytes).unwrap();

    // The secondary repo hub the bridge targets (must exist before apply).
    let secondary = "tatari-tv/okta-auth-rs";
    let hub_rel = repo_hub_path(secondary);
    let hub_abs = vault_root.join(&hub_rel);
    std::fs::create_dir_all(hub_abs.parent().unwrap()).unwrap();
    std::fs::write(
        &hub_abs,
        "---\ntitle: tatari-tv/okta-auth-rs\ntype: entity\n---\n\n# tatari-tv/okta-auth-rs\n\nHub.\n",
    )
    .unwrap();

    // Produce + persist a proposal from a real backfill run.
    let sessions = vec![session("s1", member_rel, "scottidler/loopr")];
    let detector = MockDetector::new(&[("s1", &["scottidler/loopr", secondary])]);
    let proposals = backfill(&sessions, &detector).expect("backfill");
    let proposals_path = vault_root.join("bridge-proposals.yml");
    write_bridge_proposals(&proposals_path, proposals).expect("write");

    // Dry-run then apply.
    let report = apply_bridge(&proposals_path, vault_root, member_rel, secondary, false).expect("dry-run");
    assert!(!report.applied && !report.already_present);
    assert!(report.diff.contains("s1"), "diff shows provenance");

    let report = apply_bridge(&proposals_path, vault_root, member_rel, secondary, true).expect("apply");
    assert!(report.applied);

    // The landed note is byte-for-byte unchanged; only the HUB body grew.
    let after = std::fs::read_to_string(&member_abs).unwrap();
    assert_eq!(after, member_bytes, "apply must NEVER modify a landed member note");
    let hub_after = std::fs::read_to_string(&hub_abs).unwrap();
    assert!(
        hub_after.contains(&member_wikilink(member_rel)),
        "the hub body gains the member wikilink"
    );
    // The applied proposal is dropped.
    let pf: BridgeProposalsFile = serde_yaml::from_str(&std::fs::read_to_string(&proposals_path).unwrap()).unwrap();
    assert!(
        pf.proposals.is_empty(),
        "applied proposal dropped from bridge-proposals.yml"
    );
}

// ---------------------------------------------------------------------------
// Supporting coverage.
// ---------------------------------------------------------------------------

#[test]
fn backfill_drops_the_primary_repo_and_is_deterministic() {
    // Detected set includes the primary repo (which needs no bridge) and two
    // secondaries; only the two secondaries become proposals, deterministically
    // ordered by hub path.
    let sessions = vec![session("s1", "notes/a.md", "scottidler/loopr")];
    let detector = MockDetector::new(&[(
        "s1",
        &["scottidler/loopr", "tatari-tv/okta-auth-rs", "scottidler/scaffold"],
    )]);
    let proposals = backfill(&sessions, &detector).expect("backfill");
    let repos: Vec<&str> = proposals.iter().map(|p| p.repo.as_str()).collect();
    assert!(!repos.contains(&"scottidler/loopr"), "primary repo is not re-bridged");
    assert_eq!(repos.len(), 2);
    // Deterministic: keyed on hub_path, so order is stable across runs.
    let run2 = backfill(&sessions, &detector).expect("backfill2");
    assert_eq!(proposals, run2, "backfill output is deterministic");
}

#[test]
fn backfill_ignores_non_canonical_detected_repos() {
    let sessions = vec![session("s1", "notes/a.md", "scottidler/loopr")];
    // "not-a-repo" (no `/`) and "" are non-canonical → ignored; the one valid
    // secondary survives.
    let detector = MockDetector::new(&[("s1", &["not-a-repo", "", "tatari-tv/okta-auth-rs"])]);
    let proposals = backfill(&sessions, &detector).expect("backfill");
    assert_eq!(proposals.len(), 1);
    assert_eq!(proposals[0].repo, "tatari-tv/okta-auth-rs");
}

#[test]
fn backfill_accumulates_provenance_across_sessions_of_the_same_note() {
    // Two sessions of the SAME landed note both touch the same secondary repo →
    // ONE proposal carrying BOTH session ids as provenance.
    let sessions = vec![
        session("s1", "notes/a.md", "scottidler/loopr"),
        session("s2", "notes/a.md", "scottidler/loopr"),
    ];
    let detector = MockDetector::new(&[("s1", &["tatari-tv/okta-auth-rs"]), ("s2", &["tatari-tv/okta-auth-rs"])]);
    let proposals = backfill(&sessions, &detector).expect("backfill");
    assert_eq!(proposals.len(), 1, "same (hub, member) merges to one proposal");
    assert_eq!(proposals[0].sessions, vec!["s1".to_string(), "s2".to_string()]);
}

#[test]
fn candidate_members_selects_pre_files_touched_harvest_notes_only() {
    let notes = vec![
        // Eligible: clyde source, valid repo, no repos-touched.
        harvest_note("notes/a.md", Some("clyde://s1"), Some("scottidler/loopr"), None),
        // Skipped: has repos-touched (Phase 4 owns it).
        harvest_note(
            "notes/b.md",
            Some("clyde://s2"),
            Some("scottidler/otto"),
            Some(vec!["x/y".into()]),
        ),
        // Skipped: not a clyde/harvest note.
        harvest_note("notes/c.md", Some("https://example.com"), Some("scottidler/x"), None),
        // Skipped: no repo.
        harvest_note("notes/d.md", Some("clyde://s4"), None, None),
        // Skipped: non-canonical repo.
        harvest_note("notes/e.md", Some("clyde://s5"), Some("not-a-repo"), None),
    ];
    let candidates = candidate_members(&notes);
    assert_eq!(candidates.len(), 1);
    assert_eq!(candidates[0].session_id, "s1");
    assert_eq!(candidates[0].note_path, "notes/a.md");
    assert_eq!(candidates[0].primary_repo, "scottidler/loopr");
}

#[test]
fn apply_requires_a_pending_proposal() {
    let dir = tempfile::tempdir().expect("tmp");
    let proposals_path = dir.path().join("bridge-proposals.yml");
    std::fs::write(&proposals_path, "proposals: []\n").unwrap();
    // No proposal exists → an apply must error (traceability).
    assert!(apply_bridge(&proposals_path, dir.path(), "notes/a.md", "x/y", false).is_err());
}

#[test]
fn apply_errors_when_the_hub_note_is_missing() {
    let dir = tempfile::tempdir().expect("tmp");
    let vault_root = dir.path();
    let sessions = vec![session("s1", "notes/a.md", "scottidler/loopr")];
    let detector = MockDetector::new(&[("s1", &["tatari-tv/okta-auth-rs"])]);
    let proposals = backfill(&sessions, &detector).expect("backfill");
    let proposals_path = vault_root.join("bridge-proposals.yml");
    write_bridge_proposals(&proposals_path, proposals).expect("write");
    // Hub note was never minted → apply errors loudly.
    let err = apply_bridge(
        &proposals_path,
        vault_root,
        "notes/a.md",
        "tatari-tv/okta-auth-rs",
        true,
    )
    .unwrap_err();
    assert!(format!("{err:#}").contains("does not exist"));
}

#[test]
fn apply_is_idempotent_when_already_bridged() {
    let dir = tempfile::tempdir().expect("tmp");
    let vault_root = dir.path();
    let secondary = "tatari-tv/okta-auth-rs";
    let hub_rel = repo_hub_path(secondary);
    let hub_abs = vault_root.join(&hub_rel);
    std::fs::create_dir_all(hub_abs.parent().unwrap()).unwrap();
    let link = member_wikilink("notes/a.md");
    std::fs::write(
        &hub_abs,
        format!("---\ntitle: h\n---\n\n# h\n\n{BRIDGED_SECTION}\n\n- {link}\n"),
    )
    .unwrap();

    let sessions = vec![session("s1", "notes/a.md", "scottidler/loopr")];
    let detector = MockDetector::new(&[("s1", &[secondary])]);
    let proposals = backfill(&sessions, &detector).expect("backfill");
    let proposals_path = vault_root.join("bridge-proposals.yml");
    write_bridge_proposals(&proposals_path, proposals).expect("write");

    let report = apply_bridge(&proposals_path, vault_root, "notes/a.md", secondary, true).expect("apply");
    assert!(report.already_present, "an already-linked hub is a no-op");
    assert!(!report.applied);
}

#[test]
fn write_bridge_proposals_merges_and_unions_provenance() {
    let dir = tempfile::tempdir().expect("tmp");
    let path = dir.path().join("bridge-proposals.yml");

    let hub = repo_hub_path("tatari-tv/okta-auth-rs");
    write_bridge_proposals(
        &path,
        vec![BridgeProposal {
            member: "notes/a.md".into(),
            repo: "tatari-tv/okta-auth-rs".into(),
            hub_path: hub.clone(),
            wikilink: member_wikilink("notes/a.md"),
            sessions: vec!["s1".into()],
        }],
    )
    .expect("write1");

    // Re-run: same (hub, member) with new provenance + a brand new bridge.
    write_bridge_proposals(
        &path,
        vec![
            BridgeProposal {
                member: "notes/a.md".into(),
                repo: "tatari-tv/okta-auth-rs".into(),
                hub_path: hub.clone(),
                wikilink: member_wikilink("notes/a.md"),
                sessions: vec!["s9".into()],
            },
            BridgeProposal {
                member: "notes/b.md".into(),
                repo: "scottidler/otto".into(),
                hub_path: repo_hub_path("scottidler/otto"),
                wikilink: member_wikilink("notes/b.md"),
                sessions: vec!["s2".into()],
            },
        ],
    )
    .expect("write2");

    let pf: BridgeProposalsFile = serde_yaml::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
    assert_eq!(pf.proposals.len(), 2, "existing bridge kept, new one appended");
    let a = pf.proposals.iter().find(|p| p.member == "notes/a.md").unwrap();
    assert_eq!(
        a.sessions,
        vec!["s1".to_string(), "s9".to_string()],
        "provenance unioned, not clobbered"
    );
}

#[test]
fn proposals_file_serde_roundtrips_with_kebab_keys() {
    let file = BridgeProposalsFile {
        proposals: vec![BridgeProposal {
            member: "notes/a.md".into(),
            repo: "tatari-tv/okta-auth-rs".into(),
            hub_path: repo_hub_path("tatari-tv/okta-auth-rs"),
            wikilink: member_wikilink("notes/a.md"),
            sessions: vec!["s1".into()],
        }],
    };
    let yaml = serde_yaml::to_string(&file).expect("ser");
    assert!(yaml.contains("hub-path:"), "kebab-case key");
    let back: BridgeProposalsFile = serde_yaml::from_str(&yaml).expect("de");
    assert_eq!(back.proposals, file.proposals);
}
