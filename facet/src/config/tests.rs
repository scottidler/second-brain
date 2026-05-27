use super::*;
use std::path::PathBuf;

#[test]
fn default_loads_with_no_file() {
    let cfg = Config::default();
    assert_eq!(cfg.harvest_interval_secs, 86_400);
    assert_eq!(cfg.portrait_interval_secs, 604_800);
    assert!(
        cfg.exclude_cwds
            .iter()
            .any(|p| p.to_string_lossy().contains("tatari-tv"))
    );
    assert_eq!(cfg.llm.cluster_model, "claude-haiku-4-5");
    assert_eq!(cfg.llm.extract_model, "claude-sonnet-4-6");
    assert_eq!(cfg.llm.portrait_model, "claude-opus-4-7");
    assert_eq!(cfg.extract.quote_max_chars, 800);
    assert_eq!(cfg.dormancy.inactive_days, 14);
}

#[test]
fn explicit_path_loads_yaml() {
    let dir = tempfile::tempdir().expect("tempdir");
    let p = dir.path().join("facet.yml");
    std::fs::write(
        &p,
        r#"
harvest-interval-secs: 3600
portrait-interval-secs: 0
exclude-cwds:
  - ~/secret
llm:
  cluster-model: alt-haiku
  extract-model: alt-sonnet
  portrait-model: alt-opus
  per-tick-budget-usd: 1.5
  per-day-budget-usd: 7.0
  fabric-binary: fabric
  timeout-secs: 60
"#,
    )
    .expect("write");
    let cfg = Config::load(Some(&p)).expect("load");
    assert_eq!(cfg.harvest_interval_secs, 3600);
    assert_eq!(cfg.portrait_interval_secs, 0);
    assert_eq!(cfg.llm.cluster_model, "alt-haiku");
    assert!(cfg.exclude_cwds.iter().any(|p| p.to_string_lossy().contains("secret")));
}

#[test]
fn tilde_paths_are_expanded() {
    let dir = tempfile::tempdir().expect("tempdir");
    let p = dir.path().join("facet.yml");
    std::fs::write(&p, "claude-projects-root: ~/.claude/projects\n").expect("write");
    let cfg = Config::load(Some(&p)).expect("load");
    let s = cfg.claude_projects_root.to_string_lossy().to_string();
    assert!(!s.starts_with('~'), "tilde was not expanded: {s}");
}

#[test]
fn tilde_paths_in_include_and_exclude_lists_are_expanded() {
    // Architect round-1 finding: silent work-repo-hygiene failure when a
    // ~/-prefixed path lands in include-cwds/exclude-cwds.
    let dir = tempfile::tempdir().expect("tempdir");
    let p = dir.path().join("facet.yml");
    std::fs::write(
        &p,
        "include-cwds:\n  - ~/repos/scottidler\n  - /absolute/already\nexclude-cwds:\n  - ~/repos/tatari-tv\n",
    )
    .expect("write");
    let cfg = Config::load(Some(&p)).expect("load");
    assert_eq!(cfg.include_cwds.len(), 2);
    for path in cfg.include_cwds.iter().chain(cfg.exclude_cwds.iter()) {
        let s = path.to_string_lossy();
        assert!(!s.starts_with('~'), "tilde was not expanded for list element {s}");
    }
    assert!(
        cfg.exclude_cwds
            .iter()
            .any(|p| p.to_string_lossy().ends_with("tatari-tv"))
    );
}

#[test]
fn is_cwd_eligible_excludes_subpaths() {
    let cfg = Config {
        include_cwds: vec![PathBuf::from("/home/u/repos")],
        exclude_cwds: vec![PathBuf::from("/home/u/repos/tatari-tv")],
        ..Config::default()
    };
    assert!(cfg.is_cwd_eligible(&PathBuf::from("/home/u/repos/scottidler/x")));
    assert!(!cfg.is_cwd_eligible(&PathBuf::from("/home/u/repos/tatari-tv/y")));
    assert!(!cfg.is_cwd_eligible(&PathBuf::from("/elsewhere")));
}

#[test]
fn empty_include_means_all() {
    let cfg = Config {
        include_cwds: vec![],
        exclude_cwds: vec![PathBuf::from("/home/u/repos/tatari-tv")],
        ..Config::default()
    };
    assert!(cfg.is_cwd_eligible(&PathBuf::from("/anything")));
    assert!(!cfg.is_cwd_eligible(&PathBuf::from("/home/u/repos/tatari-tv/y")));
}
