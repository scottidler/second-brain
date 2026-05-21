use super::*;
use eyre::eyre;

#[test]
fn pipeline_error_carries_stage_and_source() {
    let pe = PipelineError::new(FailureStage::FetchFailed, eyre!("yt-dlp 403"));
    assert_eq!(pe.stage, FailureStage::FetchFailed);
    assert!(pe.source.to_string().contains("yt-dlp 403"));
}

#[test]
fn pipeline_error_display_includes_stage() {
    let pe = PipelineError::new(FailureStage::QualityBlocked, eyre!("blocked"));
    let s = format!("{pe}");
    assert!(s.contains("quality-blocked"), "got {s}");
    assert!(s.contains("blocked"), "got {s}");
}

#[test]
fn pipeline_error_converts_to_report_with_stage_context() {
    let pe = PipelineError::new(FailureStage::PublishFailed, eyre!("write_atomic"));
    let report: eyre::Report = pe.into();
    let chain = format!("{report:?}");
    assert!(chain.contains("publish-failed"), "got {chain}");
}
