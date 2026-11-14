//! Spherical k-means over unit-norm rows, wrapping `e-means`.
//!
//! [`train`] fits centroids; [`nearest`] is the one function that assigns a row to a centroid, and
//! it labels every row [`train`] returns. `Model::assign` is never called: one assignment function
//! for everything this system stores, so train-time and later assignments cannot disagree on a
//! near-tie. Runs on the caller's rayon pool; this module builds none.

use e_means::EMeans;
use log::debug;

/// How far a row's L2 norm may sit from 1.0 before [`train`] refuses it.
pub const NORM_TOLERANCE: f32 = 1e-3;

#[derive(Debug, PartialEq, thiserror::Error)]
pub enum KmeansError {
    #[error("d must be at least 1")]
    ZeroDimension,
    #[error("{len} values is not a whole number of rows of d={d}")]
    RaggedRows { len: usize, d: usize },
    #[error("row {row} column {column} is {value}, not finite")]
    NonFinite { row: usize, column: usize, value: f32 },
    #[error("row {row} has norm {norm}, not within {NORM_TOLERANCE} of 1.0")]
    NotUnit { row: usize, norm: f32 },
    #[error("e-means: {0}")]
    Fit(String),
}

/// A finished run: `centroids` is `k * d` row-major; `labels[i]` and `cosines[i]` are
/// [`nearest`] of row `i` over those centroids.
#[derive(Debug, Clone, PartialEq)]
pub struct Trained {
    pub centroids: Vec<f32>,
    pub labels: Vec<u32>,
    pub cosines: Vec<f32>,
}

/// Index of the centroid with the largest dot product with `row`, and that dot product. The lowest
/// index wins a tie. `centroids` is `k * d` row-major.
pub fn nearest(row: &[f32], centroids: &[f32], d: usize) -> (u32, f32) {
    let mut best = (0u32, f32::NEG_INFINITY);
    for (index, centroid) in centroids.chunks_exact(d).enumerate() {
        let dot: f32 = row.iter().zip(centroid).map(|(a, b)| a * b).sum();
        if dot > best.1 {
            best = (index as u32, dot);
        }
    }
    best
}

/// Spherical k-means of `rows` (`n * d` row-major, every row unit-norm) into `k` clusters.
///
/// Every row is checked finite and unit-norm before training, because `e-means` does not
/// normalize and would answer wrongly in silence.
pub fn train(rows: &[f32], d: usize, k: usize, seed: u64, epochs: f64) -> Result<Trained, KmeansError> {
    debug!(
        "kmeans::train: values={} d={d} k={k} seed={seed} epochs={epochs}",
        rows.len()
    );
    validate(rows, d)?;
    let model = EMeans::new(d, k)
        .epochs(epochs)
        .seed(seed)
        .spherical(true)
        .capture_labels(false)
        .train(rows)
        .map_err(|e| KmeansError::Fit(e.to_string()))?;
    let centroids = model.centroids().to_vec();
    let (labels, cosines) = rows.chunks_exact(d).map(|row| nearest(row, &centroids, d)).unzip();
    debug!("kmeans::train: {} rows labelled", rows.len() / d);
    Ok(Trained {
        centroids,
        labels,
        cosines,
    })
}

fn validate(rows: &[f32], d: usize) -> Result<(), KmeansError> {
    if d == 0 {
        return Err(KmeansError::ZeroDimension);
    }
    if !rows.len().is_multiple_of(d) {
        return Err(KmeansError::RaggedRows { len: rows.len(), d });
    }
    for (row, values) in rows.chunks_exact(d).enumerate() {
        if let Some(column) = values.iter().position(|v| !v.is_finite()) {
            return Err(KmeansError::NonFinite {
                row,
                column,
                value: values[column],
            });
        }
        let norm = values.iter().map(|v| v * v).sum::<f32>().sqrt();
        if (norm - 1.0).abs() > NORM_TOLERANCE {
            return Err(KmeansError::NotUnit { row, norm });
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests;
