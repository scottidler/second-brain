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
