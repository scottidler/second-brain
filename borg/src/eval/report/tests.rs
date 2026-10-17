use super::*;

fn s(cc: u8, av: u8, sf: u8) -> AxisScores {
    AxisScores {
        claim_coverage: cc,
        anchor_validity: av,
        summary_faithfulness: sf,
    }
}

#[test]
fn aggregate_means_and_composite() {
    let k = KindReport::aggregate("video", &[s(3, 3, 3), s(1, 1, 1)]);
    assert_eq!(k.n, 2);
    assert!((k.claim_coverage - 2.0).abs() < 1e-9);
    assert!((k.composite - 2.0).abs() < 1e-9);
}

#[test]
fn aggregate_empty_is_zero() {
    let k = KindReport::aggregate("empty", &[]);
    assert_eq!(k.n, 0);
    assert_eq!(k.composite, 0.0);
}

#[test]
fn calibration_panel_none_without_labels() {
    assert!(calibration_panel(&[]).is_none());
}

#[test]
fn calibration_panel_trust_gate() {
    // perfect agreement -> P/R 1.0 -> trustworthy
    let good = calibration_panel(&[(3, 3), (2, 2), (0, 0)]).expect("panel");
    assert!(good.trustworthy);
    // judge misses every human hit -> recall 0 -> below the gate
    let bad = calibration_panel(&[(3, 0), (2, 0), (3, 1)]).expect("panel");
    assert!(!bad.trustworthy);
}

/// Minimal `EvalReport` builder for render tests: fills the judge-axis fields
/// with a stub two-fixture aggregate and lets the caller override the
/// deterministic-metrics fields under test.
fn stub_report(
    listicle: Vec<ListicleMetric>,
    listicle_aggregate: Option<f64>,
    note_size: Vec<NoteSizeMetric>,
) -> EvalReport {
    EvalReport {
        judge_model: String::new(),
        total_fixtures: 2,
        total_judgments: 2,
        new_judgments: 2,
        truncated_judgments: 0,
        fallback_fixtures: 1,
        kinds: vec![KindReport::aggregate("article", &[s(2, 3, 3)])],
        overall: KindReport::aggregate(crate::eval::OVERALL_LABEL, &[s(2, 3, 3)]),
        calibration: None,
        listicle,
        listicle_aggregate,
        note_size,
        judged: true,
        new_claim_judgments: 0,
        grounding: vec![],
        tools_grounding: vec![],
        artifacts: vec![],
        claim_support: vec![],
        claim_support_overall: ClaimSupport::empty(crate::eval::OVERALL_LABEL),
        flagged_claims: vec![],
        traces: vec![],
    }
}

#[test]
fn render_contains_kinds_overall_and_uncalibrated_note() {
    let report = stub_report(vec![], None, vec![]);
    let text = report.render();
    assert!(text.contains("distillation eval"));
    assert!(text.contains("article"));
    assert!(text.contains("ALL"));
    assert!(text.contains("UNCALIBRATED"));
}

#[test]
fn render_shows_na_when_no_fixture_is_listicle_applicable() {
    let text = stub_report(vec![], None, vec![]).render();
    assert!(text.contains("listicle-survival: N/A"));
}

#[test]
fn render_shows_listicle_aggregate_and_per_fixture_rows() {
    let listicle = vec![ListicleMetric {
        fixture: "video/top-10-claude-code-skills-plugins-clis-april-2026".to_string(),
        score: 1.0,
    }];
    let text = stub_report(listicle, Some(1.0), vec![]).render();
    assert!(text.contains("listicle-survival: 1.000  (1 applicable fixture)"));
    assert!(text.contains("video/top-10-claude-code-skills-plugins-clis-april-2026"));
}

#[test]
fn render_reports_note_size_pass_count_and_failing_fixtures() {
    let note_size = vec![
        NoteSizeMetric {
            fixture: "video/a".to_string(),
            rendered_bytes: 2_000,
            within_ceiling: true,
        },
        NoteSizeMetric {
            fixture: "video/b".to_string(),
            rendered_bytes: 100_000,
            within_ceiling: false,
        },
    ];
    let text = stub_report(vec![], None, note_size).render();
    assert!(text.contains("note-size: 1/2 within the 65536-byte ceiling"));
    assert!(text.contains("FAIL video/b"));
    assert!(!text.contains("FAIL video/a"));
}

/// Byte offset where the trailing value column starts on each line that
/// contains `marker`, so tests can assert every row's value lines up.
fn value_columns(text: &str, marker: &str, value_suffix: &str) -> Vec<usize> {
    text.lines()
        .filter(|l| l.contains(marker) && l.ends_with(value_suffix))
        .map(|l| {
            l.trim_end_matches(value_suffix)
                .rfind(' ')
                .expect("value column is space-separated")
                + 1
        })
        .collect()
}

#[test]
fn render_aligns_listicle_scores_past_the_old_sixty_char_floor() {
    let long = "video/a-fixture-slug-that-runs-well-past-the-old-sixty-character-column-floor";
    assert!(long.len() > 60);
    let listicle = vec![
        ListicleMetric {
            fixture: "video/short".to_string(),
            score: 0.5,
        },
        ListicleMetric {
            fixture: long.to_string(),
            score: 1.0,
        },
    ];
    let text = stub_report(listicle, Some(0.75), vec![]).render();
    assert!(text.contains(&format!("    {long} 1.000")));
    let cols = value_columns(&text, "video/", "0");
    assert_eq!(cols.len(), 2);
    assert_eq!(cols[0], cols[1]);
}

#[test]
fn render_aligns_failing_note_size_bytes_past_the_old_fifty_five_char_floor() {
    let long = "video/a-failing-fixture-slug-well-past-the-old-fifty-five-char-floor";
    assert!(long.len() > 55);
    let note_size = vec![
        NoteSizeMetric {
            fixture: "video/b".to_string(),
            rendered_bytes: 100_000,
            within_ceiling: false,
        },
        NoteSizeMetric {
            fixture: long.to_string(),
            rendered_bytes: 200_000,
            within_ceiling: false,
        },
    ];
    let text = stub_report(vec![], None, note_size).render();
    assert!(text.contains(&format!("FAIL {long} 200000 bytes")));
    let cols = value_columns(&text, "FAIL ", " bytes");
    assert_eq!(cols.len(), 2);
    assert_eq!(cols[0], cols[1]);
}

#[test]
fn render_keeps_the_twelve_wide_kind_column_for_short_kinds() {
    let text = stub_report(vec![], None, vec![]).render();
    assert!(text.contains(&format!("{:<12} {:>4}", "kind", "n")));
    assert!(text.contains(&"-".repeat(60)));
}

#[test]
fn render_widens_the_kind_column_for_a_kind_past_twelve_chars() {
    let long = "custom-long-fixture-kind";
    assert!(long.len() > 12);
    let mut report = stub_report(vec![], None, vec![]);
    report.kinds.push(KindReport::aggregate(long, &[s(2, 3, 3)]));
    let text = report.render();
    let header = text.lines().find(|l| l.starts_with("kind")).expect("header row");
    let row = text.lines().find(|l| l.starts_with(long)).expect("long kind row");
    let overall = text.lines().find(|l| l.starts_with("ALL")).expect("overall row");
    assert_eq!(header.len(), row.len());
    assert_eq!(row.len(), overall.len());
    assert!(row.starts_with(&format!("{long} ")));
}

#[test]
fn render_reports_quote_grounding_per_kind_and_low_fixtures() {
    let mut r = stub_report(vec![], None, vec![]);
    r.grounding = vec![
        GroundingMetric {
            fixture: "video/a".into(),
            kind: "video".into(),
            grounded: 3,
            quoted: 4,
        },
        GroundingMetric {
            fixture: "article/b".into(),
            kind: "article".into(),
            grounded: 2,
            quoted: 2,
        },
    ];
    let text = r.render();
    assert!(text.contains("quote-grounding: 5/6"), "{text}");
    assert!(text.contains("video      3/4 (75.0%)"), "{text}");
    assert!(text.contains("LOW video/a 3/4"), "{text}");
    assert!(!text.contains("LOW article/b"), "{text}");
}

#[test]
fn render_reports_claim_support_and_puts_contradicted_first() {
    let mut r = stub_report(vec![], None, vec![]);
    let mut video = ClaimSupport::empty("video");
    video.add(ClaimVerdict::Supported);
    video.add(ClaimVerdict::Unsupported);
    video.add(ClaimVerdict::Contradicted);
    video.add(ClaimVerdict::Supported);
    r.claim_support_overall = ClaimSupport::sum(crate::eval::OVERALL_LABEL, std::slice::from_ref(&video));
    r.claim_support = vec![video];
    r.flagged_claims = vec![
        FlaggedClaim {
            fixture: "video/a".into(),
            verdict: ClaimVerdict::Unsupported,
            text: "an over-reach".into(),
        },
        FlaggedClaim {
            fixture: "video/z".into(),
            verdict: ClaimVerdict::Contradicted,
            text: "wrong number".into(),
        },
    ];
    let text = r.render();
    assert!(text.contains("50.0%"), "{text}");
    let contradicted = text.find("CONTRADICTED video/z").expect("contradicted row");
    let unsupported = text.find("unsupported  video/a").expect("unsupported row");
    assert!(contradicted < unsupported, "{text}");
}

#[test]
fn no_judge_render_omits_axis_tables_and_keeps_deterministic() {
    let mut r = stub_report(vec![], None, vec![]);
    r.judged = false;
    r.artifacts = vec![ArtifactMetric {
        fixture: "article/x".into(),
        reasons: vec!["tldr: placeholder".into()],
    }];
    let text = r.render();
    assert!(text.contains("judges skipped"), "{text}");
    assert!(!text.contains("composite"), "{text}");
    assert!(!text.contains("claim support"), "{text}");
    assert!(text.contains("placeholder-artifacts: 1 fixture"), "{text}");
    assert!(text.contains("quote-grounding: N/A"), "{text}");
}

#[test]
fn claim_support_rate_is_none_without_claims() {
    assert_eq!(ClaimSupport::empty("x").support_rate(), None);
}

#[test]
fn render_lists_only_listicle_shortfalls_past_the_list_limit() {
    let listicle: Vec<ListicleMetric> = (0..=LIST_LIMIT)
        .map(|i| ListicleMetric {
            fixture: format!("video/v{i:02}"),
            score: if i == 3 { 0.5 } else { 1.0 },
        })
        .collect();
    let text = stub_report(listicle, Some(0.97), vec![]).render();
    assert!(text.contains("video/v03 0.500"), "{text}");
    assert!(!text.contains("video/v04"), "{text}");
}

#[test]
fn render_prints_tools_grounding_totals_per_kind_and_low_rows() {
    let mut report = stub_report(vec![], None, vec![]);
    report.tools_grounding = vec![
        ToolsGroundingMetric {
            fixture: "repo/a".to_string(),
            kind: "repo".to_string(),
            counts: calc::ToolsGrounding {
                names_grounded: 2,
                names: 2,
                commands_grounded: 1,
                commands: 2,
            },
        },
        ToolsGroundingMetric {
            fixture: "video/b".to_string(),
            kind: "video".to_string(),
            counts: calc::ToolsGrounding {
                names_grounded: 1,
                names: 1,
                commands_grounded: 0,
                commands: 0,
            },
        },
    ];
    let text = report.render();
    assert!(
        text.contains("  tools-grounding: names 3/3 (100.0%), commands 1/2 (50.0%)  (2 fixtures)\n"),
        "{text}"
    );
    assert!(
        text.contains("    repo       names 2/2 (100.0%), commands 1/2 (50.0%)\n"),
        "{text}"
    );
    assert!(
        text.contains("    video      names 1/1 (100.0%), commands 0/0 (N/A)\n"),
        "{text}"
    );
    assert!(text.contains("    LOW repo/a "), "{text}");
    assert!(!text.contains("LOW video/b"), "{text}");
}

#[test]
fn render_prints_tools_grounding_na_without_tools() {
    let text = stub_report(vec![], None, vec![]).render();
    assert!(
        text.contains("  tools-grounding: N/A (no note renders a tool)\n"),
        "{text}"
    );
}
