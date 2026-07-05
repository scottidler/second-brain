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
