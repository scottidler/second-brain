use super::*;

fn key<'a>(qid: &'a str, qhash: &'a str, note: &'a str, chash: &'a str, model: &'a str) -> CacheKey<'a> {
    CacheKey {
        query_id: qid,
        query_hash: qhash,
        note_path: note,
        content_hash: chash,
        judge_model: model,
    }
}

#[test]
fn stable_hash_is_deterministic_and_distinct() {
    assert_eq!(stable_hash("hello"), stable_hash("hello"));
    assert_ne!(stable_hash("hello"), stable_hash("world"));
}

#[test]
fn put_then_get_round_trips() {
    let c = JudgmentCache::open_memory().expect("open");
    let k = key("q1", "qh", "notes/a.md", "ch", "m");
    assert_eq!(c.get(&k).expect("get"), None);
    c.put(
        &k,
        CachedJudgment {
            score: 2,
            truncated: false,
        },
    )
    .expect("put");
    assert_eq!(
        c.get(&k).expect("get"),
        Some(CachedJudgment {
            score: 2,
            truncated: false
        })
    );
}

#[test]
fn changed_query_hash_misses_cache() {
    let c = JudgmentCache::open_memory().expect("open");
    c.put(
        &key("q1", "qh-old", "notes/a.md", "ch", "m"),
        CachedJudgment {
            score: 3,
            truncated: false,
        },
    )
    .expect("put");
    // same id + note + content + model, but the query TEXT changed (new hash)
    assert_eq!(c.get(&key("q1", "qh-new", "notes/a.md", "ch", "m")).expect("get"), None);
}

#[test]
fn changed_content_hash_misses_cache() {
    let c = JudgmentCache::open_memory().expect("open");
    c.put(
        &key("q1", "qh", "notes/a.md", "ch-old", "m"),
        CachedJudgment {
            score: 1,
            truncated: false,
        },
    )
    .expect("put");
    assert_eq!(c.get(&key("q1", "qh", "notes/a.md", "ch-new", "m")).expect("get"), None);
}

#[test]
fn put_replaces_on_same_key() {
    let c = JudgmentCache::open_memory().expect("open");
    let k = key("q1", "qh", "notes/a.md", "ch", "m");
    c.put(
        &k,
        CachedJudgment {
            score: 1,
            truncated: true,
        },
    )
    .expect("put");
    c.put(
        &k,
        CachedJudgment {
            score: 3,
            truncated: false,
        },
    )
    .expect("put2");
    assert_eq!(
        c.get(&k).expect("get"),
        Some(CachedJudgment {
            score: 3,
            truncated: false
        })
    );
}
