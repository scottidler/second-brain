use super::*;

#[test]
fn test_domain_roundtrip() {
    for d in Domain::all() {
        let s = d.as_str();
        let parsed: Domain = s.parse().expect("should parse");
        assert_eq!(*d, parsed);
    }
}

#[test]
fn test_domain_serde_roundtrip() {
    for d in Domain::all() {
        let json = serde_json::to_string(d).expect("serialize");
        let parsed: Domain = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(*d, parsed);
    }
}

#[test]
fn test_note_type_roundtrip() {
    for t in NoteType::all() {
        let s = t.as_str();
        let parsed: NoteType = s.parse().expect("should parse");
        assert_eq!(*t, parsed);
    }
}

#[test]
fn test_note_type_serde_roundtrip() {
    for t in NoteType::all() {
        let json = serde_json::to_string(t).expect("serialize");
        let parsed: NoteType = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(*t, parsed);
    }
}

#[test]
fn test_origin_roundtrip() {
    for o in Origin::all() {
        let s = o.as_str();
        let parsed: Origin = s.parse().expect("should parse");
        assert_eq!(*o, parsed);
    }
}

#[test]
fn test_status_roundtrip() {
    for s in Status::all() {
        let str_val = s.as_str();
        let parsed: Status = str_val.parse().expect("should parse");
        assert_eq!(*s, parsed);
    }
}

#[test]
fn test_method_roundtrip() {
    for m in Method::all() {
        let s = m.as_str();
        let parsed: Method = s.parse().expect("should parse");
        assert_eq!(*m, parsed);
    }
}

#[test]
fn test_method_includes_manual() {
    assert!(Method::all().contains(&Method::Manual));
}

#[test]
fn test_domain_display() {
    assert_eq!(Domain::Ai.to_string(), "ai");
    assert_eq!(Domain::Football.to_string(), "football");
}

#[test]
fn test_domain_case_insensitive_parse() {
    assert_eq!("AI".parse::<Domain>(), Ok(Domain::Ai));
    assert_eq!("Tech".parse::<Domain>(), Ok(Domain::Tech));
    assert_eq!("FOOTBALL".parse::<Domain>(), Ok(Domain::Football));
}

#[test]
fn test_unknown_domain_errors() {
    assert!("bogus".parse::<Domain>().is_err());
}

#[test]
fn test_unknown_note_type_errors() {
    assert!("blogpost".parse::<NoteType>().is_err());
}

#[test]
fn test_note_type_digest_review_variants() {
    assert_eq!("digest".parse::<NoteType>(), Ok(NoteType::Digest));
    assert_eq!("review".parse::<NoteType>(), Ok(NoteType::Review));
    assert_eq!(NoteType::Digest.as_str(), "digest");
    assert_eq!(NoteType::Review.as_str(), "review");
    assert!(NoteType::all().contains(&NoteType::Digest));
    assert!(NoteType::all().contains(&NoteType::Review));
}
