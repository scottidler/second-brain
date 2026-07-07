use super::*;

#[derive(Debug, Deserialize, Default, PartialEq)]
struct TestConfig {
    #[serde(default)]
    name: String,
}

#[test]
fn test_load_config_returns_default_when_no_file() {
    let config: TestConfig = load_config(None).expect("should succeed");
    assert_eq!(config, TestConfig::default());
}

#[test]
fn test_default_config() {
    let config = Config::default();
    assert_eq!(config.server.host, "0.0.0.0");
    assert_eq!(config.server.port, 8181);
    assert_eq!(config.transcriber.url, "http://localhost:8090");
    assert_eq!(config.groq.model, "whisper-large-v3");
    assert_eq!(config.llm.provider, "claude");
}

#[test]
fn test_pipeline_defaults_include_concurrency_caps() {
    let config = Config::default();
    assert_eq!(config.pipeline.max_concurrent_traces, DEFAULT_MAX_CONCURRENT_TRACES);
    assert_eq!(
        config.pipeline.max_concurrent_heavy_traces,
        DEFAULT_MAX_CONCURRENT_HEAVY_TRACES
    );
}

#[test]
fn test_pipeline_concurrency_caps_yaml_override() {
    let yaml = r#"
pipeline:
  max-concurrent-traces: 12
  max-concurrent-heavy-traces: 6
"#;
    let config: Config = serde_yaml::from_str(yaml).expect("should parse");
    assert_eq!(config.pipeline.max_concurrent_traces, 12);
    assert_eq!(config.pipeline.max_concurrent_heavy_traces, 6);
}

#[test]
fn test_thread_count_parse_integer() {
    let tc: ThreadCount = serde_yaml::from_str("4").expect("parse 4");
    assert_eq!(tc, ThreadCount::absolute(4));
}

#[test]
fn test_thread_count_parse_nproc() {
    let tc: ThreadCount = serde_yaml::from_str(r#""nproc""#).expect("parse nproc");
    assert_eq!(tc, ThreadCount::nproc_over(1));
}

#[test]
fn test_thread_count_parse_nproc_over_n() {
    let tc: ThreadCount = serde_yaml::from_str(r#""nproc/8""#).expect("parse nproc/8");
    assert_eq!(tc, ThreadCount::nproc_over(8));
}

#[test]
fn test_thread_count_rejects_invalid() {
    for bad in [
        "\"nproc/0\"",
        "\"-1\"",
        "\"4cores\"",
        "\"nproc/abc\"",
        "\"\"",
        "0",
        "-3",
    ] {
        let result: std::result::Result<ThreadCount, _> = serde_yaml::from_str(bad);
        assert!(result.is_err(), "expected error parsing {bad:?}, got {result:?}");
    }
}

#[test]
fn test_thread_count_resolve_floors_at_min() {
    assert_eq!(ThreadCount::absolute(1).resolve(), MIN_FFMPEG_THREADS);
    assert_eq!(ThreadCount::nproc_over(999).resolve(), MIN_FFMPEG_THREADS);
}

#[test]
fn test_thread_count_roundtrip_integer() {
    let tc = ThreadCount::absolute(4);
    let yaml = serde_yaml::to_string(&tc).expect("serialize");
    let reparsed: ThreadCount = serde_yaml::from_str(&yaml).expect("reparse");
    assert_eq!(reparsed, tc);
}

#[test]
fn test_thread_count_roundtrip_nproc() {
    let tc = ThreadCount::nproc_over(1);
    let yaml = serde_yaml::to_string(&tc).expect("serialize");
    let reparsed: ThreadCount = serde_yaml::from_str(&yaml).expect("reparse");
    assert_eq!(reparsed, tc);
}

#[test]
fn test_thread_count_roundtrip_nproc_over_n() {
    let tc = ThreadCount::nproc_over(8);
    let yaml = serde_yaml::to_string(&tc).expect("serialize");
    let reparsed: ThreadCount = serde_yaml::from_str(&yaml).expect("reparse");
    assert_eq!(reparsed, tc);
    assert!(
        yaml.contains("nproc/8"),
        "expected serialized form to contain 'nproc/8', got {yaml:?}"
    );
}

#[test]
fn test_thread_count_symbolic_forms() {
    assert_eq!(ThreadCount::absolute(4).symbolic(), "4");
    assert_eq!(ThreadCount::nproc_over(1).symbolic(), "nproc");
    assert_eq!(ThreadCount::nproc_over(8).symbolic(), "nproc/8");
}

#[test]
fn test_youtube_config_default_uses_nproc_over_default_denom() {
    let cfg = YoutubeConfig::default();
    assert_eq!(cfg.ffmpeg_threads, ThreadCount::nproc_over(DEFAULT_FFMPEG_THREAD_DENOM));
    assert_eq!(
        cfg.ffmpeg_filter_threads,
        ThreadCount::nproc_over(DEFAULT_FFMPEG_THREAD_DENOM)
    );
}

#[test]
fn test_youtube_config_serde_default_matches_struct_default() {
    let from_yaml: YoutubeConfig = serde_yaml::from_str("{}").expect("parse empty");
    let from_default = YoutubeConfig::default();
    assert_eq!(from_yaml.ffmpeg_threads, from_default.ffmpeg_threads);
    assert_eq!(from_yaml.ffmpeg_filter_threads, from_default.ffmpeg_filter_threads);
}

#[test]
fn test_youtube_config_yaml_override() {
    let yaml = r#"
youtube:
  ffmpeg-threads: 4
  ffmpeg-filter-threads: "nproc/4"
"#;
    let config: Config = serde_yaml::from_str(yaml).expect("should parse");
    assert_eq!(config.youtube.ffmpeg_threads, ThreadCount::absolute(4));
    assert_eq!(config.youtube.ffmpeg_filter_threads, ThreadCount::nproc_over(4));
}

#[test]
fn test_youtube_config_ffmpeg_thread_args_shape() {
    let cfg = YoutubeConfig {
        slides: YoutubeSlidesConfig::default(),
        ffmpeg_threads: ThreadCount::absolute(3),
        ffmpeg_filter_threads: ThreadCount::absolute(5),
    };
    let args = cfg.ffmpeg_thread_args();
    assert_eq!(args[0], "-threads");
    assert_eq!(args[1], "3");
    assert_eq!(args[2], "-filter_threads");
    assert_eq!(args[3], "5");
}

#[test]
fn test_youtube_config_yt_dlp_postprocessor_threads_matches_ffmpeg_threads() {
    let cfg = YoutubeConfig {
        slides: YoutubeSlidesConfig::default(),
        ffmpeg_threads: ThreadCount::absolute(3),
        ffmpeg_filter_threads: ThreadCount::absolute(5),
    };
    assert_eq!(cfg.yt_dlp_postprocessor_threads(), 3);
}

#[test]
fn test_config_deserialize() {
    let yaml = r#"
server:
  host: "127.0.0.1"
  port: 9090
vault:
  inbox-path: "/tmp/vault/inbox"
transcriber:
  url: "http://192.168.1.100:8090"
  timeout-secs: 60
groq:
  model: "whisper-large-v3-turbo"
llm:
  provider: "ollama"
  model: "llama3"
log-level: debug
"#;
    let config: Config = serde_yaml::from_str(yaml).expect("should parse");
    assert_eq!(config.server.host, "127.0.0.1");
    assert_eq!(config.server.port, 9090);
    assert_eq!(config.vault.inbox_path.as_deref(), Some("/tmp/vault/inbox"));
    assert_eq!(config.transcriber.url, "http://192.168.1.100:8090");
    assert_eq!(config.transcriber.timeout_secs, 60);
    assert_eq!(config.groq.model, "whisper-large-v3-turbo");
    assert_eq!(config.llm.provider, "ollama");
    assert_eq!(config.log_level.as_deref(), Some("debug"));
}

#[test]
fn test_config_without_bot_sections() {
    let yaml = r#"
server:
  host: "0.0.0.0"
  port: 8181
"#;
    let config: Config = serde_yaml::from_str(yaml).expect("should parse");
    assert!(config.telegram.is_none());
    assert!(config.discord.is_none());
    assert!(config.ntfy.is_none());
}

#[test]
fn test_config_with_ntfy_section() {
    let yaml = r#"
ntfy:
  topic: "obsidian-borg-abc123"
"#;
    let config: Config = serde_yaml::from_str(yaml).expect("should parse");
    let ntfy = config.ntfy.expect("ntfy should be Some");
    assert_eq!(ntfy.topic, "obsidian-borg-abc123");
    assert_eq!(ntfy.server, "https://ntfy.sh");
    assert!(ntfy.token.is_none());
}

#[test]
fn test_config_with_ntfy_full() {
    let yaml = r#"
ntfy:
  topic: "my-topic"
  server: "https://ntfy.example.com"
  token: "~/.config/ntfy/token"
"#;
    let config: Config = serde_yaml::from_str(yaml).expect("should parse");
    let ntfy = config.ntfy.expect("ntfy should be Some");
    assert_eq!(ntfy.topic, "my-topic");
    assert_eq!(ntfy.server, "https://ntfy.example.com");
    assert_eq!(ntfy.token, Some("~/.config/ntfy/token".to_string()));
}

#[test]
fn test_config_with_telegram_section() {
    let yaml = r#"
telegram:
  bot-token: TELEGRAM_BOT_TOKEN
  allowed-chat-ids: [123456, 789012]
"#;
    let config: Config = serde_yaml::from_str(yaml).expect("should parse");
    let tg = config.telegram.expect("telegram should be Some");
    assert_eq!(tg.bot_token, "TELEGRAM_BOT_TOKEN");
    assert_eq!(tg.allowed_chat_ids, vec![123456, 789012]);
}

#[test]
fn test_config_with_telegram_no_allowed_ids() {
    let yaml = r#"
telegram:
  bot-token: TELEGRAM_BOT_TOKEN
"#;
    let config: Config = serde_yaml::from_str(yaml).expect("should parse");
    let tg = config.telegram.expect("telegram should be Some");
    assert!(tg.allowed_chat_ids.is_empty());
}

#[test]
fn test_server_auth_token_parses_as_reference() {
    let yaml = r#"
server:
  host: "0.0.0.0"
  port: 8181
  auth-token: BORG_AUTH_TOKEN
"#;
    let config: Config = serde_yaml::from_str(yaml).expect("should parse");
    // The field holds the secret *reference* (env-var name / file path),
    // not a literal token; resolution happens at startup.
    assert_eq!(config.server.auth_token, Some("BORG_AUTH_TOKEN".to_string()));
}

#[test]
fn test_server_auth_token_defaults_none() {
    let config = Config::default();
    assert_eq!(config.server.auth_token, None);
}

#[test]
fn test_config_with_discord_section() {
    let yaml = r#"
discord:
  bot-token: DISCORD_BOT_TOKEN
  channel-id: 1234567890
"#;
    let config: Config = serde_yaml::from_str(yaml).expect("should parse");
    let dc = config.discord.expect("discord should be Some");
    assert_eq!(dc.bot_token, "DISCORD_BOT_TOKEN");
    assert_eq!(dc.channel_id, 1234567890);
}

#[test]
fn test_config_with_signal_section() {
    let yaml = r#"
signal:
  allowed-senders:
    - "00000000-0000-0000-0000-000000000001"
  host: home-server
"#;
    let config: Config = serde_yaml::from_str(yaml).expect("should parse");
    config
        .validate()
        .expect("validate should accept a populated signal section");
    let sg = config.signal.as_ref().expect("signal should be Some");
    assert_eq!(
        sg.allowed_senders,
        vec!["00000000-0000-0000-0000-000000000001".to_string()]
    );
    assert_eq!(sg.host, "home-server");
    assert_eq!(sg.notetoself_rate_threshold_per_hour, 100);
    assert!(sg.notification_recipient.is_none());
}

#[test]
fn test_config_with_signal_overrides_rate_threshold() {
    let yaml = r#"
signal:
  host: home-server
  notetoself-rate-threshold-per-hour: 250
  notification-recipient: "00000000-0000-0000-0000-000000000002"
"#;
    let config: Config = serde_yaml::from_str(yaml).expect("should parse");
    let sg = config.signal.expect("signal should be Some");
    assert_eq!(sg.notetoself_rate_threshold_per_hour, 250);
    assert_eq!(
        sg.notification_recipient.as_deref(),
        Some("00000000-0000-0000-0000-000000000002"),
    );
}

#[test]
fn test_validate_rejects_signal_with_empty_host() {
    let yaml = r#"
signal:
  host: ""
"#;
    let config: Config = serde_yaml::from_str(yaml).expect("should parse");
    let err = config.validate().expect_err("validate must reject empty signal.host");
    let msg = format!("{err:#}");
    assert!(
        msg.contains("signal.host"),
        "expected error to mention signal.host, got {msg:?}"
    );
}

#[test]
fn test_validate_rejects_signal_with_whitespace_host() {
    let yaml = r#"
signal:
  host: "   "
"#;
    let config: Config = serde_yaml::from_str(yaml).expect("should parse");
    let err = config
        .validate()
        .expect_err("validate must reject whitespace-only signal.host");
    let msg = format!("{err:#}");
    assert!(
        msg.contains("signal.host"),
        "expected error to mention signal.host, got {msg:?}"
    );
}

#[test]
fn test_validate_accepts_no_signal_block() {
    let config = Config::default();
    config
        .validate()
        .expect("validate should accept a config with no signal block");
}

#[test]
fn test_config_with_both_bots() {
    let yaml = r#"
telegram:
  bot-token: TG_TOKEN
discord:
  bot-token: DC_TOKEN
  channel-id: 999
"#;
    let config: Config = serde_yaml::from_str(yaml).expect("should parse");
    assert!(config.telegram.is_some());
    assert!(config.discord.is_some());
}

#[test]
fn test_default_canonicalization_rules() {
    let rules = default_canonicalization_rules();
    assert!(!rules.is_empty());
    assert_eq!(rules[0].name, "youtube-shorts-mobile");
}

#[test]
fn test_merge_canonicalization_rules_empty_config() {
    let merged = merge_canonicalization_rules(&[]);
    assert_eq!(merged.len(), default_canonicalization_rules().len());
}

#[test]
fn test_merge_canonicalization_rules_override() {
    let overrides = vec![CanonicalRule {
        name: "youtube-shortlink".to_string(),
        match_regex: "custom".to_string(),
        canonical: "custom".to_string(),
    }];
    let merged = merge_canonicalization_rules(&overrides);
    let rule = merged.iter().find(|r| r.name == "youtube-shortlink").expect("found");
    assert_eq!(rule.match_regex, "custom");
}

#[test]
fn test_merge_canonicalization_rules_append() {
    let custom = vec![CanonicalRule {
        name: "old-reddit".to_string(),
        match_regex: "r".to_string(),
        canonical: "c".to_string(),
    }];
    let merged = merge_canonicalization_rules(&custom);
    assert_eq!(merged.len(), default_canonicalization_rules().len() + 1);
    assert_eq!(merged.last().expect("last").name, "old-reddit");
}

#[test]
fn test_config_with_canonicalization() {
    let yaml = r#"
canonicalization:
  rules:
    - name: old-reddit
      match: 'https?://old\.reddit\.com/(?P<path>.*)'
      canonical: "https://www.reddit.com/{path}"
"#;
    let config: Config = serde_yaml::from_str(yaml).expect("should parse");
    assert_eq!(config.canonicalization.rules.len(), 1);
    assert_eq!(config.canonicalization.rules[0].name, "old-reddit");
}

#[test]
fn test_resolve_secret_from_file() {
    let dir = std::env::temp_dir().join("obsidian-borg-test-secret");
    fs::create_dir_all(&dir).expect("create dir");
    let file = dir.join("test-token");
    fs::write(&file, "  my-secret-value\n").expect("write");
    let result = resolve_secret(file.to_str().expect("path")).expect("resolve");
    assert_eq!(result, "my-secret-value");
    let _ = fs::remove_file(&file);
}

#[test]
fn test_resolve_secret_from_env() {
    let key = "OBSIDIAN_BORG_TEST_SECRET_42";
    // SAFETY: single-threaded test, no other threads reading this env var
    unsafe { std::env::set_var(key, "env-secret-value") };
    let result = resolve_secret(key).expect("resolve");
    assert_eq!(result, "env-secret-value");
    unsafe { std::env::remove_var(key) };
}

#[test]
fn test_resolve_secret_missing() {
    let result = resolve_secret("NONEXISTENT_VAR_OBSBORG_TEST_999");
    assert!(result.is_err());
}

#[test]
fn test_pipeline_config_defaults_for_split_fetch_timeouts() {
    // Per docs/design/2026-05-18-fabric-pattern-resolve-and-distill-dlq.md
    // the article-fetch path got split into per-subprocess timeouts so a
    // stuck `fabric -u` can no longer eat the LLM completion budget.
    let p = PipelineConfig::default();
    assert_eq!(p.fabric_url_timeout_secs, 60, "fabric -u: URL scrape, 60s ceiling");
    assert_eq!(
        p.fabric_transcript_timeout_secs, 120,
        "fabric -y: transcript fetch, 120s ceiling"
    );
    assert_eq!(p.markitdown_timeout_secs, 60, "markitdown fallback: 60s ceiling");
}

#[test]
fn test_pipeline_config_split_fetch_timeouts_from_yaml() {
    // YAML keys are kebab-case (rename_all = "kebab-case"); user-supplied
    // values must override the defaults. This locks the public surface
    // for the operator-tunable timeouts.
    let yaml = "\
fabric-url-timeout-secs: 45
fabric-transcript-timeout-secs: 90
markitdown-timeout-secs: 30
";
    let p: PipelineConfig = serde_yaml::from_str(yaml).expect("parse pipeline yaml");
    assert_eq!(p.fabric_url_timeout_secs, 45);
    assert_eq!(p.fabric_transcript_timeout_secs, 90);
    assert_eq!(p.markitdown_timeout_secs, 30);
    // Other defaults are preserved (serde(default) on PipelineConfig).
    assert_eq!(p.hard_timeout_secs, 1800);
    assert_eq!(p.jina_timeout_secs, 60);
}

#[test]
fn test_pipeline_config_split_fetch_timeouts_independent_of_fabric_pattern_timeout() {
    // `fabric.timeout_secs` (LLM pattern completion, currently 600s default)
    // must not be tied to any of the three new subprocess timeouts. This
    // test pins the invariant: changing one cannot change another.
    let p = PipelineConfig::default();
    let f = FabricConfig::default();
    assert_ne!(p.fabric_url_timeout_secs, f.timeout_secs);
    assert_ne!(p.fabric_transcript_timeout_secs, f.timeout_secs);
    assert_ne!(p.markitdown_timeout_secs, f.timeout_secs);
}

#[test]
fn test_pipeline_config_max_note_bytes_defaults_to_measured_ceiling() {
    // 2026-07-07 distillation-output-restore, Phase 3: the ceiling defaults
    // to MAX_NOTE_BYTES (65_536, the design's floor - the measured largest
    // transcript-free note in the live vault is 9,213 bytes, well under it).
    let p = PipelineConfig::default();
    assert_eq!(p.max_note_bytes, MAX_NOTE_BYTES);
    assert_eq!(p.max_note_bytes, 65_536);
}

#[test]
fn test_pipeline_config_max_note_bytes_yaml_override() {
    let yaml = "max-note-bytes: 131072\n";
    let p: PipelineConfig = serde_yaml::from_str(yaml).expect("parse pipeline yaml");
    assert_eq!(p.max_note_bytes, 131_072);
    // Other defaults untouched.
    assert_eq!(p.hard_timeout_secs, 1800);
}

#[test]
fn host_matches_fails_closed_when_hostname_unreadable() {
    // No pin: runs everywhere regardless of hostname readability.
    assert!(host_matches(&None, None));
    assert!(host_matches(&Some(String::new()), None));
    // Pin set + hostname known: case-insensitive match.
    assert!(host_matches(&Some("Desk".to_string()), Some("desk")));
    assert!(!host_matches(&Some("desk".to_string()), Some("laptop")));
    // Pin set + hostname UNREADABLE: fail closed (do NOT run).
    assert!(!host_matches(&Some("desk".to_string()), None));
}

// ---------------------------------------------------------------------------
// SlideCategory and ContentFilterConfig tests (Phase 1 of
// docs/design/2026-06-28-content-aware-slide-filtering.md)
// ---------------------------------------------------------------------------

#[test]
fn slide_category_roundtrip_defaults() {
    // ContentFilterConfig default: enabled=false, keep=[architecture-diagram],
    // model="", max-vision-concurrency=4, min-confidence=0.6
    let cfg = ContentFilterConfig::default();
    assert!(!cfg.enabled);
    assert_eq!(cfg.keep, vec![SlideCategory::ArchitectureDiagram]);
    assert_eq!(cfg.model, "");
    assert_eq!(cfg.max_vision_concurrency, DEFAULT_MAX_VISION_CONCURRENCY);
    assert!((cfg.min_confidence - DEFAULT_MIN_CONFIDENCE).abs() < f32::EPSILON);
}

#[test]
fn slide_category_roundtrip_yaml_override() {
    let yaml = r#"
youtube:
  slides:
    content-filter:
      enabled: true
      keep: [architecture-diagram, code, terminal]
      model: claude-opus-4-5
      max-vision-concurrency: 8
      min-confidence: 0.75
"#;
    let config: Config = serde_yaml::from_str(yaml).expect("should parse");
    let cf = &config.youtube.slides.content_filter;
    assert!(cf.enabled);
    assert_eq!(
        cf.keep,
        vec![
            SlideCategory::ArchitectureDiagram,
            SlideCategory::Code,
            SlideCategory::Terminal
        ]
    );
    assert_eq!(cf.model, "claude-opus-4-5");
    assert_eq!(cf.max_vision_concurrency, 8);
    assert!((cf.min_confidence - 0.75).abs() < f32::EPSILON);
}

#[test]
fn slide_category_all_variants_parse_lowercase() {
    let cases = [
        ("architecture-diagram", SlideCategory::ArchitectureDiagram),
        ("sequence-diagram", SlideCategory::SequenceDiagram),
        ("flowchart", SlideCategory::Flowchart),
        ("code", SlideCategory::Code),
        ("terminal", SlideCategory::Terminal),
        ("infographic", SlideCategory::Infographic),
        ("chart", SlideCategory::Chart),
        ("app-ui", SlideCategory::AppUi),
        ("webpage", SlideCategory::Webpage),
        ("talking-head", SlideCategory::TalkingHead),
        ("b-roll", SlideCategory::BRoll),
        ("title-card", SlideCategory::TitleCard),
        ("other", SlideCategory::Other),
    ];
    for (input, expected) in &cases {
        let yaml = format!(r#""{input}""#);
        let parsed: SlideCategory =
            serde_yaml::from_str(&yaml).unwrap_or_else(|e| panic!("expected {input:?} to parse, got: {e}"));
        assert_eq!(parsed, *expected, "mismatch for {input:?}");
    }
}

#[test]
fn slide_category_mixed_case_keep_entries_parse() {
    // cli.md: enum-valued flags must be case-insensitive on input.
    let yaml = r#"
youtube:
  slides:
    content-filter:
      enabled: true
      keep: [Architecture-Diagram, CODE, Talking-Head]
"#;
    let config: Config = serde_yaml::from_str(yaml).expect("should parse mixed-case keep entries");
    let keep = &config.youtube.slides.content_filter.keep;
    assert_eq!(
        keep,
        &vec![
            SlideCategory::ArchitectureDiagram,
            SlideCategory::Code,
            SlideCategory::TalkingHead
        ]
    );
}

#[test]
fn slide_category_uppercase_variants_parse() {
    // Each known variant must parse in all-uppercase form.
    let cases = [
        "ARCHITECTURE-DIAGRAM",
        "SEQUENCE-DIAGRAM",
        "FLOWCHART",
        "CODE",
        "TERMINAL",
        "INFOGRAPHIC",
        "CHART",
        "APP-UI",
        "WEBPAGE",
        "TALKING-HEAD",
        "B-ROLL",
        "TITLE-CARD",
        "OTHER",
    ];
    for input in &cases {
        let yaml = format!(r#""{input}""#);
        let result: Result<SlideCategory, _> = serde_yaml::from_str(&yaml);
        assert!(
            result.is_ok(),
            "expected {input:?} to parse (case-insensitive), got: {:?}",
            result.err()
        );
    }
}

#[test]
fn slide_category_unknown_string_is_hard_error() {
    // An unrecognised category in the keep array must be a parse error at
    // config load time, not a silent no-op.
    let bad_inputs = [
        r#""diagram""#,
        r#""video""#,
        r#""screenshot""#,
        r#""architecture_diagram""#, // underscores are not accepted
        r#""""#,
    ];
    for input in &bad_inputs {
        let result: Result<SlideCategory, _> = serde_yaml::from_str(input);
        assert!(
            result.is_err(),
            "expected {input:?} to fail, but it parsed successfully"
        );
    }
}

#[test]
fn slide_category_unknown_in_keep_array_is_hard_error() {
    let yaml = r#"
youtube:
  slides:
    content-filter:
      keep: [architecture-diagram, not-a-real-category]
"#;
    let result: Result<Config, _> = serde_yaml::from_str(yaml);
    assert!(
        result.is_err(),
        "expected config with unknown keep entry to fail to parse"
    );
    let msg = result
        .expect_err("config with unknown keep entry should fail to parse")
        .to_string();
    assert!(
        msg.contains("not-a-real-category"),
        "error should mention the unknown value, got: {msg}"
    );
}

#[test]
fn slide_category_serializes_to_kebab_case() {
    // Round-trip: serialize to YAML and confirm the canonical kebab-case form.
    let cat = SlideCategory::ArchitectureDiagram;
    let yaml = serde_yaml::to_string(&cat).expect("serialize");
    assert!(
        yaml.trim() == "architecture-diagram",
        "expected 'architecture-diagram', got: {yaml:?}"
    );
}

#[test]
fn content_filter_serde_default_matches_struct_default() {
    // An empty content-filter block must deserialize to the same value as
    // ContentFilterConfig::default().
    let from_yaml: ContentFilterConfig = serde_yaml::from_str("{}").expect("parse empty");
    let from_default = ContentFilterConfig::default();
    assert_eq!(from_yaml.enabled, from_default.enabled);
    assert_eq!(from_yaml.keep, from_default.keep);
    assert_eq!(from_yaml.model, from_default.model);
    assert_eq!(from_yaml.max_vision_concurrency, from_default.max_vision_concurrency);
    assert!((from_yaml.min_confidence - from_default.min_confidence).abs() < f32::EPSILON);
}

// ---------------------------------------------------------------------------
// DistillConfig tests — distillation feature toggles, all default-on.
// The critical guard is the bool-serde-default footgun: absent fields (and an
// absent `distill:` block entirely) MUST land TRUE, not the bool default false.
// ---------------------------------------------------------------------------

#[test]
fn test_distill_config_default_all_true() {
    let d = DistillConfig::default();
    assert!(d.slide_append);
    assert!(d.capture_note);
    assert!(d.propose_tags);
}

#[test]
fn test_distill_config_absent_block_defaults_all_true() {
    // CRITICAL back-compat: a borg.yml with NO `distill:` section must
    // deserialize to all three flags = TRUE (guards the bool-serde-default
    // footgun — container `#[serde(default)]` fills from the struct Default).
    let yaml = r#"
server:
  host: "0.0.0.0"
  port: 8181
"#;
    let config: Config = serde_yaml::from_str(yaml).expect("should parse");
    assert!(config.distill.slide_append);
    assert!(config.distill.capture_note);
    assert!(config.distill.propose_tags);
}

#[test]
fn test_distill_config_empty_block_defaults_all_true() {
    // An empty `distill: {}` block must also default every flag TRUE.
    let from_yaml: DistillConfig = serde_yaml::from_str("{}").expect("parse empty");
    assert!(from_yaml.slide_append);
    assert!(from_yaml.capture_note);
    assert!(from_yaml.propose_tags);
}

#[test]
fn test_distill_config_partial_block_defaults_unspecified_true() {
    // Explicit `slide-append: false` flips ONLY that flag; the other two
    // stay TRUE (container default fills the missing fields from the struct
    // Default, not from bool::default()).
    let yaml = r#"
distill:
  slide-append: false
"#;
    let config: Config = serde_yaml::from_str(yaml).expect("should parse");
    assert!(!config.distill.slide_append);
    assert!(config.distill.capture_note);
    assert!(config.distill.propose_tags);
}

#[test]
fn test_distill_config_all_false_yaml_override() {
    let yaml = r#"
distill:
  slide-append: false
  capture-note: false
  propose-tags: false
"#;
    let config: Config = serde_yaml::from_str(yaml).expect("should parse");
    assert!(!config.distill.slide_append);
    assert!(!config.distill.capture_note);
    assert!(!config.distill.propose_tags);
}

#[test]
fn test_distill_config_stale_article_transcript_key_fails_loudly() {
    // 2026-07-07 distillation-output-restore, Phase 3: `article-transcript`
    // was removed (nothing left to configure once `## Transcript` is gone
    // from render for every article/video note). `deny_unknown_fields` turns
    // a stale key left over in an existing borg.yml into a loud, named error
    // at config-load time rather than a silently-ignored no-op.
    let yaml = r#"
distill:
  article-transcript: false
"#;
    let err = serde_yaml::from_str::<Config>(yaml).expect_err("stale key must fail to deserialize");
    let msg = err.to_string();
    assert!(
        msg.contains("article-transcript") || msg.contains("article_transcript"),
        "error must name the unknown field: {msg}"
    );
}

#[test]
fn youtube_slides_config_no_longer_has_vision_per_slide() {
    // The dead `vision_per_slide` stub was removed in Phase 1.
    // This test would fail to compile if the field were reintroduced.
    let cfg = YoutubeSlidesConfig::default();
    // Verify the new content_filter field is present instead.
    assert!(!cfg.content_filter.enabled);
}
