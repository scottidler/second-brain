use super::*;
use crate::eval::judge::AxisScores;

fn key<'a>(fixture: &'a str, chash: &'a str, model: &'a str) -> CacheKey<'a> {
    CacheKey {
        fixture_id: fixture,
        content_hash: chash,
        judge_model: model,
    }
}

fn cj(cc: u8, av: u8, sf: u8, truncated: bool) -> CachedJudgment {
    CachedJudgment {
        scores: AxisScores {
            claim_coverage: cc,
            anchor_validity: av,
            summary_faithfulness: sf,
        },
        truncated,
    }
}

#[test]
fn stable_hash_is_deterministic_and_distinct() {
    assert_eq!(stable_hash("hello"), stable_hash("hello"));
    assert_ne!(stable_hash("hello"), stable_hash("world"));
    // 16 hex chars (64-bit)
    assert_eq!(stable_hash("x").len(), 16);
}

#[test]
fn put_then_get_round_trips_all_axes() {
    let c = JudgmentCache::open_memory().expect("open");
    let k = key("video/a", "ch", "m");
    assert_eq!(c.get(&k).expect("get"), None);
    let j = cj(3, 2, 1, true);
    c.put(&k, j).expect("put");
    assert_eq!(c.get(&k).expect("get"), Some(j));
}

#[test]
fn distinct_keys_do_not_collide() {
    let c = JudgmentCache::open_memory().expect("open");
    c.put(&key("video/a", "ch", "m"), cj(3, 3, 3, false)).expect("put");
    // different content hash -> a miss (source changed => re-judge)
    assert_eq!(c.get(&key("video/a", "OTHER", "m")).expect("get"), None);
    // different model -> a miss
    assert_eq!(c.get(&key("video/a", "ch", "other-model")).expect("get"), None);
}

#[test]
fn put_replaces_existing_row() {
    let c = JudgmentCache::open_memory().expect("open");
    let k = key("article/a", "ch", "");
    c.put(&k, cj(1, 1, 1, false)).expect("put1");
    c.put(&k, cj(2, 2, 2, false)).expect("put2");
    assert_eq!(c.get(&k).expect("get"), Some(cj(2, 2, 2, false)));
}

#[test]
fn claim_verdicts_round_trip_and_miss_on_other_key() {
    use crate::eval::judge::ClaimVerdict::{Contradicted, Supported, Unsupported};
    let cache = JudgmentCache::open_memory().expect("open");
    let k = key("video/a", "h1", "m");
    assert_eq!(cache.get_claims(&k).expect("get"), None);
    cache
        .put_claims(&k, &[Supported, Contradicted, Unsupported])
        .expect("put");
    assert_eq!(
        cache.get_claims(&k).expect("get"),
        Some(vec![Supported, Contradicted, Unsupported])
    );
    assert_eq!(cache.get_claims(&key("video/a", "h2", "m")).expect("get"), None);
}

#[test]
fn empty_claim_verdicts_are_a_hit_not_a_miss() {
    let cache = JudgmentCache::open_memory().expect("open");
    let k = key("idea/a", "h", "m");
    cache.put_claims(&k, &[]).expect("put");
    assert_eq!(cache.get_claims(&k).expect("get"), Some(vec![]));
}

/// `--inject` workers share one cache: concurrent puts and gets from many
/// threads on a file-backed cache never surface `database is locked`.
#[test]
fn one_cache_shared_by_many_threads_writes_without_lock_errors() {
    let tmp = tempfile::tempdir().unwrap();
    let cache = JudgmentCache::open(&tmp.path().join("eval-cache.db")).unwrap();
    let scores = AxisScores {
        claim_coverage: 3,
        anchor_validity: 2,
        summary_faithfulness: 1,
    };
    std::thread::scope(|s| {
        for t in 0..8 {
            let cache = &cache;
            s.spawn(move || {
                for i in 0..100 {
                    let hash = format!("{t}-{i}");
                    let key = CacheKey {
                        fixture_id: "article/x",
                        content_hash: &hash,
                        judge_model: "m",
                    };
                    cache
                        .put(
                            &key,
                            CachedJudgment {
                                scores,
                                truncated: false,
                            },
                        )
                        .unwrap();
                    cache.put_claims(&key, &[ClaimVerdict::Supported]).unwrap();
                    assert_eq!(cache.get(&key).unwrap().unwrap().scores, scores);
                }
            });
        }
    });
}
