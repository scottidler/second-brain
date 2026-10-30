use super::*;

#[test]
fn for_kind_maps_each_column_value() {
    let o = KindOffsets {
        summary: 0.1,
        transcript_chunk: 0.2,
        claim: 0.3,
    };
    assert_eq!(o.for_kind("summary").unwrap(), 0.1);
    assert_eq!(o.for_kind("transcript-chunk").unwrap(), 0.2);
    assert_eq!(o.for_kind("claim").unwrap(), 0.3);
}

#[test]
fn for_kind_rejects_an_unknown_kind() {
    let err = KindOffsets::default().for_kind("slide").expect_err("unknown kind");
    assert!(err.to_string().contains("slide"), "{err}");
}

#[test]
fn validate_rejects_infinity() {
    let o = KindOffsets {
        summary: f32::INFINITY,
        ..KindOffsets::default()
    };
    assert!(o.validate().is_err());
    assert!(KindOffsets::default().validate().is_ok());
}

#[test]
fn yaml_uses_kebab_case_and_defaults_missing_keys_to_zero() {
    let o: KindOffsets = serde_yaml::from_str("transcript-chunk: 0.05").unwrap();
    assert_eq!(o.transcript_chunk, 0.05);
    assert_eq!((o.summary, o.claim), (0.0, 0.0));
    assert!(serde_yaml::from_str::<KindOffsets>("transcript_chunk: 0.05").is_err());
}
