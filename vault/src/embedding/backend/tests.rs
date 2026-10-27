use super::*;
use crate::embedding::{BGE_BASE_MODEL_VERSION, BGE_SMALL_MODEL_VERSION};

fn avx2_fma() -> CpuFeatures {
    CpuFeatures {
        avx: true,
        avx2: true,
        fma: true,
        ..CpuFeatures::default()
    }
}

fn avx_only() -> CpuFeatures {
    CpuFeatures {
        avx: true,
        ..CpuFeatures::default()
    }
}

/// The design doc's resolution table, one row per case. `Ok` carries the
/// expected backend; `Err` carries a substring the error must contain.
#[test]
fn resolve_backend_table() {
    let parity = BGE_SMALL_MODEL_VERSION;
    let no_parity = BGE_BASE_MODEL_VERSION;
    let rows: [(BackendChoice, CpuFeatures, &str, std::result::Result<Backend, &str>); 7] = [
        (BackendChoice::Auto, avx2_fma(), parity, Ok(Backend::Onnx)),
        (BackendChoice::Auto, avx2_fma(), no_parity, Ok(Backend::Candle)),
        (BackendChoice::Auto, avx_only(), parity, Ok(Backend::Candle)),
        (BackendChoice::Candle, avx2_fma(), parity, Ok(Backend::Candle)),
        (BackendChoice::Onnx, avx_only(), parity, Err("lacks avx2 and fma")),
        (BackendChoice::Onnx, avx2_fma(), no_parity, Err("bge-base-en-v1.5")),
        (BackendChoice::Onnx, avx2_fma(), parity, Ok(Backend::Onnx)),
    ];
    for (i, (requested, cpu, model, expected)) in rows.into_iter().enumerate() {
        let got = resolve_backend(requested, &cpu, model);
        match expected {
            Ok(backend) => assert_eq!(got.expect("row must resolve"), backend, "row {i}"),
            Err(needle) => {
                let err = got.expect_err("row must fail").to_string();
                assert!(err.contains(needle), "row {i}: {err}");
            }
        }
    }
}

#[test]
fn candle_pin_ignores_cpu_and_model() {
    for cpu in [avx2_fma(), avx_only(), CpuFeatures::default()] {
        for model in [BGE_SMALL_MODEL_VERSION, BGE_BASE_MODEL_VERSION, "not-a-model"] {
            assert_eq!(
                resolve_backend(BackendChoice::Candle, &cpu, model).expect("candle"),
                Backend::Candle
            );
        }
    }
}

#[test]
fn onnx_pin_names_only_the_missing_feature() {
    let no_fma = CpuFeatures {
        fma: false,
        ..avx2_fma()
    };
    let err = resolve_backend(BackendChoice::Onnx, &no_fma, BGE_SMALL_MODEL_VERSION)
        .expect_err("fma missing")
        .to_string();
    assert!(err.contains("lacks fma"), "{err}");
    assert!(!err.contains("avx2"), "{err}");
}

#[test]
fn auto_with_unknown_model_falls_to_candle() {
    assert_eq!(
        resolve_backend(BackendChoice::Auto, &avx2_fma(), "not-a-model").expect("auto"),
        Backend::Candle
    );
}

#[test]
fn reason_names_missing_features_or_model() {
    let r = resolution_reason(
        BackendChoice::Auto,
        &avx_only(),
        BGE_SMALL_MODEL_VERSION,
        Backend::Candle,
    );
    assert!(r.contains("avx2 and fma"), "{r}");
    let r = resolution_reason(
        BackendChoice::Auto,
        &avx2_fma(),
        BGE_BASE_MODEL_VERSION,
        Backend::Candle,
    );
    assert!(r.contains(BGE_BASE_MODEL_VERSION), "{r}");
    let r = resolution_reason(
        BackendChoice::Candle,
        &avx2_fma(),
        BGE_SMALL_MODEL_VERSION,
        Backend::Candle,
    );
    assert_eq!(r, "pinned in config");
}

#[test]
fn backend_choice_parses_case_insensitively() {
    for (raw, want) in [
        ("auto", BackendChoice::Auto),
        ("AUTO", BackendChoice::Auto),
        ("Candle", BackendChoice::Candle),
        ("ONNX", BackendChoice::Onnx),
        ("onnx", BackendChoice::Onnx),
    ] {
        let got: BackendChoice = serde_yaml::from_str(raw).expect("parse");
        assert_eq!(got, want, "{raw}");
    }
}

#[test]
fn backend_choice_unknown_value_fails_and_names_the_variants() {
    let err = serde_yaml::from_str::<BackendChoice>("fastembed")
        .expect_err("unknown backend must fail")
        .to_string();
    assert!(err.contains("fastembed"), "{err}");
    assert!(err.contains("auto, candle, onnx"), "{err}");
}

#[test]
fn inference_config_defaults_to_auto_and_zero_threads() {
    let cfg: InferenceConfig = serde_yaml::from_str("{}").expect("empty block");
    assert_eq!(cfg, InferenceConfig::default());
    assert_eq!(cfg.backend, BackendChoice::Auto);
    assert_eq!(cfg.threads, 0);
}

#[test]
fn inference_config_rejects_unknown_key() {
    let err = serde_yaml::from_str::<InferenceConfig>("backend: auto\nworkers: 4\n")
        .expect_err("typo'd key must fail")
        .to_string();
    assert!(err.contains("workers"), "{err}");
}

#[test]
fn resolve_with_parity_takes_the_parity_column_from_the_caller() {
    // A model id outside the embedding registry (a cross-encoder) resolves on
    // the caller's parity bit, not the embedding registry's.
    let id = "some-cross-encoder";
    assert_eq!(
        resolve_with_parity(BackendChoice::Auto, &avx2_fma(), id, true).expect("auto"),
        Backend::Onnx
    );
    assert_eq!(
        resolve_with_parity(BackendChoice::Auto, &avx2_fma(), id, false).expect("auto"),
        Backend::Candle
    );
    let err = resolve_with_parity(BackendChoice::Onnx, &avx2_fma(), id, false)
        .expect_err("no parity")
        .to_string();
    assert!(err.contains(id), "{err}");
    // resolve_backend is the same table with the embedding registry's bit.
    assert_eq!(
        resolve_backend(BackendChoice::Auto, &avx2_fma(), BGE_SMALL_MODEL_VERSION).expect("auto"),
        resolve_with_parity(BackendChoice::Auto, &avx2_fma(), BGE_SMALL_MODEL_VERSION, true).expect("auto"),
    );
}
