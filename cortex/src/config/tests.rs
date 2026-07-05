use super::*;

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
