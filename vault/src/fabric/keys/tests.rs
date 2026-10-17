use super::*;

#[test]
fn api_keys_deserialize_every_vendor_name() {
    let keys: ApiKeys =
        serde_yaml::from_str("anthropic: ESCOTE_ANTHROPIC_API_KEY\ngemini: GEMINI_API_KEY\nopenai: OPENAI_API_KEY\n")
            .expect("parse api-keys");
    assert_eq!(keys.get(Vendor::Anthropic), Some("ESCOTE_ANTHROPIC_API_KEY"));
    assert_eq!(keys.get(Vendor::Gemini), Some("GEMINI_API_KEY"));
    assert_eq!(keys.get(Vendor::OpenAi), Some("OPENAI_API_KEY"));
}

#[test]
fn api_keys_reject_an_unknown_vendor() {
    let err = serde_yaml::from_str::<ApiKeys>("mistral: MISTRAL_API_KEY\n").expect_err("unknown vendor must fail");
    assert!(err.to_string().contains("mistral"), "{err}");
}

#[test]
fn anthropic_shorthand_maps_only_anthropic_and_empty_is_empty() {
    let keys = ApiKeys::anthropic("ESCOTE_ANTHROPIC_API_KEY");
    assert_eq!(
        keys.iter().collect::<Vec<_>>(),
        vec![(Vendor::Anthropic, "ESCOTE_ANTHROPIC_API_KEY")]
    );
    assert!(ApiKeys::anthropic("").is_empty());
}

#[test]
fn fabric_env_names_match_what_fabric_reads() {
    assert_eq!(Vendor::Anthropic.fabric_env(), "ANTHROPIC_API_KEY");
    assert_eq!(Vendor::Gemini.fabric_env(), "GEMINI_API_KEY");
    assert_eq!(Vendor::OpenAi.fabric_env(), "OPENAI_API_KEY");
}
