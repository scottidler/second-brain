#![allow(clippy::unwrap_used)]

use super::*;

fn cpu(avx2: bool, fma: bool) -> CpuFeatures {
    CpuFeatures {
        avx: true,
        avx2,
        fma,
        ..CpuFeatures::default()
    }
}

#[test]
fn require_onnx_capable_accepts_avx2_plus_fma() {
    assert!(require_onnx_capable(&cpu(true, true)).is_ok());
}

#[test]
fn require_onnx_capable_names_each_missing_feature() {
    let err = require_onnx_capable(&cpu(false, true)).unwrap_err().to_string();
    assert!(err.contains("lacks avx2"), "{err}");
    let err = require_onnx_capable(&cpu(true, false)).unwrap_err().to_string();
    assert!(err.contains("lacks fma"), "{err}");
    let err = require_onnx_capable(&cpu(false, false)).unwrap_err().to_string();
    assert!(err.contains("lacks avx2 and fma"), "{err}");
}

/// The gate runs before any ORT call: a missing model file would be an ORT
/// load error, so getting the CPU error proves the gate came first.
#[test]
fn build_session_refuses_an_avx_only_cpu_before_touching_ort() {
    let err = build_session(Path::new("/nonexistent/model.onnx"), 1, &cpu(false, false))
        .unwrap_err()
        .to_string();
    assert!(err.contains("lacks avx2 and fma"), "{err}");
}

#[test]
fn build_session_rejects_zero_threads() {
    let err = build_session(Path::new("/nonexistent/model.onnx"), 0, &cpu(true, true))
        .unwrap_err()
        .to_string();
    assert!(err.contains("intra_threads must be >= 1"), "{err}");
}

#[test]
fn only_ort_warnings_and_errors_are_forwarded() {
    assert_eq!(forwarded_level(LogLevel::Verbose), None);
    assert_eq!(forwarded_level(LogLevel::Info), None);
    assert_eq!(forwarded_level(LogLevel::Warning), Some(log::Level::Warn));
    assert_eq!(forwarded_level(LogLevel::Error), Some(log::Level::Error));
    assert_eq!(forwarded_level(LogLevel::Fatal), Some(log::Level::Error));
}
