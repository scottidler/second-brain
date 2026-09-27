//! Repeat mean (Phase 6): `repeats` calls per shard, averaged per slug.

use super::*;

/// A classifier.dev that answers every label of every input in a chunk with
/// the same score, drawn from `scores` in call order (cycling), so an
/// average over several calls differs from any single one of them. Can also
/// fail one call by number, to exercise a repeat failing partway through.
struct RepeatingService {
    scores: Vec<f32>,
    /// 1-based call number to answer with a 500 instead of a score.
    fail_on_call: Option<usize>,
    calls: std::sync::Mutex<usize>,
}

impl RepeatingService {
    fn new(scores: Vec<f32>) -> Arc<Self> {
        Arc::new(Self {
            scores,
            fail_on_call: None,
            calls: std::sync::Mutex::new(0),
        })
    }

    fn failing_on(scores: Vec<f32>, fail_on_call: usize) -> Arc<Self> {
        Arc::new(Self {
            scores,
            fail_on_call: Some(fail_on_call),
            calls: std::sync::Mutex::new(0),
        })
    }

    fn call_count(&self) -> usize {
        *self.calls.lock().expect("lock")
    }
}

impl Transport for RepeatingService {
    fn send(&self, body: &serde_json::Value, _token: Option<&str>) -> Result<(u16, String)> {
        let call = {
            let mut n = self.calls.lock().expect("lock");
            *n += 1;
            *n
        };
        if self.fail_on_call == Some(call) {
            return Ok((500, r#"{"error":"upstream"}"#.to_string()));
        }
        let inputs = body["inputs"].as_array().expect("inputs").len();
        let labels: Vec<String> = body["labels"]
            .as_array()
            .expect("labels")
            .iter()
            .map(|v| v.as_str().expect("label").to_string())
            .collect();
        let score = self.scores[(call - 1) % self.scores.len()];
        let scores: serde_json::Map<String, serde_json::Value> =
            labels.iter().map(|l| (l.clone(), serde_json::json!(score))).collect();
        let result = serde_json::json!({"model": "jev-1.13.0", "scores": scores});
        let results: Vec<serde_json::Value> = (0..inputs).map(|_| result.clone()).collect();
        let reply = serde_json::json!({
            "results": results,
            "usage": {"input_tokens": 100 * inputs as u64},
            "pricing": {"estimated_usd": 100.0 * inputs as f64 * 0.042 / 1_000_000.0},
        });
        Ok((200, reply.to_string()))
    }
}

/// Design doc Phase 6 success criterion: `repeats: 3` issues 3 calls per
/// shard (one shard here, `canon()` is 17 labels) and selects on their mean,
/// not on any single call.
#[test]
fn repeats_3_issues_3_calls_per_shard_and_selects_on_the_mean() {
    let scores = vec![0.80, 0.90, 1.00];
    let mean = scores.iter().sum::<f32>() / scores.len() as f32;

    let cfg = TagsClassifierConfig {
        repeats: 3,
        ..TagsClassifierConfig::default()
    };
    let service = RepeatingService::new(scores.clone());
    let dev = ClassifierDev::with_transport(canon(), mapping(), &cfg, service.clone());
    let owned = owned_inputs(1);
    let batch = dev.score_batch(&as_inputs(&owned));

    assert_eq!(service.call_count(), 3, "one call per repeat, one shard");
    let scored = batch.notes[0].as_ref().expect("scored");
    assert!(!scored.is_empty());
    for (label, score) in scored {
        assert!(
            (*score - mean).abs() < 1e-6,
            "label {label} scored {score}, wanted the mean {mean}"
        );
    }

    // Contrast: a single call (repeats: 1) lands on the first score alone,
    // not the mean.
    let single_cfg = TagsClassifierConfig {
        repeats: 1,
        ..TagsClassifierConfig::default()
    };
    let single_dev = ClassifierDev::with_transport(canon(), mapping(), &single_cfg, RepeatingService::new(scores));
    let single_batch = single_dev.score_batch(&as_inputs(&owned));
    let single_scored = single_batch.notes[0].as_ref().expect("scored");
    for score in single_scored.values() {
        assert!((*score - 0.80).abs() < 1e-6, "a single call must not average: {score}");
    }
}

/// P1's success criterion 3, the "later repeat failed" half left UNVERIFIED
/// because repeats did not exist yet: a note whose first repeat of a shard
/// succeeds and second fails is that note's `Err` for the whole batch, never
/// `Ok` with the first repeat's score alone.
#[test]
fn a_failed_later_repeat_is_err_not_a_partial_average() {
    let cfg = TagsClassifierConfig {
        repeats: 3,
        ..TagsClassifierConfig::default()
    };
    let service = RepeatingService::failing_on(vec![0.95, 0.95, 0.95], 2);
    let dev = ClassifierDev::with_transport(canon(), mapping(), &cfg, service.clone());
    let owned = owned_inputs(1);
    let batch = dev.score_batch(&as_inputs(&owned));

    assert_eq!(
        service.call_count(),
        2,
        "the third repeat is never sent once the second already failed the note"
    );
    let err = batch.notes[0]
        .as_ref()
        .expect_err("a later repeat's failure must fail the whole note");
    assert!(err.to_string().contains("500"), "{err}");
}

/// P2's cost estimates and allowance checks multiply by `repeats`: each
/// repeat is a full extra call, so a bulk run's spend prediction
/// (`estimate_batch` / `wire_usd`) must scale the same way `score_batch`'s
/// actual call count does.
#[test]
fn the_estimate_multiplies_by_repeats() {
    let single_cfg = TagsClassifierConfig {
        repeats: 1,
        ..TagsClassifierConfig::default()
    };
    let tripled_cfg = TagsClassifierConfig {
        repeats: 3,
        ..TagsClassifierConfig::default()
    };
    let service = Arc::new(FakeService::default());
    let single = ClassifierDev::with_transport(canon(), mapping(), &single_cfg, service.clone());
    let tripled = ClassifierDev::with_transport(canon(), mapping(), &tripled_cfg, service);
    let owned = owned_inputs(2);

    let single_estimate = single.estimate_batch(&as_inputs(&owned));
    let tripled_estimate = tripled.estimate_batch(&as_inputs(&owned));
    for (one, three) in single_estimate.per_note_usd.iter().zip(&tripled_estimate.per_note_usd) {
        assert!(
            (three - one * 3.0).abs() < 1e-12,
            "repeats must multiply the estimate: single={one} tripled={three}"
        );
    }
}

/// Implementation audit r1 (P6): three identical 0.90 answers averaged in f32
/// came to 0.8999999166 and failed the default 0.9 threshold that the single
/// 0.90 answer passes. The mean of identical scores must select exactly as
/// the score itself does, and a mean genuinely below the threshold must not.
#[test]
fn a_repeat_mean_of_identical_scores_selects_like_the_single_score() {
    let default_threshold = TagsClassifierConfig::default().threshold;
    assert_eq!(default_threshold, 0.9, "this test pins the shipped default");
    let owned = owned_inputs(1);
    let tags_for = |repeats: u8, score: f32| -> Vec<String> {
        let cfg = TagsClassifierConfig {
            repeats,
            ..TagsClassifierConfig::default()
        };
        let dev = ClassifierDev::with_transport(canon(), mapping(), &cfg, RepeatingService::new(vec![score]));
        dev.classify_batch(&as_inputs(&owned))
            .pop()
            .expect("one output")
            .expect("scored")
            .tags
    };

    assert!(!tags_for(1, 0.90).is_empty(), "a single 0.90 meets 0.9");
    assert!(!tags_for(3, 0.90).is_empty(), "the mean of three 0.90s meets 0.9");
    assert!(tags_for(3, 0.89).is_empty(), "the mean of three 0.89s does not");
}

#[test]
fn meets_threshold_absorbs_float_noise_only() {
    let f32_mean = (0.9f32 + 0.9 + 0.9) / 3.0;
    assert!(f32_mean < 0.9, "the pre-fix arithmetic this guards against");
    assert!(meets_threshold(f32_mean, 0.9));
    assert!(meets_threshold(0.9, 0.9));
    assert!(!meets_threshold(0.8999, 0.9));
}
