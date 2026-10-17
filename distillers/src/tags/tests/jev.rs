//! `Jev` against a loopback stub: one test per documented branch of the
//! design doc's API Design. No test here touches the network.

use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpListener;
use std::sync::{Arc, Mutex};

use super::*;

const FIXTURE: &str = include_str!("../fixtures/jev-2026-10-16.json");
const SHIPPED_CANONICAL_TAGS: &str = include_str!("../../../../config/canonical-tags.yml");

/// Never a real key. Every error test asserts this string is absent.
const KEY: &str = "ts-test-key-SENTINEL-7f3a";

/// The shipped 117-tag vocabulary with its `descriptions:`, exactly as
/// borg and cortex load it.
fn shipped_canon() -> CanonicalSet {
    let file: vault::canonical::CanonicalTagsFile =
        serde_yaml::from_str(SHIPPED_CANONICAL_TAGS).expect("shipped canonical-tags.yml parses");
    file.canonical_set()
}

/// One canned HTTP answer.
struct Canned {
    status: u16,
    headers: Vec<(&'static str, String)>,
    body: String,
}

impl Canned {
    fn json(status: u16, body: impl Into<String>) -> Self {
        Self {
            status,
            headers: vec![("x-typesafe-request-id", "req_stub_0001".to_string())],
            body: body.into(),
        }
    }

    fn with_header(mut self, name: &'static str, value: &str) -> Self {
        self.headers.push((name, value.to_string()));
        self
    }
}

/// What the stub saw: lowercased header lines and the raw body.
struct Hit {
    headers: Vec<String>,
    body: String,
}

/// A loopback HTTP/1.1 stub serving `answers` in order, then 500 for any
/// extra request (so an unexpected request still shows up in `hits`).
struct Stub {
    base_url: String,
    hits: Arc<Mutex<Vec<Hit>>>,
}

impl Stub {
    fn start(answers: Vec<Canned>) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind loopback");
        let base_url = format!("http://{}", listener.local_addr().expect("addr"));
        let hits = Arc::new(Mutex::new(Vec::new()));
        let seen = Arc::clone(&hits);
        std::thread::spawn(move || {
            let mut answers = answers.into_iter();
            for stream in listener.incoming() {
                let Ok(mut stream) = stream else { continue };
                let mut reader = BufReader::new(stream.try_clone().expect("clone stream"));
                let mut headers = Vec::new();
                let mut length = 0usize;
                loop {
                    let mut line = String::new();
                    if reader.read_line(&mut line).expect("read header") == 0 {
                        break;
                    }
                    let line = line.trim_end().to_string();
                    if line.is_empty() {
                        break;
                    }
                    let lower = line.to_ascii_lowercase();
                    if let Some(v) = lower.strip_prefix("content-length:") {
                        length = v.trim().parse().expect("content-length");
                    }
                    headers.push(lower);
                }
                let mut body = vec![0u8; length];
                reader.read_exact(&mut body).expect("read body");
                seen.lock().expect("lock").push(Hit {
                    headers,
                    body: String::from_utf8(body).expect("utf8 body"),
                });

                let canned = answers
                    .next()
                    .unwrap_or_else(|| Canned::json(500, r#"{"detail":"stub has no more answers"}"#));
                let mut response = format!(
                    "HTTP/1.1 {} STUB\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n",
                    canned.status,
                    canned.body.len()
                );
                for (name, value) in &canned.headers {
                    response.push_str(&format!("{name}: {value}\r\n"));
                }
                response.push_str("\r\n");
                response.push_str(&canned.body);
                let _ = stream.write_all(response.as_bytes());
            }
        });
        Self { base_url, hits }
    }

    fn hit_count(&self) -> usize {
        self.hits.lock().expect("lock").len()
    }

    fn body(&self, i: usize) -> serde_json::Value {
        serde_json::from_str(&self.hits.lock().expect("lock")[i].body).expect("request body is JSON")
    }

    fn headers(&self, i: usize) -> Vec<String> {
        self.hits.lock().expect("lock")[i].headers.clone()
    }
}

/// A `Jev` pointed at `stub`, its key read from a secret file so no test
/// mutates the process environment.
fn jev_for(stub: &Stub, canon: CanonicalSet, timeout_secs: u64) -> (Jev, tempfile::TempDir) {
    let dir = tempfile::tempdir().expect("tempdir");
    let key_path = dir.path().join("typesafe-key");
    std::fs::write(&key_path, format!("{KEY}\n")).expect("write key");
    let cfg = TagsClassifierConfig {
        classifier: ClassifierKind::Jev,
        api_key: key_path.to_string_lossy().into_owned(),
        timeout_secs,
        base_url: Some(stub.base_url.clone()),
        ..TagsClassifierConfig::default()
    };
    (Jev::new(canon, TagMapping::default(), &cfg), dir)
}

fn fixture() -> serde_json::Value {
    serde_json::from_str(FIXTURE).expect("fixture parses")
}

fn fixture_body() -> String {
    fixture()["response"].to_string()
}

fn note() -> TagInput<'static> {
    TagInput {
        title: "A note",
        text: "Text about writing a command-line tool in Rust.",
        candidates: &[],
    }
}

/// An answer for every canonical tag, each scored `score`, from `model`.
fn answers_for(canon: &CanonicalSet, model: &str, score: f64) -> serde_json::Value {
    let answers: serde_json::Map<String, serde_json::Value> = canon
        .all
        .iter()
        .map(|t| (t.clone(), serde_json::json!({"type": "noul", "noul": score})))
        .collect();
    serde_json::json!({"model": model, "answers": answers})
}

fn assert_keyless(err: &eyre::Report) {
    let text = format!("{err:#}");
    assert!(!text.contains(KEY), "the key leaked into the error: {text}");
}

// ------------------------------------------------------------------ 200

/// Success criterion: exactly 1 request per `classify`, one `noul`
/// question per canonical tag, each carrying the tag's description as
/// `criteria.true`, and the recorded live answer selecting the recorded tags.
#[test]
fn a_classify_sends_one_request_with_a_noul_question_per_canonical_tag() {
    let fx = fixture();
    let canon = shipped_canon();
    let stub = Stub::start(vec![Canned::json(200, fixture_body())]);
    let (jev, _dir) = jev_for(&stub, canon.clone(), 30);

    let title = fx["title"].as_str().expect("title");
    let text = fx["text"].as_str().expect("text");
    let out = jev
        .classify(&TagInput {
            title,
            text,
            candidates: &[],
        })
        .expect("classify");

    assert_eq!(stub.hit_count(), 1, "exactly one request per classify");
    let body = stub.body(0);
    let questions = body["questions"].as_object().expect("questions map");
    assert_eq!(questions.len(), canon.all.len(), "one question per canonical tag");
    for (tag, question) in questions {
        assert!(canon.all.contains(tag), "question for non-canonical {tag}");
        assert_eq!(question["type"], "noul", "{tag}");
        assert_eq!(question["instructions"], format!("Is this text about {tag}?"));
        assert_eq!(
            question["criteria"]["true"].as_str(),
            canon.description.get(tag).map(String::as_str),
            "{tag} must carry its description as criteria.true"
        );
    }
    assert_eq!(body["model"], "jev-latest");
    assert_eq!(body["state"], format!("{title}\n\n{text}"));
    assert!(
        stub.headers(0)
            .contains(&format!("authorization: bearer {}", KEY.to_ascii_lowercase())),
        "the key rides the Authorization header"
    );

    let want: Vec<String> = fx["expected"]
        .as_array()
        .expect("expected")
        .iter()
        .map(|v| v.as_str().expect("tag").to_string())
        .collect();
    assert_eq!(out.tags, want, "the recorded answer selects the recorded tags");
    assert_eq!(out.method, TagMethod::Jev);
    assert_eq!(out.confidence, Confidence::High);
    assert!(out.tags.len() <= canon.max_per_note);
}

#[test]
fn a_tag_without_a_description_is_asked_by_name_alone() {
    let mut canon = shipped_canon();
    canon.description.remove("rust");
    let stub = Stub::start(vec![Canned::json(
        200,
        answers_for(&canon, "jev-1.13.0", 0.1).to_string(),
    )]);
    let (jev, _dir) = jev_for(&stub, canon, 30);
    jev.classify(&note()).expect("classify");

    let body = stub.body(0);
    assert!(
        body["questions"]["rust"].get("criteria").is_none(),
        "no criteria without a description"
    );
    assert!(body["questions"]["cli"]["criteria"]["true"].is_string());
}

#[test]
fn unasked_answer_keys_are_dropped() {
    let canon = shipped_canon();
    let mut reply = answers_for(&canon, "jev-1.13.0", 0.0);
    reply["answers"]["not-a-canonical-tag"] = serde_json::json!({"type": "noul", "noul": 0.99});
    reply["answers"]["rust"] = serde_json::json!({"type": "noul", "noul": 0.97});
    let stub = Stub::start(vec![Canned::json(200, reply.to_string())]);
    let (jev, _dir) = jev_for(&stub, canon, 30);
    let out = jev.classify(&note()).expect("classify");
    assert_eq!(out.tags, vec!["rust".to_string()]);
}

// --------------------------------------------------------------- errors

/// Success criterion: the 401 error names `authentication_error` and never
/// the key.
#[test]
fn a_401_names_authentication_error_and_not_the_key() {
    let stub = Stub::start(vec![Canned::json(
        401,
        r#"{"detail":{"error_type":"authentication_error","message":"Invalid API key."}}"#,
    )]);
    let (jev, _dir) = jev_for(&stub, shipped_canon(), 30);
    let err = jev.classify(&note()).expect_err("401 is an error");
    let text = format!("{err:#}");
    assert!(text.contains("authentication_error"), "{text}");
    assert!(text.contains("401"), "{text}");
    assert!(text.contains("req_stub_0001"), "the request id is carried: {text}");
    assert_keyless(&err);
}

#[test]
fn a_400_names_api_usage_error() {
    let stub = Stub::start(vec![Canned::json(
        400,
        r#"{"detail":{"error_type":"api_usage_error","message":"Unknown model: jev-1.12"}}"#,
    )]);
    let (jev, _dir) = jev_for(&stub, shipped_canon(), 30);
    let err = jev.classify(&note()).expect_err("400 is an error");
    let text = format!("{err:#}");
    assert!(text.contains("api_usage_error"), "{text}");
    assert!(text.contains("Unknown model: jev-1.12"), "{text}");
    assert_keyless(&err);
}

/// FastAPI's 422 echoes the whole request in `input`; none of it may reach
/// the error text.
#[test]
fn a_422_drops_the_echoed_input() {
    let echo = r#"{"detail":[{"type":"missing","loc":["body","state"],"msg":"Field required","input":{"model":"jev-latest","questions":{"rust":"ECHOED-REQUEST-MARKER"}}}]}"#;
    let stub = Stub::start(vec![Canned::json(422, echo)]);
    let (jev, _dir) = jev_for(&stub, shipped_canon(), 30);
    let err = jev.classify(&note()).expect_err("422 is an error");
    let text = format!("{err:#}");
    assert!(text.contains("validation_error"), "{text}");
    assert!(text.contains("body.state: Field required"), "{text}");
    assert!(
        !text.contains("ECHOED-REQUEST-MARKER"),
        "the echoed input leaked: {text}"
    );
    assert!(!text.contains("input"), "{text}");
    assert_keyless(&err);
}

#[test]
fn a_500_is_an_error() {
    let stub = Stub::start(vec![Canned::json(500, "<html>upstream broke</html>")]);
    let (jev, _dir) = jev_for(&stub, shipped_canon(), 30);
    let err = jev.classify(&note()).expect_err("500 is an error");
    let text = format!("{err:#}");
    assert!(text.contains("500"), "{text}");
    assert!(
        !text.contains("upstream broke"),
        "a non-JSON body contributes nothing: {text}"
    );
}

#[test]
fn malformed_json_is_an_error() {
    let stub = Stub::start(vec![Canned::json(200, "{\"model\": \"jev-1.13.0\", \"answers\": ")]);
    let (jev, _dir) = jev_for(&stub, shipped_canon(), 30);
    let err = jev.classify(&note()).expect_err("malformed JSON is an error");
    assert!(format!("{err:#}").contains("malformed JSON"), "{err:#}");
}

#[test]
fn a_missing_tag_is_an_error() {
    let canon = shipped_canon();
    let mut reply = answers_for(&canon, "jev-1.13.0", 0.5);
    reply["answers"].as_object_mut().expect("answers").remove("rust");
    let stub = Stub::start(vec![Canned::json(200, reply.to_string())]);
    let (jev, _dir) = jev_for(&stub, canon, 30);
    let err = jev.classify(&note()).expect_err("a missing tag is an error");
    assert!(format!("{err:#}").contains("\"rust\""), "{err:#}");
}

#[test]
fn a_noul_outside_the_unit_interval_is_an_error() {
    let canon = shipped_canon();
    for bad in [1.5, -0.1] {
        let mut reply = answers_for(&canon, "jev-1.13.0", 0.5);
        reply["answers"]["rust"] = serde_json::json!({"type": "noul", "noul": bad});
        let stub = Stub::start(vec![Canned::json(200, reply.to_string())]);
        let (jev, _dir) = jev_for(&stub, canon.clone(), 30);
        let err = jev.classify(&note()).expect_err("out-of-range noul is an error");
        assert!(format!("{err:#}").contains("outside [0, 1]"), "{err:#}");
    }
}

#[test]
fn a_non_jev_model_is_an_error() {
    let canon = shipped_canon();
    let reply = answers_for(&canon, "ling-3.0-flash", 0.5);
    let stub = Stub::start(vec![Canned::json(200, reply.to_string())]);
    let (jev, _dir) = jev_for(&stub, canon, 30);
    let err = jev.classify(&note()).expect_err("a non-jev answer is an error");
    assert!(format!("{err:#}").contains("ling-3.0-flash"), "{err:#}");
}

// ------------------------------------------------------------------ 429

#[test]
fn a_429_then_200_retries_once() {
    let canon = shipped_canon();
    let stub = Stub::start(vec![
        Canned::json(429, r#"{"detail":{"error_type":"rate_limit_error"}}"#).with_header("retry-after-ms", "20"),
        Canned::json(200, answers_for(&canon, "jev-1.13.0", 0.95).to_string()),
    ]);
    let (jev, _dir) = jev_for(&stub, canon, 30);
    let out = jev.classify(&note()).expect("the retry succeeds");
    assert_eq!(stub.hit_count(), 2, "one retry");
    assert_eq!(out.confidence, Confidence::High);
}

#[test]
fn a_429_then_429_is_an_error() {
    let stub = Stub::start(vec![
        Canned::json(429, "{}").with_header("retry-after", "0"),
        Canned::json(429, "{}").with_header("retry-after", "0"),
    ]);
    let (jev, _dir) = jev_for(&stub, shipped_canon(), 30);
    let err = jev.classify(&note()).expect_err("a second 429 is an error");
    assert_eq!(stub.hit_count(), 2, "never a third request");
    assert!(format!("{err:#}").contains("429 again"), "{err:#}");
    assert_keyless(&err);
}

#[test]
fn a_429_with_no_header_is_an_error_without_a_retry() {
    let stub = Stub::start(vec![Canned::json(429, "{}")]);
    let (jev, _dir) = jev_for(&stub, shipped_canon(), 30);
    let err = jev.classify(&note()).expect_err("no header, no retry");
    assert_eq!(stub.hit_count(), 1);
    assert!(format!("{err:#}").contains("no usable"), "{err:#}");
}

#[test]
fn a_429_whose_delay_overruns_the_deadline_is_an_error_without_a_retry() {
    let stub = Stub::start(vec![Canned::json(429, "{}").with_header("retry-after", "5")]);
    let (jev, _dir) = jev_for(&stub, shipped_canon(), 2);
    let err = jev
        .classify(&note())
        .expect_err("a delay past the deadline is an error");
    assert_eq!(stub.hit_count(), 1);
    assert!(format!("{err:#}").contains("does not fit"), "{err:#}");
}

#[test]
fn retry_delay_prefers_milliseconds_then_seconds() {
    assert_eq!(retry_delay(Some("250"), Some("3")), Some(Duration::from_millis(250)));
    assert_eq!(retry_delay(Some("soon"), Some("3")), Some(Duration::from_secs(3)));
    assert_eq!(retry_delay(None, Some("3")), Some(Duration::from_secs(3)));
    assert_eq!(retry_delay(None, Some("Wed, 21 Oct 2026 07:28:00 GMT")), None);
    assert_eq!(retry_delay(None, None), None);
}

// ------------------------------------------------------- no request at all

/// Success criterion: an api-key name that resolves empty errors with zero
/// stub hits. Inverts the old keyless-tier test: there is no keyless path.
#[test]
fn an_unresolved_api_key_errors_without_a_request() {
    let stub = Stub::start(vec![Canned::json(200, fixture_body())]);
    let cfg = TagsClassifierConfig {
        classifier: ClassifierKind::Jev,
        api_key: "DISTILLERS_TEST_UNSET_TYPESAFE_KEY_9c2f4b".to_string(),
        base_url: Some(stub.base_url.clone()),
        ..TagsClassifierConfig::default()
    };
    let jev = Jev::new(shipped_canon(), TagMapping::default(), &cfg);
    let err = jev.classify(&note()).expect_err("an empty key is an error");
    assert!(
        format!("{err:#}").contains("DISTILLERS_TEST_UNSET_TYPESAFE_KEY_9c2f4b"),
        "the error names the variable: {err:#}"
    );
    assert_eq!(stub.hit_count(), 0, "zero requests without a key");
}

#[test]
fn a_blank_key_file_errors_without_a_request() {
    let stub = Stub::start(vec![Canned::json(200, fixture_body())]);
    let dir = tempfile::tempdir().expect("tempdir");
    let key_path = dir.path().join("blank");
    std::fs::write(&key_path, "   \n").expect("write");
    let cfg = TagsClassifierConfig {
        classifier: ClassifierKind::Jev,
        api_key: key_path.to_string_lossy().into_owned(),
        base_url: Some(stub.base_url.clone()),
        ..TagsClassifierConfig::default()
    };
    let jev = Jev::new(shipped_canon(), TagMapping::default(), &cfg);
    assert!(jev.classify(&note()).is_err());
    assert_eq!(stub.hit_count(), 0);
}

/// Success criterion: empty text sends nothing. Author tags still land.
#[test]
fn empty_text_sends_no_request() {
    let stub = Stub::start(vec![Canned::json(200, fixture_body())]);
    let (jev, _dir) = jev_for(&stub, shipped_canon(), 30);

    let out = jev
        .classify(&TagInput {
            title: "A title",
            text: "  \n\t ",
            candidates: &[],
        })
        .expect("empty text is not an error");
    assert!(out.tags.is_empty());
    assert_eq!(out.confidence, Confidence::Low);

    let authored = [TagCandidate::new("rust", CandidateSource::Author)];
    let out = jev
        .classify(&TagInput {
            title: "A title",
            text: "",
            candidates: &authored,
        })
        .expect("empty text is not an error");
    assert_eq!(out.tags, vec!["rust".to_string()]);
    assert_eq!(out.confidence, Confidence::High);

    assert_eq!(stub.hit_count(), 0, "zero requests for empty text");
}

// --------------------------------------------------------------- config

/// Inverts the old token-env default: the default names the TypeSafe key's
/// variable and the `jev-latest` model.
#[test]
fn the_default_config_names_the_typesafe_key_and_jev_latest() {
    let cfg = TagsClassifierConfig::default();
    assert_eq!(cfg.api_key, "ESCOTE_TYPESAFE_API_KEY");
    assert_eq!(cfg.model, "jev-latest");
    assert!(cfg.base_url.is_none());
}

#[test]
fn the_config_parses_jev_and_rejects_stale_keys() {
    let cfg: TagsClassifierConfig =
        serde_yaml::from_str("classifier: jev\nthreshold: 0.9\nfallback: deterministic\n").expect("parses");
    assert_eq!(cfg.classifier, ClassifierKind::Jev);
    assert_eq!(cfg.api_key, "ESCOTE_TYPESAFE_API_KEY");

    for stale in ["token-env: SOME_VAR\n", "endpoint: https://example.invalid\n"] {
        let yaml = format!("classifier: jev\n{stale}");
        let err = serde_yaml::from_str::<TagsClassifierConfig>(&yaml).expect_err("a stale key must fail the load");
        let field = stale.split(':').next().expect("field");
        assert!(format!("{err}").contains(field), "the error names {field}: {err}");
    }

    let err = serde_yaml::from_str::<TagsClassifierConfig>("base-url: http://127.0.0.1:1\n")
        .expect_err("base-url is never a yml key");
    assert!(format!("{err}").contains("base-url"), "{err}");
}

#[test]
fn jev_writes_jev_as_its_method() {
    assert_eq!(TagMethod::Jev.as_str(), "jev");
    let cfg = TagsClassifierConfig {
        classifier: ClassifierKind::Jev,
        ..TagsClassifierConfig::default()
    };
    let built = build(&cfg, &shipped_canon(), &TagMapping::default(), None);
    assert_eq!(built.method(), TagMethod::Jev);
}
