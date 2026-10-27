//! Which inference backend serves an embedding model on this host.
//!
//! One `sb` binary carries both backends. Config names a [`BackendChoice`]
//! (`auto | candle | onnx`); [`resolve_backend`] turns it, the detected
//! [`CpuFeatures`] and the model id into the concrete [`Backend`]. The
//! resolution is pure so its whole decision table is a unit test with an
//! injected CPU. `docs/design/2026-10-27-cpu-tier-inference-backend.md`.

use eyre::{Result, eyre};
use serde::{Deserialize, Deserializer};

use super::model_spec;
use crate::cpu::CpuFeatures;

/// The backend an operator asks for in config (`embed.backend` in cortex.yml,
/// `inference.backend` in oracle.yml).
///
/// `Auto` is the fallback policy: ONNX when the CPU and the model allow it,
/// Candle otherwise. `Candle` and `Onnx` are explicit pins with no fallback.
/// Deserialization is case-insensitive; an unknown value fails config load.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum BackendChoice {
    #[default]
    Auto,
    Candle,
    Onnx,
}

impl BackendChoice {
    /// The config spelling.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Auto => "auto",
            Self::Candle => "candle",
            Self::Onnx => "onnx",
        }
    }

    /// Parse a config value, ignoring case. `None` for anything else.
    pub fn parse(value: &str) -> Option<Self> {
        match value.to_ascii_lowercase().as_str() {
            "auto" => Some(Self::Auto),
            "candle" => Some(Self::Candle),
            "onnx" => Some(Self::Onnx),
            _ => None,
        }
    }
}

impl<'de> Deserialize<'de> for BackendChoice {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> std::result::Result<Self, D::Error> {
        let raw = String::deserialize(deserializer)?;
        Self::parse(&raw).ok_or_else(|| {
            serde::de::Error::custom(format!(
                "unknown inference backend {raw:?}; valid values are: auto, candle, onnx"
            ))
        })
    }
}

impl std::fmt::Display for BackendChoice {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// The backend that actually runs inference.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Backend {
    Candle,
    Onnx,
}

impl Backend {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Candle => "candle",
            Self::Onnx => "onnx",
        }
    }
}

impl std::fmt::Display for Backend {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// oracle.yml's `inference:` block: the backend and thread count for query
/// embedding.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize)]
#[serde(default, rename_all = "kebab-case", deny_unknown_fields)]
pub struct InferenceConfig {
    pub backend: BackendChoice,
    /// Inference threads. `0` is the backend's default.
    pub threads: usize,
}

/// Whether ONNX may serve `model`: the registry entry exists and carries an
/// ONNX parity proof.
fn has_onnx_parity(model: &str) -> bool {
    model_spec(model).is_some_and(|spec| spec.onnx_parity)
}

/// Resolve the requested backend for `model` on a CPU with `cpu`'s features.
///
/// | requested | `onnx_capable()` | ONNX parity | result |
/// |---|---|---|---|
/// | `auto` | yes | yes | `Onnx` |
/// | `auto` | yes | no | `Candle` |
/// | `auto` | no | any | `Candle` |
/// | `candle` | any | any | `Candle` |
/// | `onnx` | no | any | error naming the missing features |
/// | `onnx` | yes | no | error naming the model |
/// | `onnx` | yes | yes | `Onnx` |
pub fn resolve_backend(requested: BackendChoice, cpu: &CpuFeatures, model: &str) -> Result<Backend> {
    match requested {
        BackendChoice::Candle => Ok(Backend::Candle),
        BackendChoice::Auto => Ok(if cpu.onnx_capable() && has_onnx_parity(model) {
            Backend::Onnx
        } else {
            Backend::Candle
        }),
        BackendChoice::Onnx => {
            let missing = cpu.missing_for_onnx();
            if !missing.is_empty() {
                return Err(eyre!(
                    "inference backend `onnx` is pinned but this CPU lacks {}; \
                     set the backend to `auto` or `candle`",
                    missing.join(" and ")
                ));
            }
            if !has_onnx_parity(model) {
                return Err(eyre!(
                    "inference backend `onnx` is pinned but model {model:?} has no ONNX parity proof; \
                     set the backend to `auto` or `candle`"
                ));
            }
            Ok(Backend::Onnx)
        }
    }
}

/// Why `requested` resolved to `resolved` for `model`, in words for the INFO
/// line: names the missing CPU features or the model when `auto` fell to
/// Candle.
pub fn resolution_reason(requested: BackendChoice, cpu: &CpuFeatures, model: &str, resolved: Backend) -> String {
    match (requested, resolved) {
        (BackendChoice::Auto, Backend::Candle) => {
            let missing = cpu.missing_for_onnx();
            if missing.is_empty() {
                format!("model {model} has no ONNX parity proof")
            } else {
                format!("CPU lacks {}", missing.join(" and "))
            }
        }
        (BackendChoice::Auto, Backend::Onnx) => "CPU has avx2 and fma, model has an ONNX parity proof".to_string(),
        _ => "pinned in config".to_string(),
    }
}

/// [`resolve_backend`], then one INFO line naming the request, the result and
/// the reason. Callers run it once per model load, which is once per process
/// for cortex and once per pinned model for oracle.
pub fn resolve_backend_logged(requested: BackendChoice, cpu: &CpuFeatures, model: &str) -> Result<Backend> {
    let resolved = resolve_backend(requested, cpu, model)?;
    log::info!(
        "inference backend: requested={requested} resolved={resolved} model={model} ({})",
        resolution_reason(requested, cpu, model, resolved)
    );
    Ok(resolved)
}

#[cfg(test)]
mod tests;
