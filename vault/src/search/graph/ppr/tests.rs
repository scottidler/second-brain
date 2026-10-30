use super::*;
use crate::search::{Edge, SearchIndex};

const EPSILON: f64 = 1e-4;

fn params(alpha: f64) -> PprParams {
    PprParams {
        alpha,
        epsilon: EPSILON,
        max_pushes: 20_000,
    }
}

fn path(i: usize) -> String {
    format!("notes/{i}.md")
}

/// Six notes, nine weighted undirected edges, every node reachable, mixed
/// kinds and weights, and one pair written in both directions (so its weight
/// counts twice, as `expand_graph` also sees it twice).
const FIXTURE: &[(usize, usize, &str, f32)] = &[
    (0, 1, "wikilink", 1.0),
    (1, 0, "wikilink", 1.0),
    (0, 2, "wikilink", 1.0),
    (1, 2, "semantic", 0.7),
    (2, 3, "wikilink", 1.0),
    (3, 4, "shared-tag", 0.4),
    (4, 5, "wikilink", 1.0),
    (1, 5, "semantic", 0.9),
    (3, 5, "wikilink", 1.0),
];

fn fixture_index() -> SearchIndex {
    let mut index = SearchIndex::open_memory().expect("open");
    for i in 0..6 {
        index
            .insert_test_note_row(&path(i), "article", 1_000 + i as i64)
            .expect("insert note");
    }
    let edges: Vec<Edge> = FIXTURE
        .iter()
        .map(|(s, d, k, w)| Edge::deterministic(path(*s), path(*d), *k, *w))
        .collect();
    let (inserted, skipped) = index.insert_edges(&edges).expect("edges");
    assert_eq!((inserted, skipped), (FIXTURE.len(), 0));
    index
}

/// Dense power iteration for `pi = alpha * s + (1 - alpha) * pi * P`, with
/// `P[u][v] = w(u,v) / d(u)` over the undirected weighted edges and a
/// self-loop for a node with no edge. Iterated far past f64 noise.
fn power_iteration(n: usize, edges: &[(usize, usize, f64)], seeds: &[(usize, f64)], alpha: f64) -> Vec<f64> {
    let mut w = vec![vec![0.0_f64; n]; n];
    for (s, d, wt) in edges {
        w[*s][*d] += wt;
        w[*d][*s] += wt;
    }
    let mut p = vec![vec![0.0_f64; n]; n];
    for u in 0..n {
        let deg: f64 = w[u].iter().sum();
        if deg == 0.0 {
            p[u][u] = 1.0;
        } else {
            for v in 0..n {
                p[u][v] = w[u][v] / deg;
            }
        }
    }
    let total: f64 = seeds.iter().map(|(_, x)| x).sum();
    let mut s = vec![0.0_f64; n];
    for (i, x) in seeds {
        s[*i] += x / total;
    }
    let mut pi = s.clone();
    for _ in 0..10_000 {
        let mut next: Vec<f64> = s.iter().map(|x| alpha * x).collect();
        for u in 0..n {
            for v in 0..n {
                next[v] += (1.0 - alpha) * pi[u] * p[u][v];
            }
        }
        pi = next;
    }
    pi
}

fn score_of(scores: &PprScores, p: &str) -> f64 {
    scores
        .scores
        .iter()
        .find(|(q, _)| q == p)
        .map(|(_, s)| *s)
        .unwrap_or(0.0)
}

/// Push PPR agrees with a dense power-iteration reference within
/// `ppr-epsilon` per unit of weighted degree at every node, for both alphas
/// the eval tunes over.
#[test]
fn ppr_matches_power_iteration() {
    let index = fixture_index();
    let edges: Vec<(usize, usize, f64)> = FIXTURE.iter().map(|(s, d, _, w)| (*s, *d, *w as f64)).collect();
    // Seeds weighted by reciprocal rank, as oracle builds them.
    let seed_ranks = [(0usize, 1.0_f64), (3, 0.5)];
    // Forward push guarantees `pi(v) - p(v) < epsilon * d(v)` (ACL 2006, the
    // residual threshold is per unit of weighted degree), so that is the bound.
    let mut degree = [0.0_f64; 6];
    for (s, d, w) in &edges {
        degree[*s] += w;
        degree[*d] += w;
    }
    let seeds: Vec<(String, f64)> = seed_ranks.iter().map(|(i, w)| (path(*i), *w)).collect();

    for alpha in [0.15, 0.3] {
        let reference = power_iteration(6, &edges, &seed_ranks, alpha);
        assert!(
            (reference.iter().sum::<f64>() - 1.0).abs() < 1e-12,
            "reference is a distribution"
        );
        let got = index
            .personalized_pagerank(&seeds, None, 0.0, &params(alpha))
            .expect("ppr");
        assert!(!got.capped, "alpha {alpha}: the fixture converges well under the cap");
        for (v, want) in reference.iter().enumerate() {
            let have = score_of(&got, &path(v));
            let bound = EPSILON * degree[v];
            assert!(
                (have - want).abs() <= bound,
                "alpha {alpha}: node {v} push {have} vs power iteration {want}, off by {} > ppr-epsilon * d(v) = {bound}",
                (have - want).abs()
            );
            assert!(
                have <= want + 1e-12,
                "alpha {alpha}: push estimate is a lower bound at node {v}"
            );
        }
    }
}

#[test]
fn ppr_scores_sorted_desc_then_path() {
    let index = fixture_index();
    let got = index
        .personalized_pagerank(&[(path(0), 1.0)], None, 0.0, &params(0.15))
        .expect("ppr");
    assert_eq!(got.scores.len(), 6, "every node of the connected fixture scores");
    for pair in got.scores.windows(2) {
        assert!(
            pair[0].1 > pair[1].1 || (pair[0].1 == pair[1].1 && pair[0].0 < pair[1].0),
            "{pair:?} out of order"
        );
    }
}

#[test]
fn ppr_respects_edge_kinds() {
    let index = fixture_index();
    // Wikilinks only: 1-2 and 1-5 are semantic, so node 1's one way out is
    // through node 0, and node 5 is three wikilinks away (1-0-2-3-5).
    let kinds = vec!["wikilink".to_string()];
    let got = index
        .personalized_pagerank(&[(path(1), 1.0)], Some(&kinds), 0.0, &params(0.15))
        .expect("ppr");
    let all = index
        .personalized_pagerank(&[(path(1), 1.0)], None, 0.0, &params(0.15))
        .expect("ppr");
    assert!(
        score_of(&got, &path(5)) < score_of(&all, &path(5)),
        "without the 1-5 semantic edge node 5 is reached only the long way"
    );
}

#[test]
fn ppr_dangling_seed_keeps_its_mass() {
    let mut index = SearchIndex::open_memory().expect("open");
    for i in 0..3 {
        index
            .insert_test_note_row(&path(i), "article", 1_000)
            .expect("insert note");
    }
    index
        .insert_edges(&[Edge::deterministic(path(0), path(1), "wikilink", 1.0)])
        .expect("edges");
    let got = index
        .personalized_pagerank(&[(path(0), 3.0), (path(2), 1.0)], None, 0.0, &params(0.15))
        .expect("ppr");
    assert!((score_of(&got, &path(2)) - 0.25).abs() < 1e-12, "{got:?}");
}

#[test]
fn ppr_push_cap_returns_the_current_estimate() {
    let index = fixture_index();
    let capped = PprParams {
        max_pushes: 1,
        ..params(0.15)
    };
    let got = index
        .personalized_pagerank(&[(path(0), 1.0)], None, 0.0, &capped)
        .expect("a capped run is not an error");
    assert!(got.capped);
    assert_eq!(got.pushes, 1);
    assert_eq!(
        got.scores,
        vec![(path(0), 0.15)],
        "one push from the seed: alpha of its mass"
    );
}

#[test]
fn ppr_empty_seeds_score_nothing() {
    let index = fixture_index();
    let got = index.personalized_pagerank(&[], None, 0.0, &params(0.15)).expect("ppr");
    assert!(got.scores.is_empty());
    assert_eq!(got.pushes, 0);
}

#[test]
fn ppr_rejects_bad_params_and_weights() {
    for bad in [
        PprParams {
            alpha: 0.0,
            ..params(0.15)
        },
        PprParams {
            alpha: 1.0,
            ..params(0.15)
        },
        PprParams {
            epsilon: 0.0,
            ..params(0.15)
        },
        PprParams {
            epsilon: f64::NAN,
            ..params(0.15)
        },
        PprParams {
            max_pushes: 0,
            ..params(0.15)
        },
    ] {
        assert!(bad.validate().is_err(), "{bad:?} must be rejected");
        assert!(forward_push(&[(path(0), 1.0)], &bad, |_| Ok(Vec::new())).is_err());
    }
    assert!(forward_push(&[(path(0), 0.0)], &params(0.15), |_| Ok(Vec::new())).is_err());
    let err = forward_push(&[(path(0), 1.0)], &params(0.15), |_| Ok(vec![(path(1), -1.0)]))
        .expect_err("negative edge weight");
    assert!(err.to_string().contains("traversed edge weights"), "{err}");
}
