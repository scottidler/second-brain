use super::*;

// Suite-wide lock: every test in this crate's test binary that mutates
// `XDG_CONFIG_HOME` (or resolves it indirectly) acquires the SAME
// `crate::testutil::ENV_LOCK` - see that static's doc comment for the race
// this closes.
use crate::testutil::ENV_LOCK;

struct EnvGuard {
    key: &'static str,
    original: Option<std::ffi::OsString>,
}

impl EnvGuard {
    fn set(key: &'static str, value: &std::path::Path) -> Self {
        let original = std::env::var_os(key);
        // SAFETY: intentional env mutation for path-resolution tests.
        unsafe { std::env::set_var(key, value) };
        Self { key, original }
    }
}

impl Drop for EnvGuard {
    fn drop(&mut self) {
        // SAFETY: restore env to avoid leaking state.
        unsafe {
            match self.original.take() {
                Some(v) => std::env::set_var(self.key, v),
                None => std::env::remove_var(self.key),
            }
        }
    }
}

#[test]
fn load_inner_defaults_when_primary_config_missing() {
    // 2026-07-24 cortex-association-sweep design, Phase 1 fail-closed loader:
    // a MISSING config file still defaults (this half of the contract is
    // unchanged - only the present-but-unparseable half hard-errors now).
    let _lock = ENV_LOCK.lock().expect("env lock");
    let tmp = tempfile::tempdir().expect("tempdir");
    let _guard = EnvGuard::set("XDG_CONFIG_HOME", tmp.path());
    // No cortex.yml written under tmp/sb/ - the primary path does not exist.

    let config = Config::load_inner(None).expect("missing config must default, not error");
    assert_eq!(config.log_level, "info", "defaulted config carries the default value");
}

#[test]
fn load_inner_fails_loud_on_present_but_unparseable_config() {
    // The fail-closed fix itself: a PRESENT config with a typo'd key must
    // hard-error, never silently fall back to defaults (the pre-Phase-1 bug -
    // a typo ran the daemon on defaults with zero visible signal).
    let _lock = ENV_LOCK.lock().expect("env lock");
    let tmp = tempfile::tempdir().expect("tempdir");
    let _guard = EnvGuard::set("XDG_CONFIG_HOME", tmp.path());

    let sb_root = tmp.path().join("sb");
    std::fs::create_dir_all(&sb_root).expect("create sb config dir");
    // `log-leveel` is a typo'd key that does not exist on `Config`. Config's
    // top-level struct has no `deny_unknown_fields`, so this alone would not
    // trip serde - use genuinely malformed YAML instead, which is the actual
    // failure mode `load_from_file`'s `serde_yaml::from_str` reports.
    std::fs::write(sb_root.join("cortex.yml"), "log-level: [unterminated\n").expect("write malformed cortex.yml");

    let err = Config::load_inner(None).expect_err("a present-but-unparseable config must hard-error");
    let msg = format!("{err:#}");
    assert!(
        msg.contains("cortex.yml"),
        "error should name the config file that failed to load: {msg}"
    );
}

#[test]
fn load_inner_explicit_path_hard_errors_on_unparseable_content() {
    // The explicit `--config <path>` branch already hard-errored before this
    // phase; pinning it here so a future refactor of `load_inner` cannot
    // silently regress it back to a warn-and-default.
    let tmp = tempfile::tempdir().expect("tempdir");
    let path = tmp.path().join("explicit.yml");
    std::fs::write(&path, "actions: {association: {threshold: not-a-number}}\n").expect("write");

    let err = Config::load_inner(Some(&path)).expect_err("unparseable explicit config must error");
    assert!(format!("{err:#}").contains("explicit.yml"));
}

#[test]
fn association_config_default_values() {
    let cfg = AssociationConfig::default();
    assert_eq!(cfg.threshold, 0.85);
    assert_eq!(cfg.similarity_source, SimilaritySource::Both);
    assert_eq!(cfg.min_quiescence_secs, 600);
    assert!(cfg.exclude.is_empty());
    // Phase 5: the daemon's own cadence knob, hourly by default (a merge is
    // soft-retire-destructive, so it runs far less often than the
    // read-mostly embed/graph ticks).
    assert_eq!(cfg.interval_secs, 3_600);
}

#[test]
fn association_config_deserializes_interval_secs_kebab_case() {
    let yaml = "interval-secs: 120\n";
    let cfg: AssociationConfig = serde_yaml::from_str(yaml).expect("deserialize");
    assert_eq!(cfg.interval_secs, 120);
}

#[test]
fn association_config_rejects_unknown_field() {
    // deny_unknown_fields on AssociationConfig itself: a typo'd key fails to
    // deserialize even in isolation from the full Config/loader path.
    let yaml = "threshold: 0.9\nsimilarty-source: both\n"; // "similarty" typo
    let err = serde_yaml::from_str::<AssociationConfig>(yaml).expect_err("typo'd key must fail deny_unknown_fields");
    assert!(format!("{err}").contains("unknown field"), "{err}");
}

#[test]
fn association_config_typo_under_full_config_fails_loud() {
    // The end-to-end shape `Config::load_from_file` actually parses: a typo
    // nested under `actions.association` must fail the whole `Config` parse,
    // not just an isolated `AssociationConfig`.
    let yaml = "actions:\n  association:\n    threshhold: 0.9\n"; // "threshhold" typo
    let err = serde_yaml::from_str::<Config>(yaml).expect_err("typo under actions.association must fail");
    assert!(format!("{err}").contains("unknown field"), "{err}");
}

#[test]
fn test_schema_config_default_non_empty() {
    // SchemaConfig::default() must be built from the vault::schema enums, never
    // the empty derived Default that validated nothing.
    let schema = SchemaConfig::default();
    assert!(!schema.domains.is_empty());
    assert!(!schema.types.is_empty());
    assert!(!schema.origins.is_empty());
    assert!(!schema.statuses.is_empty());
    assert!(!schema.methods.is_empty());
}

#[test]
fn test_schema_config_default_matches_enums() {
    use vault::schema::{Domain, Method, NoteType, Origin, Status};
    let schema = SchemaConfig::default();
    assert_eq!(schema.domains.len(), Domain::all().len());
    assert_eq!(schema.types.len(), NoteType::all().len());
    assert_eq!(schema.origins.len(), Origin::all().len());
    assert_eq!(schema.statuses.len(), Status::all().len());
    assert_eq!(schema.methods.len(), Method::all().len());
    // The two NoteType variants this phase added must flow through.
    assert!(schema.types.contains(&"digest".to_string()));
    assert!(schema.types.contains(&"review".to_string()));
}

#[test]
fn test_embed_kinds_absent_config_disables_claim() {
    // An existing cortex.yml with NO `embed` section must deserialize to the
    // claim-free baseline: summary + transcript-chunk ON, claim OFF. This is
    // the 2026-07-05 retrieval-gate remediation - claim regressed retrieval and
    // the daemon tick embedded it unconditionally, so absent config must land
    // claim OFF.
    let cfg: Config = serde_yaml::from_str("log-level: info\n").expect("parse minimal config");
    assert!(cfg.embed.kinds.summary, "summary must default ON");
    assert!(cfg.embed.kinds.transcript_chunk, "transcript-chunk must default ON");
    assert!(!cfg.embed.kinds.claim, "claim must default OFF");
}

#[test]
fn test_embed_kinds_explicit_claim_true_enables_claim() {
    // Opting in via config flips claim ON; the other two keep their defaults
    // (container-level #[serde(default)] fills the absent fields).
    let yaml = "embed:\n  kinds:\n    claim: true\n";
    let cfg: Config = serde_yaml::from_str(yaml).expect("parse config with embed.kinds.claim");
    assert!(cfg.embed.kinds.claim, "explicit claim: true must enable claim");
    assert!(cfg.embed.kinds.summary, "summary stays default ON");
    assert!(cfg.embed.kinds.transcript_chunk, "transcript-chunk stays default ON");
}
