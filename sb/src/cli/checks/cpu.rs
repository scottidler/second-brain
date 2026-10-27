//! Doctor check: the CPU's inference features and the backend each consumer
//! resolves to on it.

use vault::cpu::CpuFeatures;
use vault::embedding::{Backend, BackendChoice, model_spec, resolve_backend};

use super::Finding;

/// The `cpu` section: detected features, the backend cortex's embed loop and
/// oracle each resolve for the pinned model, and the DB pin.
///
/// This reports the resolution, not a load: it runs the same decision table
/// the loaders run (`vault::embedding::resolve_backend`) on the same detected
/// CPU and the same config, so it names the backend the next model load takes.
/// It never loads a model (a multi-second, memory-heavy act for a health
/// check) and reads the pin through a read-only connection, so it neither
/// migrates nor creates the search DB. The loaders' own INFO line
/// (`inference backend: requested=.. resolved=..`) is the record of what a
/// running process actually loaded. A config that does not load is reported
/// by the config section, so that consumer is skipped here.
pub(super) fn findings() -> Vec<Finding> {
    let cortex_choice = cortex::config::Config::load_service_config(&vault::paths::cortex_config())
        .ok()
        .map(|cfg| cfg.embed.backend);
    let oracle_choice = oracle::config::Config::load(None).ok().map(|cfg| cfg.inference.backend);
    let pin = vault::search::SearchIndex::peek_active_model(&vault::paths::oracle_db_path());
    findings_for(&CpuFeatures::detect(), cortex_choice, oracle_choice, pin)
}

fn findings_for(
    cpu: &CpuFeatures,
    cortex_choice: Option<BackendChoice>,
    oracle_choice: Option<BackendChoice>,
    pin: eyre::Result<Option<String>>,
) -> Vec<Finding> {
    log::debug!("findings_for: cpu={cpu:?} cortex={cortex_choice:?} oracle={oracle_choice:?}");
    let mut findings = vec![features_finding(cpu)];

    let pin = match pin {
        Ok(Some(model)) => {
            findings.push(pin_finding(&model));
            Some(model)
        }
        Ok(None) => {
            findings.push(Finding::info(
                "db pin: none (no search DB or no active_model row yet)".to_string(),
            ));
            None
        }
        Err(e) => {
            findings.push(Finding::error(
                format!("db pin: unreadable ({e})"),
                "check the search DB with sb oracle stats".to_string(),
            ));
            None
        }
    };
    let model = pin.as_deref().unwrap_or(vault::embedding::ACTIVE_MODEL_VERSION);

    for (consumer, choice, key) in [
        ("embed", cortex_choice, "embed.backend in cortex.yml"),
        ("oracle", oracle_choice, "inference.backend in oracle.yml"),
    ] {
        if let Some(choice) = choice {
            findings.push(backend_finding(consumer, choice, key, cpu, model));
        }
    }
    findings
}

fn features_finding(cpu: &CpuFeatures) -> Finding {
    let present: Vec<&str> = [
        ("avx", cpu.avx),
        ("avx2", cpu.avx2),
        ("fma", cpu.fma),
        ("f16c", cpu.f16c),
        ("avx512f", cpu.avx512f),
        ("avx512_vnni", cpu.avx512_vnni),
        ("avx512_bf16", cpu.avx512_bf16),
        ("avx_vnni", cpu.avx_vnni),
    ]
    .into_iter()
    .filter_map(|(name, on)| on.then_some(name))
    .collect();
    let list = if present.is_empty() { "none".to_string() } else { present.join(" ") };
    let verdict = if cpu.onnx_capable() {
        "onnx capable".to_string()
    } else {
        format!("onnx incapable, lacks {}", cpu.missing_for_onnx().join(" and "))
    };
    Finding::info(format!("cpu features: {list} ({verdict})"))
}

fn pin_finding(model: &str) -> Finding {
    match model_spec(model) {
        Some(spec) => Finding::ok(format!(
            "db pin: {model} (onnx parity: {})",
            if spec.onnx_parity { "yes" } else { "no" }
        )),
        None => Finding::warn(
            format!("db pin: {model} is not in the model registry"),
            "a legacy `-candle` id is renamed when cortex or oracle on this build opens the search DB; \
             otherwise sb cortex embed --model bge-small-en-v1.5"
                .to_string(),
        ),
    }
}

fn backend_finding(consumer: &str, choice: BackendChoice, key: &str, cpu: &CpuFeatures, model: &str) -> Finding {
    match resolve_backend(choice, cpu, model) {
        Ok(backend) => Finding::ok(format!(
            "{consumer} backend: {} (requested {choice}; {})",
            backend_name(backend),
            vault::embedding::backend::resolution_reason(choice, cpu, model, backend)
        )),
        Err(e) if choice == BackendChoice::Onnx && !cpu.onnx_capable() => Finding::warn(
            format!("{consumer} backend: onnx is pinned but this CPU cannot run it ({e})"),
            format!("set {key} to auto or candle"),
        ),
        Err(e) => Finding::error(
            format!("{consumer} backend: cannot resolve ({e})"),
            format!("set {key} to auto or candle"),
        ),
    }
}

fn backend_name(backend: Backend) -> &'static str {
    backend.as_str()
}

#[cfg(test)]
mod tests;
