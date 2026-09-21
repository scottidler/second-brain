use super::*;

/// A vocabulary snapshot for the union tests: `max` is `max-per-note`,
/// `protected` stands in for `no-classifier-tags`. The tag sets the union
/// path never reads (`all`, `no_segment`) stay empty.
fn canon(max: usize, protected: &[&str]) -> vault::canonical::CanonicalSet {
    vault::canonical::CanonicalSet {
        all: std::collections::HashSet::new(),
        no_segment: std::collections::HashSet::new(),
        no_classifier: protected.iter().map(|t| (*t).to_string()).collect(),
        max_per_note: max,
    }
}

/// Builds a `Config` whose canonical-tag vocabulary and mapping live in a
/// throwaway tempdir, so the test does not depend on (or mutate) the real
/// `~/.config/sb/` catalogue.
///
/// Canonical vocabulary: `rust`, `llm`. Mapping: `rustlang` -> `rust`
/// (collapses a near-miss into canonical), `javascript` -> null (explicit
/// rejection, distinct from "no match at all").
fn config_with_fixture_canonical(dir: &std::path::Path) -> Config {
    let canonical_path = dir.join("canonical-tags.yml");
    let mapping_path = dir.join("tag-mapping.yml");

    std::fs::write(
        &canonical_path,
        "max-per-note: 7\ntags:\n  ai:\n    - llm\n  systems:\n    - rust\n",
    )
    .expect("write fixture canonical-tags.yml");
    std::fs::write(&mapping_path, "rustlang: rust\njavascript: null\n").expect("write fixture tag-mapping.yml");

    Config {
        tags: crate::config::TagsConfig {
            canonical_path: canonical_path.display().to_string(),
            mapping_path: mapping_path.display().to_string(),
            reject_concatenated: true,
            // Deterministic (the field's own default): a pure canonical
            // mapping over whatever candidates the test builds, with no
            // network call and no dependence on candidate source.
            classifier: distillers::tags::TagsClassifierConfig::default(),
        },
        ..Config::default()
    }
}

/// Phase 2 success criterion, carried into Phase 6's `TagSources`/`TagOutcome`
/// seam: a distiller that proposes candidate tags (the new pattern
/// instruction) has those tags reach the note through the exact
/// candidates-then-classify code path `pipeline.rs` runs at publish time -
/// canonical tags survive, non-canonical/rejected ones are dropped, and a
/// mapped near-miss collapses onto its canonical target. `Deterministic`
/// (the config default here) does not discriminate by candidate source, so
/// routing the "other sources" through `author` and the distiller output
/// through `model` changes nothing about which tags survive.
#[tokio::test]
async fn distiller_proposed_tags_survive_canonical_filter() {
    let dir = tempfile::tempdir().expect("tempdir");
    let config = config_with_fixture_canonical(dir.path());

    let mut sources = TagSources::from_summary("title", "text");
    sources
        .author
        .extend(["llm".to_string()].iter().map(|t| hygiene::sanitize_tag(t)));
    sources.model.extend(
        [
            "rust".to_string(),             // exact canonical hit
            "made-up-nonsense".to_string(), // no canonical match anywhere
            "RustLang".to_string(),         // mapped near-miss -> collapses to "rust"
            "javascript".to_string(),       // explicitly rejected by the mapping
        ]
        .iter()
        .map(|t| hygiene::sanitize_tag(t)),
    );

    let outcome = finalize_tags(sources, &config).await;
    let all_tags = outcome.tags;

    assert!(
        all_tags.contains(&"llm".to_string()),
        "canonical tag from other sources must survive: {all_tags:?}"
    );
    assert!(
        all_tags.contains(&"rust".to_string()),
        "canonical distiller-proposed tag must survive: {all_tags:?}"
    );
    assert!(
        !all_tags.contains(&"made-up-nonsense".to_string()),
        "non-canonical distiller-proposed tag must be dropped: {all_tags:?}"
    );
    assert!(
        !all_tags.contains(&"javascript".to_string()),
        "mapping-rejected tag must be dropped: {all_tags:?}"
    );
    assert!(
        !all_tags.contains(&"rustlang".to_string()),
        "mapped near-miss must collapse onto its canonical target, not survive raw: {all_tags:?}"
    );
    assert_eq!(
        all_tags.len(),
        2,
        "expected exactly {{llm, rust}} to survive: {all_tags:?}"
    );
}

/// A distiller that (still) proposes nothing produces no canonical tags from
/// that source - the filter has nothing to gate, not an error.
#[tokio::test]
async fn empty_distiller_tags_yield_no_canonical_tags_from_that_source() {
    let dir = tempfile::tempdir().expect("tempdir");
    let config = config_with_fixture_canonical(dir.path());

    let sources = TagSources::from_summary("title", "text");
    let outcome = finalize_tags(sources, &config).await;

    assert!(
        outcome.tags.is_empty(),
        "no proposed tags means nothing survives: {:?}",
        outcome.tags
    );
}

/// Render just enough of a note to extract the byte-for-byte `tags:` block -
/// the AC3/P6 criterion is on the rendered YAML, not on `Vec<String>` equality.
fn tags_block(tags: &[String]) -> String {
    let note = crate::markdown::NoteContent {
        title: "T".to_string(),
        tags: tags.to_vec(),
        ..crate::markdown::NoteContent::default()
    };
    let rendered = crate::markdown::render_note(&note, &crate::config::FrontmatterConfig::default());
    let (yaml, _body) = vault::frontmatter::split_raw(&rendered).expect("frontmatter block");
    let start = yaml.find("tags:").expect("tags: key present");
    let rest = &yaml[start..];
    let end = rest[5..].find("\n\n").map(|i| i + 5).unwrap_or(rest.len());
    rest[..end].to_string()
}

/// Design doc P6 success criterion / AC3: two ingests of one fixture URL (same
/// title, same text, same distiller output) produce byte-identical `tags:`
/// blocks. `Deterministic` (the config default) is a pure function of its
/// candidates, so this is the borg-level seam - `TagSources` built the way
/// `pipeline.rs` builds them, through `finalize_tags` - proving that purity
/// survives the classifier dispatch, not just the `distillers::tags` unit.
///
/// Break-the-code evidence (implementation notes): reverting `finalize_tags`
/// to build a fresh `Deterministic` per call with a `HashMap`-backed mapping
/// iterated in nondeterministic order would falsify this; reverting the P6
/// candidate-building to skip `hygiene::sanitize_tag` before matching, so a
/// case difference between two "identical" ingests changed the raw candidate
/// text, also falsifies it.
#[tokio::test]
async fn ingest_tags_are_repeatable_under_deterministic() {
    let dir = tempfile::tempdir().expect("tempdir");
    let config = config_with_fixture_canonical(dir.path());

    let build_sources = || {
        let mut sources = TagSources::from_summary(
            "Async Rust for humans",
            "A practical guide to async Rust and LLM tooling.",
        );
        sources
            .model
            .extend(["rust", "llm"].iter().map(|t| hygiene::sanitize_tag(t)));
        sources
    };

    let ingest_1 = finalize_tags(build_sources(), &config).await;
    let ingest_2 = finalize_tags(build_sources(), &config).await;

    assert_eq!(
        ingest_1.tags, ingest_2.tags,
        "Deterministic must be a pure function of its candidates: {:?} vs {:?}",
        ingest_1.tags, ingest_2.tags
    );
    assert_eq!(
        tags_block(&ingest_1.tags),
        tags_block(&ingest_2.tags),
        "two ingests of the same fixture must render byte-identical tags: blocks"
    );
}

/// Design doc P6 success criterion / AC3, panel r4 OQ7: two ingests of the
/// SAME URL where the distiller's own `tags` output DRIFTS between calls (the
/// actual source of instability G5 restates around, `classifier-dev` at 23%
/// per Phase 0b) must not lose a canonical tag the first ingest produced. The
/// P3 reingest union (`apply_cortex_fields`, preserved-first) is what
/// delivers that, not the classifier call alone - so this drives BOTH:
/// `finalize_tags` twice with deliberately different `model` candidates, then
/// the same `apply_cortex_fields` union a URL reingest runs.
///
/// Break-the-code evidence (implementation notes): reverting P3's union to a
/// plain replace (fresh tags overwrite preserved) falsifies this immediately
/// - `python` would replace `rust` instead of joining it.
#[tokio::test]
async fn ingest_tags_are_stable_under_distiller_drift() {
    let dir = tempfile::tempdir().expect("tempdir");
    let config = config_with_fixture_canonical(dir.path());

    // Ingest 1: distiller proposes `rust`.
    let mut sources_1 = TagSources::from_summary("Async Rust for humans", "A guide to async Rust.");
    sources_1.model.push(hygiene::sanitize_tag("rust"));
    let ingest_1 = finalize_tags(sources_1, &config).await;
    assert_eq!(ingest_1.tags, vec!["rust".to_string()]);

    // Ingest 2 (a reingest of the same URL): the distiller's own output
    // DRIFTED to `llm` instead of `rust` - the exact instability this test
    // exists to survive. This is the FRESH render about to be published.
    let mut sources_2 = TagSources::from_summary("Async Rust for humans", "A guide to async Rust.");
    sources_2.model.push(hygiene::sanitize_tag("llm"));
    let ingest_2 = finalize_tags(sources_2, &config).await;
    assert_eq!(ingest_2.tags, vec!["llm".to_string()]);
    let fresh_rendered = tags_block(&ingest_2.tags);
    let fresh_note = format!("---\ntitle: T\n{fresh_rendered}\n---\nBody.\n");

    // The P3 reingest union: preserved (ingest 1's landed tags, read back off
    // the old note on disk) merged into the fresh render (ingest 2's tags).
    let preserved = vec![("tags".to_string(), FieldValue::List(ingest_1.tags.clone()))];
    let merged = apply_cortex_fields(&fresh_note, &preserved, Some(&canon(7, &[])));
    let (yaml, _body) = vault::frontmatter::split_raw(&merged).expect("frontmatter block");
    let map: serde_yaml::Mapping = serde_yaml::from_str(yaml).expect("parse merged frontmatter");
    let final_tags: Vec<String> = map
        .get("tags")
        .and_then(|v| v.as_sequence())
        .into_iter()
        .flatten()
        .filter_map(|v| v.as_str().map(str::to_string))
        .collect();

    assert!(
        final_tags.contains(&"rust".to_string()),
        "drift must not remove the first ingest's tag: {final_tags:?}"
    );
    assert!(
        final_tags.contains(&"llm".to_string()),
        "the union must absorb the drifted tag: {final_tags:?}"
    );
}

/// Design doc P6 success criterion: a classifier error runs the configured
/// `fallback`, and the outcome is marked `degraded` so the caller can flag the
/// receipt (`degraded=true`) rather than publishing a silently-untagged note.
///
/// Break-the-code evidence (implementation notes): reverting `finalize_tags`
/// to `.expect()` the primary classifier's result instead of falling back
/// turns this into a panic; reverting the `degraded` flag to always `false`
/// leaves `outcome.tags` correct but the assertion on `degraded` fails.
#[tokio::test]
async fn classifier_failure_degrades_visibly() {
    let dir = tempfile::tempdir().expect("tempdir");
    let mut config = config_with_fixture_canonical(dir.path());
    config.tags.classifier = distillers::tags::TagsClassifierConfig {
        classifier: distillers::tags::ClassifierKind::ClassifierDev,
        threshold: 0.9,
        // Keyless, like production. The error path comes from the
        // unreachable endpoint below (port 1), not from a missing
        // credential: an absent token is a valid keyless request now.
        token_env: String::new(),
        fallback: distillers::tags::ClassifierKind::Deterministic,
        endpoint: "http://127.0.0.1:1/classify".to_string(),
        timeout_secs: 1,
    };

    let mut sources = TagSources::from_summary("title", "text");
    sources.model.push(hygiene::sanitize_tag("rust"));

    let outcome = finalize_tags(sources, &config).await;

    assert!(
        outcome.degraded,
        "a classifier error must mark the outcome degraded so the receipt can say so"
    );
    assert!(
        outcome.tags.contains(&"rust".to_string()),
        "the configured fallback must still classify: {:?}",
        outcome.tags
    );
}

/// Design doc P6 success criterion: publisher-supplied candidates (a creator's
/// own hashtag, yt-dlp tags) that map to a canonical tag are recorded to
/// `author-tags` so a later `--retag` can recover them even when the
/// classifier fails to re-derive them from the text - and the key is
/// OMITTED, not emitted empty, when there is nothing to record.
///
/// Break-the-code evidence (implementation notes): reverting
/// `pipeline::tags::insert_author_tags` to always insert (even when empty)
/// falsifies the omission half; reverting `finalize_tags` to compute
/// `author_tags` from `sources.model` instead of `sources.author` falsifies
/// the provenance half (a distiller guess would leak into `author-tags`).
#[tokio::test]
async fn author_tags_are_recorded() {
    let dir = tempfile::tempdir().expect("tempdir");
    let config = config_with_fixture_canonical(dir.path());

    // A YouTube fixture: the creator's own `#rust` hashtag is author-side;
    // the classifier never sees it as a scored candidate.
    let mut sources = TagSources::from_summary("A talk about async Rust", "...");
    sources.author.push(hygiene::sanitize_tag("rust"));
    let outcome = finalize_tags(sources, &config).await;
    assert_eq!(outcome.author_tags, vec!["rust".to_string()]);

    let additions = insert_author_tags(std::collections::BTreeMap::new(), &outcome.author_tags);
    let note = crate::markdown::NoteContent {
        title: "T".to_string(),
        tags: outcome.tags.clone(),
        frontmatter_additions: additions,
        ..crate::markdown::NoteContent::default()
    };
    let rendered = crate::markdown::render_note(&note, &crate::config::FrontmatterConfig::default());
    assert!(
        rendered.contains("author-tags:") && rendered.contains("  - rust\n---"),
        "author-tags must be written when non-empty:\n{rendered}"
    );

    // The omission half: no author candidates means no key at all.
    let empty_note = crate::markdown::NoteContent {
        title: "T".to_string(),
        frontmatter_additions: insert_author_tags(std::collections::BTreeMap::new(), &[]),
        ..crate::markdown::NoteContent::default()
    };
    let empty_rendered = crate::markdown::render_note(&empty_note, &crate::config::FrontmatterConfig::default());
    assert!(
        !empty_rendered.contains("author-tags"),
        "author-tags must be omitted, not emitted empty:\n{empty_rendered}"
    );
}

/// Implementation audit r1, M3: `text` is what leaves the machine under
/// `classifier-dev`. A summary-less audio note used to hand the classifier
/// its whole transcript; `from_body` is the one seam that clips it.
#[test]
fn from_body_clips_a_transcript_to_the_documented_excerpt() {
    let transcript = "word ".repeat(4_000);
    let sources = TagSources::from_body("A voice note", &transcript);
    assert_eq!(
        sources.text.chars().count(),
        distillers::tags::CLASSIFIER_TEXT_CHARS,
        "the transcript was not clipped"
    );
    assert!(transcript.starts_with(&sources.text), "the excerpt is the head");
}

/// The other constructor is deliberately verbatim: a distilled summary is
/// already short, and cortex's `classifier_text` does not clip one either, so
/// both call paths score identical text for the same note.
#[test]
fn from_summary_is_verbatim() {
    let summary = "A distilled summary.";
    assert_eq!(TagSources::from_summary("T", summary).text, summary);
}

/// Implementation audit r1, M5: a vocabulary load failure used to return the
/// raw author+model candidates un-canonicalized, uncapped and
/// `degraded: false`, so the receipt read clean while an unfiltered tag set
/// reached the note. The documented contract is the opposite.
#[test]
fn a_vocabulary_failure_publishes_untagged_and_degraded() {
    let outcome = vocabulary_unavailable();
    assert!(outcome.tags.is_empty(), "unfiltered tags escaped: {:?}", outcome.tags);
    assert!(outcome.author_tags.is_empty());
    assert!(outcome.degraded, "the receipt would have read clean");
}
