use super::*;

const D: usize = 8;

/// Unit rows of pure noise (no cluster structure), so the seed decides the result.
fn noise_rows(n: usize) -> Vec<f32> {
    let mut state = 0x2545_f491_4f6c_dd1du64;
    let mut next = move || {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        (state >> 40) as f32 / (1u64 << 24) as f32 - 0.5
    };
    let mut rows = Vec::with_capacity(n * D);
    for _ in 0..n {
        let row: Vec<f32> = (0..D).map(|_| next()).collect();
        let norm = row.iter().map(|v| v * v).sum::<f32>().sqrt();
        rows.extend(row.iter().map(|v| v / norm));
    }
    rows
}

fn bytes(values: &[f32]) -> Vec<u8> {
    values.iter().flat_map(|v| v.to_le_bytes()).collect()
}

#[test]
fn same_seed_gives_identical_centroid_bytes() {
    let rows = noise_rows(300);
    let a = train(&rows, D, 5, 7, 3.0).unwrap();
    let b = train(&rows, D, 5, 7, 3.0).unwrap();
    assert_eq!(bytes(&a.centroids), bytes(&b.centroids));
    assert_eq!(a.labels, b.labels);
}

#[test]
fn different_seeds_give_different_centroid_bytes() {
    let rows = noise_rows(300);
    let a = train(&rows, D, 5, 7, 3.0).unwrap();
    let b = train(&rows, D, 5, 8, 3.0).unwrap();
    assert_ne!(bytes(&a.centroids), bytes(&b.centroids));
}

#[test]
fn train_labels_equal_nearest_over_the_returned_centroids() {
    let rows = noise_rows(200);
    let t = train(&rows, D, 6, 1, 3.0).unwrap();
    assert_eq!(t.labels.len(), 200);
    for (i, row) in rows.as_chunks::<D>().0.iter().enumerate() {
        let (label, cosine) = nearest(row, &t.centroids, D);
        assert_eq!(t.labels[i], label);
        assert_eq!(t.cosines[i], cosine);
    }
}

#[test]
fn nearest_breaks_a_tie_to_the_lowest_index() {
    let centroids = [0.0, 1.0, 1.0, 0.0, 0.0, 1.0];
    assert_eq!(nearest(&[0.0, 1.0], &centroids, 2), (0, 1.0));
    assert_eq!(nearest(&[1.0, 0.0], &centroids, 2), (1, 1.0));
}

#[test]
fn non_unit_row_is_a_typed_error() {
    let mut rows = noise_rows(20);
    for v in &mut rows[2 * D..3 * D] {
        *v *= 2.0;
    }
    let err = train(&rows, D, 3, 1, 1.0).unwrap_err();
    assert!(matches!(err, KmeansError::NotUnit { row: 2, .. }), "{err:?}");
    assert!(err.to_string().contains("row 2"));
}

#[test]
fn nan_row_is_a_typed_error() {
    let mut rows = noise_rows(20);
    rows[4 * D + 3] = f32::NAN;
    let err = train(&rows, D, 3, 1, 1.0).unwrap_err();
    assert!(
        matches!(err, KmeansError::NonFinite { row: 4, column: 3, .. }),
        "{err:?}"
    );
}

#[test]
fn ragged_rows_and_too_few_rows_are_typed_errors() {
    let rows = noise_rows(10);
    assert!(matches!(
        train(&rows[..D * 10 - 1], D, 3, 1, 1.0),
        Err(KmeansError::RaggedRows { .. })
    ));
    assert_eq!(train(&rows, 0, 3, 1, 1.0), Err(KmeansError::ZeroDimension));
    assert!(matches!(train(&rows, D, 11, 1, 1.0), Err(KmeansError::Fit(_))));
}

#[test]
fn e_means_version_matches_the_workspace_dependency() {
    let manifest = include_str!("../../../Cargo.toml");
    let line = manifest
        .lines()
        .find(|l| l.trim_start().starts_with("e-means"))
        .expect("workspace Cargo.toml has an e-means line");
    assert!(
        line.contains(&format!("version = \"{E_MEANS_VERSION}\"")),
        "E_MEANS_VERSION {E_MEANS_VERSION} does not match `{line}`"
    );
}

#[test]
fn check_row_accepts_a_unit_row_and_names_a_bad_one() {
    let rows = noise_rows(1);
    assert_eq!(check_row(0, &rows), Ok(()));
    let doubled: Vec<f32> = rows.iter().map(|v| v * 2.0).collect();
    assert!(matches!(
        check_row(9, &doubled),
        Err(KmeansError::NotUnit { row: 9, .. })
    ));
    let mut nan = rows.clone();
    nan[1] = f32::INFINITY;
    assert!(matches!(
        check_row(4, &nan),
        Err(KmeansError::NonFinite { row: 4, column: 1, .. })
    ));
}
