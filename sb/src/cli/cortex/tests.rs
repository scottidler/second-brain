#![allow(clippy::unwrap_used)]

use super::*;
use clap::Parser;

// Wrap LintArgs in a minimal Parser so we can drive try_parse_from
// without standing up the entire sb CLI tree just to assert clap's
// value-enum validation behaviour.
#[derive(Parser)]
struct LintHarness {
    #[command(flatten)]
    lint: LintArgs,
}

#[derive(Parser)]
struct LinkHarness {
    #[command(flatten)]
    link: LinkArgs,
}

#[derive(Parser)]
struct HubHarness {
    #[command(flatten)]
    hub: HubArgs,
}

#[test]
fn lint_format_rejects_unknown_value() {
    let Err(err) = LintHarness::try_parse_from(["sb", "--format", "yaml"]) else {
        panic!("clap must reject --format yaml at parse time");
    };
    let msg = err.to_string();
    assert!(
        msg.contains("possible values"),
        "expected possible-values hint; got: {msg}"
    );
    assert!(msg.contains("human"), "expected 'human' listed; got: {msg}");
    assert!(msg.contains("json"), "expected 'json' listed; got: {msg}");
}

#[test]
fn lint_format_accepts_human_and_json() {
    let Ok(human) = LintHarness::try_parse_from(["sb", "--format", "human"]) else {
        panic!("--format human must parse");
    };
    assert_eq!(human.lint.format, cortex::opts::LintFormat::Human);
    let Ok(json) = LintHarness::try_parse_from(["sb", "--format", "json"]) else {
        panic!("--format json must parse");
    };
    assert_eq!(json.lint.format, cortex::opts::LintFormat::Json);
}

#[test]
fn lint_format_defaults_to_human() {
    let Ok(parsed) = LintHarness::try_parse_from(["sb"]) else {
        panic!("default parse must succeed");
    };
    assert_eq!(parsed.lint.format, cortex::opts::LintFormat::Human);
}

#[test]
fn link_scan_rejects_unknown_value() {
    let Err(err) = LinkHarness::try_parse_from(["sb", "--scan", "everything"]) else {
        panic!("clap must reject unknown --scan values at parse time");
    };
    let msg = err.to_string();
    assert!(
        msg.contains("possible values"),
        "expected possible-values hint; got: {msg}"
    );
}

#[test]
fn link_scan_accepts_each_variant() {
    for (input, expected) in [
        ("people", cortex::opts::ScanScope::People),
        ("projects", cortex::opts::ScanScope::Projects),
        ("concepts", cortex::opts::ScanScope::Concepts),
        ("all", cortex::opts::ScanScope::All),
    ] {
        let Ok(parsed) = LinkHarness::try_parse_from(["sb", "--scan", input]) else {
            panic!("--scan {input} must parse");
        };
        assert_eq!(parsed.link.scan, expected, "input={input}");
    }
}

#[test]
fn link_scan_defaults_to_all() {
    let Ok(parsed) = LinkHarness::try_parse_from(["sb"]) else {
        panic!("default parse must succeed");
    };
    assert_eq!(parsed.link.scan, cortex::opts::ScanScope::All);
}

// --- entity-hub-two-vector-synthesis Phase 3: --asymmetry CLI wiring ------

#[test]
fn hub_args_default_to_no_flags() {
    let Ok(parsed) = HubHarness::try_parse_from(["sb"]) else {
        panic!("default parse must succeed");
    };
    assert!(!parsed.hub.apply);
    assert!(!parsed.hub.synthesize);
    assert!(!parsed.hub.asymmetry, "--asymmetry defaults to false");
}

#[test]
fn hub_args_asymmetry_flag_parses_and_threads_into_opts() {
    let Ok(parsed) = HubHarness::try_parse_from(["sb", "--asymmetry"]) else {
        panic!("--asymmetry must parse");
    };
    assert!(parsed.hub.asymmetry);
    assert!(!parsed.hub.apply);
    assert!(!parsed.hub.synthesize);

    let opts: cortex::opts::HubOpts = parsed.hub.into();
    assert!(opts.asymmetry, "the flag threads through HubArgs -> HubOpts");
}

#[test]
fn hub_args_asymmetry_combines_with_apply_and_synthesize_at_parse_time() {
    // Nothing here enforces the read-only contract at the parse layer (that is
    // cortex::hub::run's job - --asymmetry is checked first and short-circuits
    // before any write path). Clap just has to accept the combination.
    let Ok(parsed) = HubHarness::try_parse_from(["sb", "--apply", "--synthesize", "--asymmetry"]) else {
        panic!("combined flags must parse");
    };
    assert!(parsed.hub.apply);
    assert!(parsed.hub.synthesize);
    assert!(parsed.hub.asymmetry);
}
