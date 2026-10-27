use super::*;
use crate::cli::checks::Severity;

fn avx512_host() -> CpuFeatures {
    CpuFeatures {
        avx: true,
        avx2: true,
        fma: true,
        f16c: true,
        avx512f: true,
        ..CpuFeatures::default()
    }
}

fn avx_only_host() -> CpuFeatures {
    CpuFeatures {
        avx: true,
        ..CpuFeatures::default()
    }
}

const SMALL: &str = "bge-small-en-v1.5";

fn run(cpu: &CpuFeatures, cortex: BackendChoice, oracle: BackendChoice) -> Vec<Finding> {
    findings_for(cpu, Some(cortex), Some(oracle), Ok(Some(SMALL.to_string())))
}

#[test]
fn capable_host_names_avx512f_and_resolves_onnx() {
    let findings = run(&avx512_host(), BackendChoice::Auto, BackendChoice::Auto);
    assert!(findings[0].message.contains("avx512f"), "{}", findings[0].message);
    assert_eq!(findings[0].severity, Severity::Info);
    let embed = findings
        .iter()
        .find(|f| f.message.starts_with("embed backend"))
        .expect("embed");
    assert!(embed.message.contains("backend: onnx"), "{}", embed.message);
    assert_eq!(embed.severity, Severity::Ok);
    let oracle = findings
        .iter()
        .find(|f| f.message.starts_with("oracle backend"))
        .expect("oracle");
    assert!(oracle.message.contains("backend: onnx"), "{}", oracle.message);
}

#[test]
fn avx_only_host_under_auto_resolves_candle_without_a_warning() {
    let findings = run(&avx_only_host(), BackendChoice::Auto, BackendChoice::Auto);
    let embed = findings
        .iter()
        .find(|f| f.message.starts_with("embed backend"))
        .expect("embed");
    assert!(embed.message.contains("backend: candle"), "{}", embed.message);
    assert!(embed.message.contains("lacks avx2 and fma"), "{}", embed.message);
    assert!(findings.iter().all(|f| f.severity < Severity::Warn));
}

#[test]
fn onnx_pinned_on_an_incapable_cpu_warns_naming_the_key() {
    let findings = run(&avx_only_host(), BackendChoice::Onnx, BackendChoice::Auto);
    let embed = findings
        .iter()
        .find(|f| f.message.starts_with("embed backend"))
        .expect("embed");
    assert_eq!(embed.severity, Severity::Warn);
    assert!(embed.message.contains("lacks avx2 and fma"), "{}", embed.message);
    assert!(embed.suggested_fix.as_deref().unwrap().contains("embed.backend"));
}

#[test]
fn onnx_pinned_on_a_model_without_parity_is_an_error() {
    let findings = findings_for(
        &avx512_host(),
        None,
        Some(BackendChoice::Onnx),
        Ok(Some("bge-base-en-v1.5".to_string())),
    );
    let oracle = findings
        .iter()
        .find(|f| f.message.starts_with("oracle backend"))
        .expect("oracle");
    assert_eq!(oracle.severity, Severity::Error);
    assert!(oracle.message.contains("no ONNX parity proof"), "{}", oracle.message);
}

#[test]
fn pin_finding_reports_parity_and_flags_an_unknown_model() {
    assert!(pin_finding(SMALL).message.contains("onnx parity: yes"));
    assert!(pin_finding("bge-base-en-v1.5").message.contains("onnx parity: no"));
    assert_eq!(pin_finding("mystery").severity, Severity::Warn);
}

#[test]
fn unreadable_pin_is_an_error_and_a_missing_pin_is_info() {
    let findings = findings_for(&avx512_host(), None, None, Err(eyre::eyre!("boom")));
    assert!(
        findings
            .iter()
            .any(|f| f.severity == Severity::Error && f.message.contains("boom"))
    );
    let findings = findings_for(&avx512_host(), None, None, Ok(None));
    assert!(
        findings
            .iter()
            .any(|f| f.severity == Severity::Info && f.message.starts_with("db pin: none"))
    );
}

#[test]
fn skipped_consumers_emit_no_backend_line() {
    let findings = findings_for(&avx512_host(), None, None, Ok(Some(SMALL.to_string())));
    assert!(findings.iter().all(|f| !f.message.contains("backend")));
}

#[test]
fn detected_host_renders_a_cpu_section() {
    let host = CpuFeatures::detect();
    let findings = run(&host, BackendChoice::Auto, BackendChoice::Auto);
    println!("{:#?}", findings.iter().map(|f| &f.message).collect::<Vec<_>>());
    assert!(findings[0].message.starts_with("cpu features:"));
}
