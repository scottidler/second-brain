//! Runtime CPU feature probe.
//!
//! One `sb` binary carries both inference backends (Candle, no AVX2 floor;
//! ONNX Runtime, which needs AVX2+FMA). The binary is compiled for baseline
//! x86-64, so which backend a host can run is a runtime question, answered
//! here. `docs/design/2026-10-27-cpu-tier-inference-backend.md`.
//!
//! [`CpuFeatures::onnx_capable`] is the SIGILL guard: ORT's prebuilt archive
//! executes AVX2 instructions the first time it parses a model (measured on
//! `talos`, a Sandy Bridge-era CPU), so every ONNX load path checks
//! it before its first `ort` call. The remaining flags are reported, not gated
//! on: ORT's MLAS layer picks its own kernels at runtime.

/// The instruction-set extensions that matter to local inference.
///
/// Fields are public so tests (and the backend-resolution table) can inject a
/// CPU that is not the one running the test.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct CpuFeatures {
    pub avx: bool,
    pub avx2: bool,
    pub fma: bool,
    pub f16c: bool,
    pub avx512f: bool,
    pub avx512_vnni: bool,
    pub avx512_bf16: bool,
    pub avx_vnni: bool,
}

impl CpuFeatures {
    /// Probe the running CPU. Every flag is `false` off x86_64.
    pub fn detect() -> Self {
        let features = probe();
        log::debug!("CpuFeatures::detect: {features:?}");
        features
    }

    /// True when ONNX Runtime can run here: AVX2 and FMA both present.
    pub fn onnx_capable(&self) -> bool {
        self.missing_for_onnx().is_empty()
    }

    /// The ONNX-required feature names this CPU lacks, in a fixed order, so an
    /// error can name exactly what is missing. Empty when [`Self::onnx_capable`].
    pub fn missing_for_onnx(&self) -> Vec<&'static str> {
        [("avx2", self.avx2), ("fma", self.fma)]
            .into_iter()
            .filter_map(|(name, present)| (!present).then_some(name))
            .collect()
    }
}

#[cfg(target_arch = "x86_64")]
fn probe() -> CpuFeatures {
    CpuFeatures {
        avx: std::arch::is_x86_feature_detected!("avx"),
        avx2: std::arch::is_x86_feature_detected!("avx2"),
        fma: std::arch::is_x86_feature_detected!("fma"),
        f16c: std::arch::is_x86_feature_detected!("f16c"),
        avx512f: std::arch::is_x86_feature_detected!("avx512f"),
        avx512_vnni: std::arch::is_x86_feature_detected!("avx512vnni"),
        avx512_bf16: std::arch::is_x86_feature_detected!("avx512bf16"),
        avx_vnni: std::arch::is_x86_feature_detected!("avxvnni"),
    }
}

#[cfg(not(target_arch = "x86_64"))]
fn probe() -> CpuFeatures {
    CpuFeatures::default()
}

#[cfg(test)]
mod tests;
