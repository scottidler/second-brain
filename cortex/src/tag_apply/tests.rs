use std::collections::{HashMap, HashSet};
use std::sync::Mutex;

use chrono::NaiveDate;
use distillers::tags::spend::Clock;
use distillers::tags::{TagsClassifierConfig, Transport};
use vault::canonical::TagMapping;

use super::*;

const TAG: &str = "ci-cd";

/// A classifier.dev that answers every label as Jev at 0.5, except the
/// labels given their own score, and records the inputs of every call.
struct FakeService {
    overrides: HashMap<String, f32>,
    sent: Mutex<Vec<Vec<String>>>,
}

impl FakeService {
    fn scoring(overrides: &[(&str, f32)]) -> Arc<Self> {
        Arc::new(Self {
            overrides: overrides.iter().map(|(l, s)| (l.to_string(), *s)).collect(),
            sent: Mutex::new(Vec::new()),
        })
    }

    fn sent_inputs(&self) -> Vec<String> {
        self.sent.lock().expect("lock").iter().flatten().cloned().collect()
    }

    fn call_count(&self) -> usize {
        self.sent.lock().expect("lock").len()
    }
}

impl Transport for FakeService {
    fn send(&self, body: &serde_json::Value, _token: Option<&str>) -> Result<(u16, String)> {
        let inputs: Vec<String> = body["inputs"]
            .as_array()
            .expect("inputs")
            .iter()
            .map(|v| v.as_str().expect("input").to_string())
            .collect();
        let scores: serde_json::Map<String, serde_json::Value> = body["labels"]
            .as_array()
            .expect("labels")
            .iter()
            .map(|v| {
                let label = v.as_str().expect("label").to_string();
                let score = self.overrides.get(&label).copied().unwrap_or(0.5);
                (label, serde_json::json!(score))
            })
            .collect();
        let results: Vec<serde_json::Value> = inputs
            .iter()
            .map(|_| serde_json::json!({"model": "jev-1.13.0", "scores": scores}))
            .collect();
        self.sent.lock().expect("lock").push(inputs.clone());
        let reply = serde_json::json!({
            "results": results,
            "usage": {"input_tokens": 100 * inputs.len()},
            "pricing": {"estimated_usd": 0.0001},
        });
        Ok((200, reply.to_string()))
    }
}

struct FixedClock(NaiveDate);

impl Clock for FixedClock {
    fn today_utc(&self) -> NaiveDate {
        self.0
    }
}

struct FakeScreen {
    paths: Vec<String>,
    asked: Mutex<Vec<(String, String, usize)>>,
}

impl FakeScreen {
    fn returning(paths: Vec<String>) -> Self {
        Self {
            paths,
            asked: Mutex::new(Vec::new()),
        }
    }
}

impl CandidateScreen for FakeScreen {
    fn screen(&self, query: &str, slug: &str, n: usize) -> Result<Vec<String>> {
        self.asked
            .lock()
            .expect("lock")
            .push((query.to_string(), slug.to_string(), n));
        Ok(self.paths.iter().take(n).cloned().collect())
    }
}

/// Twelve slugs, one shard: the target plus eleven others.
fn canon() -> CanonicalSet {
    let mut all: HashSet<String> = (1..=11).map(|i| format!("other-{i:02}")).collect();
    all.insert(TAG.to_string());
    CanonicalSet {
        all,
        no_segment: HashSet::new(),
        no_classifier: HashSet::new(),
        max_per_note: 8,
        descriptions: Default::default(),
    }
}

fn classifier(service: Arc<FakeService>, instructions: &str) -> ClassifierDev {
    let cfg = TagsClassifierConfig {
        repeats: 1,
        instructions: instructions.to_string(),
        ..TagsClassifierConfig::default()
    };
    ClassifierDev::with_transport(canon(), TagMapping::new(), &cfg, service)
}

struct Fixture {
    dir: tempfile::TempDir,
    notes: Vec<Note>,
}

impl Fixture {
    /// `notes/note-NNNN.md` with the given tags each, in path order.
    fn with_notes(tags: &[&[&str]]) -> Self {
        let dir = tempfile::tempdir().expect("tempdir");
        let root = dir.path().join("vault");
        std::fs::create_dir_all(root.join("notes")).expect("mkdir");
        let notes = tags
            .iter()
            .enumerate()
            .map(|(i, tags)| {
                let path = root.join(format!("notes/note-{i:04}.md"));
                std::fs::write(&path, note_text(i, tags)).expect("write");
                vault::note::parse_note(&root, &path).expect("parse")
            })
            .collect();
        Self { dir, notes }
    }

    fn untagged(n: usize) -> Self {
        Self::with_notes(&vec![&[][..]; n])
    }

    fn root(&self) -> PathBuf {
        self.dir.path().join("vault")
    }

    fn cursor_path(&self) -> PathBuf {
        TagApplyCursor::path(&self.dir.path().join("tag-apply"), TAG)
    }

    fn spend(&self) -> DaySpend {
        let cfg = TagsClassifierConfig {
            daily_usd_budget: 50.0,
            ..TagsClassifierConfig::default()
        };
        DaySpend::new(
            SpendLedger::new(self.dir.path().join("classifier-spend.jsonl")),
            Arc::new(FixedClock("2026-09-26".parse().expect("date"))),
            &cfg,
        )
    }

    fn paths(&self) -> Vec<String> {
        self.notes
            .iter()
            .map(|n| n.path.to_string_lossy().to_string())
            .collect()
    }

    fn read(&self, path: &str) -> String {
        std::fs::read_to_string(self.root().join(path)).expect("read")
    }

    fn run(
        &self,
        classifier: &ClassifierDev,
        screen: &dyn CandidateScreen,
        opts: TagApplyOpts,
    ) -> Result<TagApplyReport> {
        self.run_tag(TAG, classifier, screen, opts)
    }

    fn run_tag(
        &self,
        tag: &str,
        classifier: &ClassifierDev,
        screen: &dyn CandidateScreen,
        opts: TagApplyOpts,
    ) -> Result<TagApplyReport> {
        let canon = canon();
        let spend = self.spend();
        let cursor_path = self.cursor_path();
        let run = TagApplyRun {
            tag,
            canon: &canon,
            canonical_path: Path::new("/deployed/canonical-tags.yml"),
            classifier,
            spend: &spend,
            threshold: 0.9,
            cursor_path: &cursor_path,
            screen,
        };
        let notes: Vec<&Note> = self.notes.iter().collect();
        tag_apply(&run, &self.root(), &notes, &opts)
    }
}

fn note_text(i: usize, tags: &[&str]) -> String {
    let tags = if tags.is_empty() {
        String::new()
    } else {
        format!(
            "tags:\n{}",
            tags.iter().map(|t| format!("  - {t}\n")).collect::<String>()
        )
    };
    format!("---\ntitle: Note {i}\n{tags}---\n## Summary\n\nWe moved build number {i} to a new pipeline.\n")
}

fn no_screen() -> FakeScreen {
    FakeScreen::returning(Vec::new())
}

fn apply() -> TagApplyOpts {
    TagApplyOpts {
        apply: true,
        ..TagApplyOpts::default()
    }
}

/// Success criterion 1 (library half; the CLI half is checked against the
/// built binary): a tag the deployed vocabulary lacks is refused, naming
/// `otto deploy`, before any call.
#[test]
fn a_tag_missing_from_the_deployed_vocabulary_is_refused_naming_otto_deploy() {
    let fx = Fixture::untagged(2);
    let service = FakeService::scoring(&[]);
    let err = fx
        .run_tag(
            "not-yet-deployed",
            &classifier(service.clone(), ""),
            &no_screen(),
            apply(),
        )
        .expect_err("refused");
    let msg = format!("{err:#}");
    assert!(msg.contains("otto deploy"), "{msg}");
    assert!(msg.contains("/deployed/canonical-tags.yml"), "{msg}");
    assert_eq!(service.call_count(), 0);
    assert!(!fx.cursor_path().exists(), "no cursor for a refused tag");
}

/// Success criterion 2: a note at the cap is reported as skipped and its
/// file is byte-identical after `--apply`.
#[test]
fn a_note_at_the_cap_is_reported_and_left_byte_identical() {
    let eight = ["a", "b", "c", "d", "e", "f", "g", "h"];
    let fx = Fixture::with_notes(&[&eight, &["a"]]);
    let before = fx.read("notes/note-0000.md");
    let service = FakeService::scoring(&[(TAG, 0.95)]);

    let report = fx.run(&classifier(service, ""), &no_screen(), apply()).expect("run");

    assert_eq!(
        report.at_cap,
        vec![ScoredNote {
            path: "notes/note-0000.md".to_string(),
            score: 0.95
        }]
    );
    assert_eq!(fx.read("notes/note-0000.md"), before, "the capped note is untouched");
    assert_eq!(report.added.len(), 1);
    assert_eq!(report.added[0].path, "notes/note-0001.md");
    assert!(fx.read("notes/note-0001.md").contains("  - a\n  - ci-cd\n"));
}

/// Success criterion 3: the target scores 0.91 but eight other slugs score
/// higher, so selection would cap it off; tag-apply reads its own score and
/// adds it to a note with room.
#[test]
fn a_ninth_ranked_tag_scoring_0_91_is_added_under_the_cap() {
    let fx = Fixture::with_notes(&[&["other-01", "other-02"]]);
    let mut overrides: Vec<(String, f32)> = (1..=8).map(|i| (format!("other-{i:02}"), 0.95)).collect();
    overrides.push((TAG.to_string(), 0.91));
    let overrides: Vec<(&str, f32)> = overrides.iter().map(|(l, s)| (l.as_str(), *s)).collect();
    let service = FakeService::scoring(&overrides);

    let report = fx.run(&classifier(service, ""), &no_screen(), apply()).expect("run");

    assert_eq!(report.added.len(), 1, "{report:?}");
    assert!((report.added[0].score - 0.91).abs() < 1e-6);
    let text = fx.read("notes/note-0000.md");
    let (fm, _) = vault::frontmatter::parse_frontmatter(&text).expect("parse");
    assert_eq!(
        fm.tags.expect("tags"),
        vec!["other-01", "other-02", "ci-cd"],
        "added, nothing removed, the other high scorers not written"
    );
}

/// Success criterion 4, first half: `--all` after a `screen:300` run sends
/// only the notes outside the 300.
#[test]
fn all_after_a_screen_300_run_scores_only_the_notes_outside_the_300() {
    let fx = Fixture::untagged(360);
    let paths = fx.paths();
    let screen = FakeScreen::returning(paths.clone());

    let first = FakeService::scoring(&[]);
    let report = fx
        .run(
            &classifier(first.clone(), ""),
            &screen,
            TagApplyOpts {
                mode: Some(CandidateMode::Screen(300)),
                ..TagApplyOpts::default()
            },
        )
        .expect("screen run");
    assert_eq!(report.candidate_mode, CandidateMode::Screen(300));
    assert_eq!(report.scoring.scored, 300);
    assert_eq!(first.sent_inputs().len(), 300);
    let cursor = TagApplyCursor::load(&fx.cursor_path()).expect("load").expect("cursor");
    assert_eq!(cursor.candidate_mode, CandidateMode::Screen(300));
    assert_eq!(cursor.candidates, paths[..300].to_vec());
    assert_eq!(
        screen.asked.lock().expect("lock").clone(),
        vec![(TAG.to_string(), TAG.to_string(), 300)]
    );

    let second = FakeService::scoring(&[]);
    let report = fx
        .run(
            &classifier(second.clone(), ""),
            &screen,
            TagApplyOpts {
                mode: Some(CandidateMode::All),
                ..TagApplyOpts::default()
            },
        )
        .expect("all run");
    assert_eq!(report.candidate_mode, CandidateMode::All);
    assert!(!report.scores_discarded);
    assert_eq!(report.scoring.scored, 60);
    let sent: HashSet<String> = second.sent_inputs().into_iter().collect();
    let outside: HashSet<String> = fx.notes[300..]
        .iter()
        .map(|n| {
            distillers::tags::clip_classifier_text(crate::classify::note_title(n), &crate::classify::classifier_text(n))
        })
        .collect();
    assert_eq!(sent, outside, "only the notes outside the screen were sent");
    assert_eq!(second.sent_inputs().len(), 60);
    let cursor = TagApplyCursor::load(&fx.cursor_path()).expect("load").expect("cursor");
    assert_eq!(cursor.scores.scores.len(), 360);
}

/// Success criterion 4, second half: a changed `instructions` value
/// discards the cursor's scores, so every candidate is scored again.
#[test]
fn a_changed_instructions_value_discards_the_cursors_scores() {
    let fx = Fixture::untagged(5);
    let first = FakeService::scoring(&[]);
    fx.run(&classifier(first, ""), &no_screen(), TagApplyOpts::default())
        .expect("first");

    let same = FakeService::scoring(&[]);
    let report = fx
        .run(&classifier(same.clone(), ""), &no_screen(), TagApplyOpts::default())
        .expect("same config");
    assert!(!report.scores_discarded);
    assert_eq!(same.call_count(), 0, "the same config reuses every score");

    let changed = FakeService::scoring(&[]);
    let report = fx
        .run(
            &classifier(
                changed.clone(),
                "Apply a category only when the text is substantially about it.",
            ),
            &no_screen(),
            TagApplyOpts::default(),
        )
        .expect("changed config");
    assert!(report.scores_discarded);
    assert_eq!(report.scoring.scored, 5);
    assert_eq!(changed.sent_inputs().len(), 5);
}

/// A dry run scores and caches, writing no note; `--apply` then writes from
/// the cursor with no call at all.
#[test]
fn a_dry_run_caches_scores_and_apply_writes_them_without_rescoring() {
    let fx = Fixture::untagged(3);
    let before: Vec<String> = fx.paths().iter().map(|p| fx.read(p)).collect();
    let dry = FakeService::scoring(&[(TAG, 0.93)]);
    let report = fx
        .run(&classifier(dry.clone(), ""), &no_screen(), TagApplyOpts::default())
        .expect("dry run");
    assert!(!report.applied);
    assert_eq!(report.added.len(), 3, "reported as would-add");
    let after_dry: Vec<String> = fx.paths().iter().map(|p| fx.read(p)).collect();
    assert_eq!(after_dry, before, "a dry run writes no note");

    // A service that would score below the threshold: never asked.
    let wet = FakeService::scoring(&[(TAG, 0.1)]);
    let report = fx
        .run(&classifier(wet.clone(), ""), &no_screen(), apply())
        .expect("apply");
    assert_eq!(wet.call_count(), 0);
    assert_eq!(report.added.len(), 3);
    for path in fx.paths() {
        assert!(fx.read(&path).contains("  - ci-cd\n"), "{path}");
    }
}

/// The union is taken against the file as it is at write time: a tag added
/// by another writer after the scan is kept, not overwritten.
#[test]
fn a_concurrent_edit_is_merged_at_write_time() {
    let fx = Fixture::with_notes(&[&["rust"]]);
    let path = fx.root().join("notes/note-0000.md");
    std::fs::write(&path, note_text(0, &["rust", "homelab"])).expect("edit after scan");

    let service = FakeService::scoring(&[(TAG, 0.97)]);
    fx.run(&classifier(service, ""), &no_screen(), apply()).expect("run");

    let (fm, _) = vault::frontmatter::parse_frontmatter(&fx.read("notes/note-0000.md")).expect("parse");
    assert_eq!(fm.tags.expect("tags"), vec!["rust", "homelab", "ci-cd"]);
}

/// Below the threshold, already tagged, and failed notes are all left alone;
/// a failed note is listed.
#[test]
fn below_threshold_already_tagged_and_failed_notes_are_not_changed() {
    let fx = Fixture::with_notes(&[&[], &[TAG]]);
    let before: Vec<String> = fx.paths().iter().map(|p| fx.read(p)).collect();
    let low = FakeService::scoring(&[(TAG, 0.89)]);
    let report = fx.run(&classifier(low, ""), &no_screen(), apply()).expect("run");
    assert_eq!(report.below_threshold, 2);
    assert!(report.added.is_empty());

    struct Failing;
    impl Transport for Failing {
        fn send(&self, _body: &serde_json::Value, _token: Option<&str>) -> Result<(u16, String)> {
            Ok((500, r#"{"error":"upstream"}"#.to_string()))
        }
    }
    let cfg = TagsClassifierConfig {
        repeats: 1,
        instructions: "a new config, so every score is rescored".to_string(),
        ..TagsClassifierConfig::default()
    };
    let failing = ClassifierDev::with_transport(canon(), TagMapping::new(), &cfg, Arc::new(Failing));
    let report = fx.run(&failing, &no_screen(), apply()).expect("run");
    assert_eq!(report.failed.len(), 2, "{report:?}");
    assert!(
        report.failed.iter().all(|(_, e)| e.contains("500")),
        "{:?}",
        report.failed
    );
    let after: Vec<String> = fx.paths().iter().map(|p| fx.read(p)).collect();
    assert_eq!(after, before);

    let high = FakeService::scoring(&[(TAG, 0.99)]);
    let report = fx.run(&classifier(high, ""), &no_screen(), apply()).expect("run");
    assert_eq!(report.already_tagged, 1);
    assert_eq!(report.added.len(), 1);
}

/// A narrower request keeps the wider list, and a bare rerun resumes the
/// cursor's own mode without screening again.
#[test]
fn narrowing_keeps_the_wider_list_and_a_bare_rerun_resumes_the_stored_mode() {
    let fx = Fixture::untagged(10);
    let screen = FakeScreen::returning(fx.paths());
    let service = FakeService::scoring(&[]);
    let dev = classifier(service, "");
    let screened = |n| TagApplyOpts {
        mode: Some(CandidateMode::Screen(n)),
        ..TagApplyOpts::default()
    };
    fx.run(&dev, &screen, screened(6)).expect("six");
    let report = fx.run(&dev, &screen, screened(3)).expect("three");
    assert_eq!(report.candidate_mode, CandidateMode::Screen(6));
    assert_eq!(report.candidates, 6);
    let report = fx.run(&dev, &screen, TagApplyOpts::default()).expect("bare");
    assert_eq!(report.candidate_mode, CandidateMode::Screen(6));
    assert_eq!(
        screen.asked.lock().expect("lock").len(),
        1,
        "screened only for the first run"
    );
    let report = fx.run(&dev, &screen, screened(8)).expect("eight");
    assert_eq!(report.candidate_mode, CandidateMode::Screen(8));
    assert_eq!(report.candidates, 8);
    assert_eq!(report.scoring.scored, 2, "widening scores only the new candidates");
}

/// A candidate that is no longer an eligible note is counted, not scored.
#[test]
fn a_candidate_that_left_the_vault_is_counted_missing() {
    let fx = Fixture::untagged(3);
    let mut paths = fx.paths();
    paths.push("notes/deleted.md".to_string());
    let screen = FakeScreen::returning(paths);
    let service = FakeService::scoring(&[]);
    let report = fx
        .run(
            &classifier(service, ""),
            &screen,
            TagApplyOpts {
                mode: Some(CandidateMode::Screen(10)),
                ..TagApplyOpts::default()
            },
        )
        .expect("run");
    assert_eq!(report.candidates, 4);
    assert_eq!(report.missing, 1);
    assert_eq!(report.scoring.scored, 3);
}

#[test]
fn a_cursor_for_another_tag_is_refused() {
    let fx = Fixture::untagged(1);
    TagApplyCursor {
        tag: "other-01".to_string(),
        candidate_mode: CandidateMode::All,
        candidates: Vec::new(),
        scores: ScoreCursor::default(),
    }
    .save(&fx.cursor_path())
    .expect("save");
    let err = fx
        .run(&classifier(FakeService::scoring(&[]), ""), &no_screen(), apply())
        .expect_err("refused");
    assert!(format!("{err:#}").contains("not \"ci-cd\""), "{err:#}");
}

#[test]
fn candidate_mode_parses_prints_and_widens() {
    for (text, mode) in [("all", CandidateMode::All), ("screen:300", CandidateMode::Screen(300))] {
        assert_eq!(CandidateMode::try_from(text.to_string()), Ok(mode));
        assert_eq!(mode.to_string(), text);
    }
    for bad in ["screen:0", "screen:x", "some", ""] {
        assert!(CandidateMode::try_from(bad.to_string()).is_err(), "{bad}");
    }
    let (s300, s600) = (CandidateMode::Screen(300), CandidateMode::Screen(600));
    assert_eq!(s300.widest(s600), s600);
    assert_eq!(s600.widest(s300), s600);
    assert_eq!(s600.widest(CandidateMode::All), CandidateMode::All);
    assert_eq!(CandidateMode::All.widest(s300), CandidateMode::All);
}

/// The cursor file carries the Data Model's fields at the top level.
#[test]
fn the_cursor_file_reads_as_one_object() {
    let fx = Fixture::untagged(1);
    fx.run(
        &classifier(FakeService::scoring(&[(TAG, 0.4)]), ""),
        &no_screen(),
        TagApplyOpts::default(),
    )
    .expect("run");
    let json: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(fx.cursor_path()).expect("read")).expect("json");
    for key in [
        "tag",
        "scoring_config_hash",
        "candidate_mode",
        "candidates",
        "scores",
        "failed",
    ] {
        assert!(json.get(key).is_some(), "missing {key}: {json}");
    }
    assert_eq!(json["candidate_mode"], "all");
    let score = &json["scores"]["notes/note-0000.md"];
    assert!(
        (score["scores"][TAG].as_f64().expect("score") - 0.4).abs() < 1e-6,
        "{score}"
    );
    assert!(score["wire_text_hash"].is_string());
}

#[test]
fn plan_add_handles_missing_tags_and_no_frontmatter() {
    assert!(matches!(
        plan_add("---\ntitle: T\n---\nbody\n", TAG, 8).expect("plan"),
        AddPlan::Add(text) if text.contains("tags:\n  - ci-cd")
    ));
    assert!(plan_add("no frontmatter here\n", TAG, 8).is_err());
    assert_eq!(
        plan_add("---\ntags:\n  - ci-cd\n---\n", TAG, 8).expect("plan"),
        AddPlan::AlreadyTagged
    );
}

#[test]
fn screen_query_uses_the_description_when_there_is_one() {
    let mut canon = canon();
    assert_eq!(screen_query(TAG, &canon), TAG);
    canon
        .descriptions
        .insert(TAG.to_string(), "build and deploy pipelines".to_string());
    assert_eq!(screen_query(TAG, &canon), "ci-cd: build and deploy pipelines");
}

#[test]
fn eligible_notes_are_governed_notes_under_notes_not_exempt_from_tags() {
    let note = |path: &str| Note {
        path: PathBuf::from(path),
        frontmatter: Default::default(),
        body: String::new(),
        raw: String::new(),
    };
    let notes = vec![
        note("notes/keep.md"),
        note("notes/ai/digest.md"),
        note("notes/excluded.md"),
        note("inbox/new.md"),
        note("system/hub.md"),
    ];
    let mut config = Config::default();
    config.vault.exclude = vec!["notes/excluded.md".to_string()];
    config
        .actions
        .frontmatter
        .path_exempt
        .insert("notes/ai/**".to_string(), vec!["tags".to_string()]);

    let paths: Vec<String> = eligible_notes(&notes, &config)
        .iter()
        .map(|n| n.path.to_string_lossy().to_string())
        .collect();
    assert_eq!(paths, vec!["notes/keep.md"]);
}
