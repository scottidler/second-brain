use super::*;
use vault::embedding::{BGE_BASE_MODEL_VERSION, BGE_SMALL_MODEL_VERSION, BackendChoice, MockEmbedder};

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

fn config(backend: BackendChoice, workers: usize) -> EmbedConfig {
    EmbedConfig {
        backend,
        workers,
        ..EmbedConfig::default()
    }
}

/// A stand-in for `vault::embedding::load_model_version` that records what it
/// was asked to load and returns a mock.
fn recording_load(
    seen: &mut Vec<(String, Backend, usize)>,
) -> impl FnOnce(&str, Backend, usize, &CpuFeatures) -> Result<Box<dyn EmbeddingModel>> + '_ {
    move |model, backend, threads, _cpu| {
        seen.push((model.to_string(), backend, threads));
        Ok(Box::new(MockEmbedder::new(8, model)))
    }
}

#[test]
fn auto_on_avx2_fma_loads_onnx() {
    let mut seen = Vec::new();
    let loaded = load_embedder(
        &config(BackendChoice::Auto, 0),
        BGE_SMALL_MODEL_VERSION,
        &avx2_fma(),
        recording_load(&mut seen),
    )
    .expect("auto resolves");
    assert_eq!(loaded.backend, Backend::Onnx);
    assert_eq!(seen, vec![(BGE_SMALL_MODEL_VERSION.to_string(), Backend::Onnx, 0)]);
}

#[test]
fn auto_on_avx_only_loads_candle() {
    let mut seen = Vec::new();
    let loaded = load_embedder(
        &config(BackendChoice::Auto, 0),
        BGE_SMALL_MODEL_VERSION,
        &avx_only(),
        recording_load(&mut seen),
    )
    .expect("auto resolves");
    assert_eq!(loaded.backend, Backend::Candle);
    assert_eq!(seen[0].1, Backend::Candle);
}

#[test]
fn auto_without_parity_loads_candle_even_on_capable_cpu() {
    let mut seen = Vec::new();
    let loaded = load_embedder(
        &config(BackendChoice::Auto, 0),
        BGE_BASE_MODEL_VERSION,
        &avx2_fma(),
        recording_load(&mut seen),
    )
    .expect("auto resolves");
    assert_eq!(loaded.backend, Backend::Candle);
}

#[test]
fn workers_reach_the_backend_as_its_thread_count() {
    let mut seen = Vec::new();
    load_embedder(
        &config(BackendChoice::Onnx, 24),
        BGE_SMALL_MODEL_VERSION,
        &avx2_fma(),
        recording_load(&mut seen),
    )
    .expect("onnx pin on a capable CPU");
    assert_eq!(seen[0].2, 24);
}

#[test]
fn onnx_pin_on_avx_only_fails_without_loading() {
    let mut seen = Vec::new();
    let err = match load_embedder(
        &config(BackendChoice::Onnx, 0),
        BGE_SMALL_MODEL_VERSION,
        &avx_only(),
        recording_load(&mut seen),
    ) {
        Ok(_) => panic!("onnx on a CPU lacking AVX2+FMA must fail"),
        Err(e) => format!("{e:#}"),
    };
    assert!(err.contains("lacks avx2 and fma"), "{err}");
    assert!(seen.is_empty(), "no load may run after a failed resolution");
}

#[test]
fn load_failure_is_returned_not_replaced_by_candle() {
    let err = match load_embedder(
        &config(BackendChoice::Auto, 0),
        BGE_SMALL_MODEL_VERSION,
        &avx2_fma(),
        |_, _, _, _| Err(eyre::eyre!("ort exploded")),
    ) {
        Ok(_) => panic!("a failed ONNX load must surface"),
        Err(e) => format!("{e:#}"),
    };
    assert!(err.contains("ort exploded"), "{err}");
    assert!(err.contains("onnx"), "{err}");
}
