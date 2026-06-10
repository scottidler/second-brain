use super::*;
use crate::config::{Bm25Method, ExcludeConfig, MethodsConfig, RetrievalConfig, VaultConfig, VectorMethod};
use serde_json::json;
use std::path::PathBuf;
use vault::frontmatter::Frontmatter;
use vault::note::Note;
use vault::search::SearchIndex;

fn seed_one_article(db: &SearchIndex, path: &str, title: &str, body: &str) {
    let fm = Frontmatter {
        title: Some(title.to_string()),
        note_type: Some("article".to_string()),
        origin: Some("assisted".to_string()),
        domain: Some("ai".to_string()),
        ..Frontmatter::default()
    };
    let note = Note {
        path: PathBuf::from(path),
        frontmatter: fm,
        body: body.to_string(),
        raw: format!("---\n---\n{body}"),
    };
    db.index_one(&note, 100).expect("seed note");
}

/// Load-bearing regression guard for the decay model.
///
/// `knowledge_search` must NOT bump `search_hit_count` / `last_accessed_at`.
/// Counting BM25 / hybrid matches as access creates a positive feedback
/// loop where high-scoring notes become immortal and the entire decay
/// premise collapses (parent roadmap: "high-BM25-scoring notes become
/// immortal and the entire decay premise collapses"). Only an explicit
/// `note_read` is a human-intent signal. If a future refactor adds a
/// `bump_access` call into `knowledge_search`, this test must fail.
#[tokio::test]
async fn knowledge_search_does_not_bump_access() {
    let db = SearchIndex::open_memory().expect("open db");
    seed_one_article(
        &db,
        "notes/ai/transformer.md",
        "Transformer",
        "Transformer attention mechanism.",
    );
    let server = OracleMcpServer::new(Config::default(), db);

    // BM25 mode avoids the embedding-model lookup; the rule we are
    // guarding applies to every mode of knowledge_search.
    let search_args = json!({"query": "transformer", "mode": "bm25"});
    let result = server
        .dispatch("knowledge_search", search_args)
        .await
        .expect("knowledge_search dispatch");
    assert_ne!(result.is_error, Some(true), "knowledge_search returned an error");

    let signals_after_search = {
        let db = server.db.lock().expect("lock");
        db.note_signals("notes/ai/transformer.md")
            .expect("signals")
            .expect("present")
    };
    assert_eq!(
        signals_after_search.0, 0,
        "knowledge_search must not bump search_hit_count",
    );
    assert!(
        signals_after_search.1.is_none(),
        "knowledge_search must not stamp last_accessed_at",
    );

    // Now note_read MUST bump.
    let read_args = json!({"path": "notes/ai/transformer.md"});
    let result = server
        .dispatch("note_read", read_args)
        .await
        .expect("note_read dispatch");
    assert_ne!(result.is_error, Some(true), "note_read returned an error");

    let signals_after_read = {
        let db = server.db.lock().expect("lock");
        db.note_signals("notes/ai/transformer.md")
            .expect("signals")
            .expect("present")
    };
    assert_eq!(signals_after_read.0, 1, "note_read must bump search_hit_count");
    assert!(signals_after_read.1.is_some(), "note_read must stamp last_accessed_at",);
}

/// Decode a CallToolResult's first content item as a JSON value. All the
/// list-tools we test below serialize their response via Content::json,
/// which the rmcp `Content::json` constructor stores as RawContent::Text
/// (per rmcp 1.x).
fn first_content_as_json(result: &rmcp::model::CallToolResult) -> serde_json::Value {
    let text = result
        .content
        .first()
        .expect("response has at least one content item")
        .as_text()
        .expect("content[0] is text-shaped JSON")
        .text
        .clone();
    serde_json::from_str(&text).expect("content text is valid JSON")
}

/// Phase 2 invariant: every list-shaped tool's response is keyed on
/// `results`. After the clean rename, the legacy keys (`tags`,
/// `creators`, `sources`, `recent`) must be absent.
#[tokio::test]
async fn tag_search_no_arg_returns_results_key() {
    let db = SearchIndex::open_memory().expect("open db");
    seed_one_article(&db, "notes/ai/transformer.md", "Transformer", "body");
    let server = OracleMcpServer::new(Config::default(), db);

    let result = server.dispatch("tag_search", json!({})).await.expect("dispatch");
    assert_ne!(result.is_error, Some(true));
    let v = first_content_as_json(&result);
    assert!(v.get("results").is_some(), "tag_search must expose `results`: {v}");
    assert!(v.get("count").is_some(), "tag_search must expose `count`: {v}");
    assert!(
        v.get("tags").is_none(),
        "legacy `tags` key must be gone (clean rename, no aliases): {v}"
    );
}

#[tokio::test]
async fn creator_browse_no_arg_returns_results_key() {
    let db = SearchIndex::open_memory().expect("open db");
    seed_one_article(&db, "notes/ai/transformer.md", "Transformer", "body");
    let server = OracleMcpServer::new(Config::default(), db);

    let result = server.dispatch("creator_browse", json!({})).await.expect("dispatch");
    assert_ne!(result.is_error, Some(true));
    let v = first_content_as_json(&result);
    assert!(v.get("results").is_some(), "creator_browse must expose `results`: {v}");
    assert!(v.get("count").is_some());
    assert!(v.get("creators").is_none(), "legacy `creators` key must be gone: {v}");
}

#[tokio::test]
async fn source_browse_no_arg_returns_results_key() {
    let db = SearchIndex::open_memory().expect("open db");
    seed_one_article(&db, "notes/ai/transformer.md", "Transformer", "body");
    let server = OracleMcpServer::new(Config::default(), db);

    let result = server.dispatch("source_browse", json!({})).await.expect("dispatch");
    assert_ne!(result.is_error, Some(true));
    let v = first_content_as_json(&result);
    assert!(v.get("results").is_some(), "source_browse must expose `results`: {v}");
    assert!(v.get("count").is_some());
    assert!(v.get("sources").is_none(), "legacy `sources` key must be gone: {v}");
}

#[tokio::test]
async fn domain_brief_returns_results_key() {
    let db = SearchIndex::open_memory().expect("open db");
    seed_one_article(&db, "notes/ai/transformer.md", "Transformer", "body");
    let server = OracleMcpServer::new(Config::default(), db);

    let result = server
        .dispatch("domain_brief", json!({"domain": "ai"}))
        .await
        .expect("dispatch");
    assert_ne!(result.is_error, Some(true));
    let v = first_content_as_json(&result);
    assert!(v.get("results").is_some(), "domain_brief must expose `results`: {v}");
    assert!(v.get("recent").is_none(), "legacy `recent` key must be gone: {v}");
    assert!(
        v.get("recent_notes").is_none(),
        "legacy `recent_notes` key (per design doc) must be gone: {v}"
    );
    // unread is u64, not Option<u64>, so it must serialize as a number, never null.
    assert!(
        v.get("unread").is_some_and(|u| u.is_number()),
        "domain_brief.unread must be a number, never null: {v}"
    );
}

/// D2: missing-note paths should return a structured `{found: false, ...}`
/// payload, not a free-text string. The CallToolResult must NOT set
/// `is_error: true` (MCP `isError` is reserved for protocol-level
/// failures, not domain-level "no row matched").
#[tokio::test]
async fn note_read_missing_path_returns_found_false() {
    let db = SearchIndex::open_memory().expect("open db");
    let server = OracleMcpServer::new(Config::default(), db);

    let result = server
        .dispatch("note_read", json!({"path": "notes/does-not-exist.md"}))
        .await
        .expect("dispatch");
    assert_ne!(
        result.is_error,
        Some(true),
        "domain not-found must not set is_error: true",
    );
    let v = first_content_as_json(&result);
    assert_eq!(v.get("found").and_then(|f| f.as_bool()), Some(false), "{v}");
    assert_eq!(v.get("kind").and_then(|k| k.as_str()), Some("note"), "{v}");
    assert_eq!(
        v.get("path").and_then(|p| p.as_str()),
        Some("notes/does-not-exist.md"),
        "{v}"
    );
}

#[tokio::test]
async fn find_similar_missing_path_returns_found_false() {
    let db = SearchIndex::open_memory().expect("open db");
    let server = OracleMcpServer::new(Config::default(), db);

    let result = server
        .dispatch("find_similar", json!({"path": "notes/does-not-exist.md"}))
        .await
        .expect("dispatch");
    assert_ne!(result.is_error, Some(true));
    let v = first_content_as_json(&result);
    assert_eq!(v.get("found").and_then(|f| f.as_bool()), Some(false), "{v}");
    assert_eq!(v.get("kind").and_then(|k| k.as_str()), Some("note"), "{v}");
}

/// D2: invalid arguments (neither `content` nor `path`) IS a protocol-level
/// failure - the tool can't execute. This branch should set
/// `is_error: true` so MCP agents know the call itself failed.
#[tokio::test]
async fn find_similar_missing_args_returns_is_error() {
    let db = SearchIndex::open_memory().expect("open db");
    let server = OracleMcpServer::new(Config::default(), db);

    let result = server.dispatch("find_similar", json!({})).await.expect("dispatch");
    assert_eq!(
        result.is_error,
        Some(true),
        "invalid args must be a protocol-level error",
    );
}

#[tokio::test]
async fn find_links_missing_path_returns_found_false() {
    let db = SearchIndex::open_memory().expect("open db");
    let server = OracleMcpServer::new(Config::default(), db);

    let result = server
        .dispatch("find_links", json!({"path": "notes/does-not-exist.md"}))
        .await
        .expect("dispatch");
    assert_ne!(result.is_error, Some(true));
    let v = first_content_as_json(&result);
    assert_eq!(v.get("found").and_then(|f| f.as_bool()), Some(false), "{v}");
    assert_eq!(v.get("kind").and_then(|k| k.as_str()), Some("note"), "{v}");
}

/// D4: ingest_history should return its rows under the canonical
/// `results` key, not the legacy `entries` key. The Phase 2 design
/// classified ingest_history as "per-tool object - unchanged," but the
/// shakedown showed it's really a list-of-things tool that should
/// follow the same convention as tag_search/source_browse/etc.
#[tokio::test]
async fn ingest_history_returns_results_key() {
    // ingest_history needs a vault root to locate the ledger; query_entries
    // returns an empty Vec when the ledger file doesn't exist, so a bare
    // tempdir with a `.obsidian/` marker is sufficient.
    let tmp = tempfile::TempDir::new().expect("tempdir");
    std::fs::create_dir(tmp.path().join(".obsidian")).expect("mkdir .obsidian");
    let config = Config {
        vault: VaultConfig {
            root_path: Some(tmp.path().to_string_lossy().into_owned()),
        },
        ..Config::default()
    };
    let db = SearchIndex::open_memory().expect("open db");
    let server = OracleMcpServer::new(config, db);

    let result = server.dispatch("ingest_history", json!({})).await.expect("dispatch");
    assert_ne!(result.is_error, Some(true));
    let v = first_content_as_json(&result);
    assert!(v.get("results").is_some(), "ingest_history must expose `results`: {v}");
    assert!(v.get("count").is_some(), "ingest_history must expose `count`: {v}");
    assert!(v.get("entries").is_none(), "legacy `entries` key must be gone: {v}");
}

/// D4: inbox_status should rename `notes` -> `results`. The other keys
/// (inbox_count, needs_review, classified, review_candidates) stay -
/// they're semantic counters and a secondary list, not "the list" the
/// tool is named for.
#[tokio::test]
async fn inbox_status_returns_results_key() {
    let db = SearchIndex::open_memory().expect("open db");
    let server = OracleMcpServer::new(Config::default(), db);

    let result = server.dispatch("inbox_status", json!({})).await.expect("dispatch");
    assert_ne!(result.is_error, Some(true));
    let v = first_content_as_json(&result);
    assert!(v.get("results").is_some(), "inbox_status must expose `results`: {v}");
    assert!(v.get("notes").is_none(), "legacy `notes` key must be gone: {v}");
    // Secondary keys must remain.
    assert!(v.get("inbox_count").is_some(), "{v}");
    assert!(v.get("needs_review").is_some(), "{v}");
    assert!(v.get("classified").is_some(), "{v}");
    assert!(v.get("review_candidates").is_some(), "{v}");
}

#[tokio::test]
async fn duplicate_groups_missing_group_id_returns_found_false() {
    let db = SearchIndex::open_memory().expect("open db");
    let server = OracleMcpServer::new(Config::default(), db);

    let result = server
        .dispatch("duplicate_groups", json!({"group_id": "no-such-group"}))
        .await
        .expect("dispatch");
    assert_ne!(result.is_error, Some(true));
    let v = first_content_as_json(&result);
    assert_eq!(v.get("found").and_then(|f| f.as_bool()), Some(false), "{v}");
    assert_eq!(v.get("kind").and_then(|k| k.as_str()), Some("duplicate_group"), "{v}");
}

/// A retrieval config with only BM25 enabled. Lets the pipeline tests run
/// without loading the real embedding model (the vector path needs it).
fn bm25_only_config() -> Config {
    let retrieval = RetrievalConfig {
        methods: MethodsConfig {
            vector: VectorMethod {
                enabled: false,
                ..Default::default()
            },
            bm25: Bm25Method {
                enabled: true,
                ..Default::default()
            },
            ..Default::default()
        },
        ..Default::default()
    };
    Config {
        retrieval,
        ..Default::default()
    }
}

/// Phase 2: a `knowledge_search` with no `mode` routes to `run_pipeline`
/// (reported as `mode: "configured"`) and returns the configured retrievers'
/// results. Uses a bm25-only pipeline to avoid the embedding-model load.
#[tokio::test]
async fn knowledge_search_no_mode_routes_to_configured_pipeline() {
    let db = SearchIndex::open_memory().expect("open db");
    seed_one_article(
        &db,
        "notes/ai/transformer.md",
        "Transformer",
        "Transformer attention mechanism.",
    );
    let server = OracleMcpServer::new(bm25_only_config(), db);

    let result = server
        .dispatch("knowledge_search", json!({"query": "transformer"}))
        .await
        .expect("dispatch");
    assert_ne!(result.is_error, Some(true));
    let v = first_content_as_json(&result);
    assert_eq!(
        v.get("mode").and_then(|m| m.as_str()),
        Some("configured"),
        "no-mode call must route to the configured pipeline: {v}"
    );
    assert!(
        v.get("count").and_then(|c| c.as_u64()).unwrap_or(0) >= 1,
        "bm25 pipeline must find the seeded note: {v}"
    );
}

/// Explicit `mode` still uses the legacy single-mode path and reports that
/// mode's label (back-compat preserved).
#[tokio::test]
async fn knowledge_search_explicit_mode_reports_that_mode() {
    let db = SearchIndex::open_memory().expect("open db");
    seed_one_article(&db, "notes/ai/transformer.md", "Transformer", "body");
    let server = OracleMcpServer::new(Config::default(), db);

    let result = server
        .dispatch("knowledge_search", json!({"query": "transformer", "mode": "bm25"}))
        .await
        .expect("dispatch");
    assert_ne!(result.is_error, Some(true));
    let v = first_content_as_json(&result);
    assert_eq!(v.get("mode").and_then(|m| m.as_str()), Some("bm25"), "{v}");
}

/// Seed an article with a cortex `quality` level (empty string = unscored)
/// and a chosen body, so the exclude-filter tests can drive the `quality`
/// column and body length without the embedding model.
fn seed_with_quality(db: &SearchIndex, path: &str, title: &str, body: &str, quality: &str) {
    let mut extra = std::collections::HashMap::new();
    if !quality.is_empty() {
        extra.insert(
            "cortex-quality".to_string(),
            serde_yaml::Value::String(quality.to_string()),
        );
    }
    let fm = Frontmatter {
        title: Some(title.to_string()),
        note_type: Some("article".to_string()),
        origin: Some("assisted".to_string()),
        domain: Some("ai".to_string()),
        extra,
        ..Frontmatter::default()
    };
    let note = Note {
        path: PathBuf::from(path),
        frontmatter: fm,
        body: body.to_string(),
        raw: format!("---\n---\n{body}"),
    };
    db.index_one(&note, 100).expect("seed note");
}

fn bm25_config_with_exclude(exclude: ExcludeConfig) -> Config {
    let mut cfg = bm25_only_config();
    cfg.retrieval.exclude = exclude;
    cfg
}

/// Phase 3: the stub filter (on by default) drops a `quality=low` note from
/// the results while keeping a `quality=high` note that matches the query.
#[tokio::test]
async fn pipeline_stub_filter_drops_low_quality() {
    let db = SearchIndex::open_memory().expect("open db");
    seed_with_quality(
        &db,
        "notes/ai/good.md",
        "Good transformer",
        "transformer attention good",
        "high",
    );
    seed_with_quality(
        &db,
        "notes/ai/stub.md",
        "Stub transformer",
        "transformer attention stub",
        "low",
    );
    // bm25_only_config keeps exclude at its default (stub = true).
    let server = OracleMcpServer::new(bm25_only_config(), db);

    let result = server
        .dispatch(
            "knowledge_search",
            json!({"query": "transformer", "detail": "metadata"}),
        )
        .await
        .expect("dispatch");
    let v = first_content_as_json(&result);
    let paths: Vec<String> = v["results"]
        .as_array()
        .expect("results array")
        .iter()
        .map(|r| r["path"].as_str().unwrap_or_default().to_string())
        .collect();
    assert!(
        paths.contains(&"notes/ai/good.md".to_string()),
        "high-quality kept: {paths:?}"
    );
    assert!(
        !paths.contains(&"notes/ai/stub.md".to_string()),
        "low-quality stub must be dropped: {paths:?}"
    );
}

/// Phase 3: with `exclude.stub = false`, the same low-quality note survives.
#[tokio::test]
async fn pipeline_stub_filter_disabled_keeps_low_quality() {
    let db = SearchIndex::open_memory().expect("open db");
    seed_with_quality(
        &db,
        "notes/ai/stub.md",
        "Stub transformer",
        "transformer attention stub",
        "low",
    );
    let cfg = bm25_config_with_exclude(ExcludeConfig {
        stub: false,
        min_body_chars: 0,
    });
    let server = OracleMcpServer::new(cfg, db);

    let result = server
        .dispatch(
            "knowledge_search",
            json!({"query": "transformer", "detail": "metadata"}),
        )
        .await
        .expect("dispatch");
    let v = first_content_as_json(&result);
    let paths: Vec<String> = v["results"]
        .as_array()
        .expect("results array")
        .iter()
        .map(|r| r["path"].as_str().unwrap_or_default().to_string())
        .collect();
    assert!(
        paths.contains(&"notes/ai/stub.md".to_string()),
        "stub filter off must keep low-quality: {paths:?}"
    );
}

/// Phase 3: `min_body_chars` drops a note whose body is shorter than the
/// threshold and keeps a longer one.
#[tokio::test]
async fn pipeline_min_body_chars_drops_short_body() {
    let db = SearchIndex::open_memory().expect("open db");
    // Short body (< 50 chars) and a long one; both match the bm25 query.
    seed_with_quality(&db, "notes/ai/short.md", "Short", "transformer", "high");
    seed_with_quality(
        &db,
        "notes/ai/long.md",
        "Long",
        "transformer attention mechanism explained at length for retrieval testing purposes",
        "high",
    );
    let cfg = bm25_config_with_exclude(ExcludeConfig {
        stub: false,
        min_body_chars: 50,
    });
    let server = OracleMcpServer::new(cfg, db);

    let result = server
        .dispatch(
            "knowledge_search",
            json!({"query": "transformer", "detail": "metadata"}),
        )
        .await
        .expect("dispatch");
    let v = first_content_as_json(&result);
    let paths: Vec<String> = v["results"]
        .as_array()
        .expect("results array")
        .iter()
        .map(|r| r["path"].as_str().unwrap_or_default().to_string())
        .collect();
    assert!(
        paths.contains(&"notes/ai/long.md".to_string()),
        "long body kept: {paths:?}"
    );
    assert!(
        !paths.contains(&"notes/ai/short.md".to_string()),
        "short body must be dropped: {paths:?}"
    );
}

/// Phase 2: a pipeline with every retriever disabled returns no results
/// (and does not error) - the degenerate operator config.
#[tokio::test]
async fn run_pipeline_no_methods_enabled_returns_empty() {
    let db = SearchIndex::open_memory().expect("open db");
    seed_one_article(&db, "notes/ai/transformer.md", "Transformer", "body");
    // vector off, bm25/graph already off by default => nothing enabled.
    let retrieval = RetrievalConfig {
        methods: MethodsConfig {
            vector: VectorMethod {
                enabled: false,
                ..Default::default()
            },
            ..Default::default()
        },
        ..Default::default()
    };
    let cfg = Config {
        retrieval,
        ..Default::default()
    };
    let server = OracleMcpServer::new(cfg, db);

    let result = server
        .dispatch("knowledge_search", json!({"query": "transformer"}))
        .await
        .expect("dispatch");
    assert_ne!(result.is_error, Some(true));
    let v = first_content_as_json(&result);
    assert_eq!(
        v.get("count").and_then(|c| c.as_u64()),
        Some(0),
        "no methods enabled must yield zero results: {v}"
    );
}

/// Parity guard: every tool advertised by the router (`list_tools`) must have a
/// matching arm in the hand-written `dispatch()` mirror. Dispatching with a
/// `null` argument fails deserialization for every KNOWN tool (a struct can't
/// be built from null) before its body runs - so a known tool yields a
/// `"{name}: ..."` deser error, while a tool missing from `dispatch()` yields
/// `"unknown tool: ..."`. Asserting no router tool produces "unknown tool"
/// catches a tool added to the router but forgotten in the dispatch match.
#[tokio::test]
async fn every_router_tool_has_a_dispatch_arm() {
    let db = SearchIndex::open_memory().expect("open db");
    let server = OracleMcpServer::new(Config::default(), db);

    let tools = OracleMcpServer::list_tools();
    assert!(!tools.is_empty(), "router advertised no tools");

    for tool in &tools {
        let name = tool.name.as_ref();
        if let Err(e) = server.dispatch(name, serde_json::Value::Null).await {
            assert!(
                !e.message.contains("unknown tool"),
                "router tool {name:?} has no dispatch() arm: {}",
                e.message
            );
        }
    }
}
