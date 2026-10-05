use colored::Colorize;
use eyre::Result;

/// Emit each pre-rendered line from the lib boundary. Used by every borg
/// sub-verb that returns `Vec<String>` so sb owns stdout uniformly.
pub(super) fn print_daemon_outcome(outcome: &borg::DaemonOutcome) {
    use borg::DaemonOutcome;
    match outcome {
        DaemonOutcome::Installed { unit_path } => {
            println!("Wrote {}", unit_path.display());
            println!("Service installed and started.");
        }
        DaemonOutcome::Uninstalled { unit_path } => {
            println!("Removed {}", unit_path.display());
            println!("Service uninstalled.");
        }
        DaemonOutcome::NotInstalled { unit_path } => {
            println!("No service file found at {}", unit_path.display());
        }
        DaemonOutcome::Reinstalled { unit_path } => {
            println!("Wrote {}", unit_path.display());
            println!("Service reinstalled and started.");
        }
        DaemonOutcome::Stopped => println!("Stopped borg service"),
        DaemonOutcome::Restarted => println!("Restarted borg service"),
        DaemonOutcome::Status { raw_output } => print!("{raw_output}"),
        DaemonOutcome::NoAction => {
            println!("No daemon action specified. See: sb borg daemon --help");
        }
    }
}

pub(super) fn print_receipt_rows(rows: &[borg::receipts::Receipt]) {
    if rows.is_empty() {
        println!("(no receipts rows match)");
        return;
    }
    println!(
        "{:>3}  {:<24}  {:<10}  {:<9}  {:<10}  {:<19}  trace_id",
        "#", "received_at", "method", "status", "kind", "stage"
    );
    println!("{}", "-".repeat(110));
    for (i, r) in rows.iter().enumerate() {
        let stage = r.failure_stage.as_deref().unwrap_or("-");
        println!(
            "{:>3}  {:<24}  {:<10}  {:<9}  {:<10}  {:<19}  {}",
            i + 1,
            r.received_at,
            r.method,
            r.status,
            r.kind,
            stage,
            r.trace_id
        );
    }
}

pub(super) fn print_receipt_detail(r: &borg::receipts::Receipt) {
    println!("trace_id:       {}", r.trace_id);
    println!("status:         {}", r.status);
    println!("received_at:    {}", r.received_at);
    println!("method:         {}", r.method);
    println!("kind:           {}", r.kind);
    if let Some(t) = &r.terminal_at {
        println!("terminal_at:    {t}");
    }
    if let Some(n) = &r.note_path {
        println!("note_path:      {n}");
    }
    if let Some(s) = &r.failure_stage {
        println!("failure_stage:  {s}");
    }
    if let Some(reason) = &r.failure_reason {
        println!("failure_reason: {reason}");
    }
    if let Some(rep) = &r.replay_of {
        println!("replay_of:      {rep}");
    }
    let preview = vault::text::truncate_with_ellipsis(&r.raw_input, 200);
    println!("raw_input:      {preview}");
}

pub(super) fn print_harvest_report(report: &borg::harvest::HarvestReport) {
    use borg::harvest::watermark::Reappearance;
    use borg::types::IngestStatus;

    let publishable: Vec<_> = report.plan.publishable().collect();

    if report.dry_run {
        println!("DRY RUN - nothing written (no notes, receipts rows, reject artifacts, or watermark advance)");
        println!();
        println!(
            "Would select {} thread(s) -> {} note(s):",
            publishable.len(),
            publishable.len()
        );
        for t in &publishable {
            let kind = match &t.decision {
                Reappearance::NewNote => "new",
                Reappearance::FollowUp { .. } => "follow-up",
                Reappearance::Skip { .. } => "skip",
            };
            println!(
                "  [{kind}] {} ({} session(s), {} msgs)",
                t.primary_id,
                t.member_ids.len(),
                t.total_msgs
            );
        }
        println!();
        println!("Would reject {} candidate(s):", report.plan.rejections.len());
        for r in &report.plan.rejections {
            println!("  {} - {}", r.session_id, r.record.reason);
        }
        if !report.parse_rejections.is_empty() {
            println!();
            println!(
                "Skipped {} unparseable record(s) (would receipt as rejected on a live run):",
                report.parse_rejections.len()
            );
            for r in &report.parse_rejections {
                let id = r.session_id.as_deref().unwrap_or("<unreadable session-id>");
                println!("  {id} (index {}) - {}", r.index, r.reason);
            }
        }
        return;
    }

    println!("Published {} thread(s):", report.outcomes.len());
    for o in &report.outcomes {
        match &o.result.status {
            IngestStatus::Completed => {
                let path = o.result.note_path.as_deref().unwrap_or("(no path)");
                let title = o.result.title.as_deref().unwrap_or("");
                println!("  ok    {} -> {} \"{}\"", o.primary_id, path, title);
            }
            IngestStatus::Duplicate { original_date } => {
                println!("  dup   {} (already ingested {original_date})", o.primary_id);
            }
            IngestStatus::Queued => {
                println!("  queued {}", o.primary_id);
            }
            IngestStatus::Failed { reason } => {
                eprintln!("  FAIL  {} - {reason}", o.primary_id);
            }
        }
    }
    println!();
    println!(
        "Rejected {} candidate(s); skipped {} unparseable record(s); cursor -> {}",
        report.plan.rejections.len(),
        report.parse_rejections.len(),
        report.plan.new_cursor
    );
}

/// Render a `sb borg dedupe-sessions` outcome. Dry-run and apply share the
/// same shape, differing only in verb tense, so the operator can review the
/// EXACT plan (survivor + tombstoned set per trace) before approving `--apply`.
pub(super) fn print_dedupe_report(r: &borg::dedupe::DedupeReport) {
    let mode = if r.applied { "APPLY" } else { "DRY-RUN" };
    println!("dedupe-sessions: {mode}");
    if r.groups.is_empty() {
        println!("No duplicate trace groups found.");
    } else {
        let tombstone_verb = if r.applied { "tombstoned" } else { "would tombstone" };
        println!("{} duplicate trace group(s):", r.groups.len());
        for g in &r.groups {
            println!("  trace {}", g.trace);
            println!("    survivor:     {}", g.survivor.display());
            for t in &g.tombstoned {
                println!("    {tombstone_verb}: {}", t.display());
            }
        }
    }

    println!();
    let backfill_verb = if r.applied { "backfilled" } else { "would backfill" };
    println!(
        "harvest-body-hash backfill: {} {} note(s)",
        backfill_verb,
        r.backfill.backfilled.len()
    );
    for p in &r.backfill.backfilled {
        println!("  {}", p.display());
    }
    if !r.backfill.uncovered.is_empty() {
        println!(
            "  {} note(s) have no surviving staging - harvest-body-hash left absent:",
            r.backfill.uncovered.len()
        );
        for p in &r.backfill.uncovered {
            println!("    {}", p.display());
        }
    }

    if let Some(purge) = &r.purge {
        println!();
        let purge_verb = if r.applied { "archived" } else { "would archive" };
        println!("purge: {purge_verb} {} tombstone(s)", purge.archived.len());
        for p in &purge.archived {
            println!("  {}", p.display());
        }
        if !purge.refused.is_empty() {
            println!(
                "purge refused {} tombstone(s) with a live inbound link:",
                purge.refused.len()
            );
            for (p, sources) in &purge.refused {
                println!("  {} <- linked from:", p.display());
                for s in sources {
                    println!("      {}", s.display());
                }
            }
        }
    }
}

pub(super) fn print_replay_event(event: &borg::replay::ReplayEvent) {
    use borg::replay::ReplayEvent;
    match event {
        ReplayEvent::BootstrapHeader {
            note_path,
            source,
            method,
        } => {
            println!("bootstrap: {} -> {source} (method: {method})", note_path.display());
        }
        ReplayEvent::TraceHeader { trace_id, source } => {
            println!("replay trace {trace_id}: {source}");
        }
        ReplayEvent::MatchingHeader { count } => {
            println!("replay: {count} matching trace(s)");
        }
        ReplayEvent::NoMatches => {
            println!("replay: no traces matched");
        }
        ReplayEvent::DryRunBootstrap { source } => {
            println!("  [dry-run] would re-ingest {source}");
        }
        ReplayEvent::DryRunTrace => {
            println!("  [dry-run] would re-ingest via daemon");
        }
        ReplayEvent::ResultOk { title } => {
            println!("  -> {}", title.as_deref().unwrap_or("(no title)"));
        }
        ReplayEvent::ResultDuplicate { original_date } => {
            println!("  -> duplicate (originally ingested {original_date})");
        }
        ReplayEvent::ResultFailed { reason } => {
            println!("  -> failed: {reason}");
        }
        ReplayEvent::ResultQueued => {
            println!("  -> queued");
        }
        ReplayEvent::ResultOther { description } => {
            println!("  -> {description}");
        }
        ReplayEvent::MatchingItemError { trace_id, error } => {
            println!("  trace {trace_id}: {error}");
        }
    }
}

pub(super) fn print_audit_event(event: &borg::audit::AuditEvent) {
    use borg::audit::AuditEvent;
    match event {
        AuditEvent::FixStart { count } => {
            println!();
            println!("Fixing {count} finding(s)...");
        }
        AuditEvent::Fixed {
            rel_path,
            expected_type,
        } => {
            println!("  Fixed: {} -> type: {expected_type}", rel_path.display());
        }
        AuditEvent::FixError { path, error } => {
            println!("  Error fixing {}: {error}", path.display());
        }
        AuditEvent::NothingFixable => {
            println!();
            println!("No fixable findings for the requested kinds.");
        }
        AuditEvent::RowDropped { source, date } => {
            println!("  Dropped \u{1F504} ledger row: {date}  {source}");
        }
        AuditEvent::NoteRemoved { rel_path, source } => {
            println!("  rkvr rmrf: {} ({source})", rel_path.display());
        }
        AuditEvent::Quarantined {
            source,
            kept,
            quarantined,
        } => {
            println!("  Quarantined {} dup(s) for source: {source}", quarantined.len());
            println!("    kept: {}", kept.display());
            for q in quarantined {
                println!("    moved: {}", q.display());
            }
        }
        AuditEvent::DuplicateReported { count } => {
            println!(
                "  {count} notes share identical content (report only; run --fix duplicate-quarantine to quarantine)"
            );
        }
        AuditEvent::DuplicateNotEligible { count, reason } => {
            println!("  {count} notes hash alike but failed the quarantine second-proof ({reason}); not moved");
        }
        AuditEvent::RkvrUnavailable { path, error } => {
            eprintln!("  rkvr unavailable for {}: {error}", path.display());
        }
        AuditEvent::CreatorSet { rel_path, creator } => {
            println!("  set creator: {creator} on {}", rel_path.display());
        }
    }
}

pub(super) fn print_audit_summary(report: &borg::audit::AuditReport, fix: bool) {
    if report.no_ledger {
        println!("No Borg Ledger found at {}", report.ledger_path.display());
        return;
    }
    println!("Auditing Borg Ledger: {}", report.ledger_path.display());
    println!("Vault: {}", report.vault_root.display());
    println!("Found {} completed ledger entries to audit", report.entries_scanned);
    println!();

    if report.findings.is_empty() {
        println!("No issues found.");
        return;
    }

    let mut mistype_count = 0;
    let mut blocked_count = 0;
    let mut raw_title_count = 0;
    let mut duplicate_count = 0;
    let mut orphan_count = 0;
    let mut github_creator_count = 0;
    for finding in &report.findings {
        match finding {
            borg::audit::AuditFinding::Mistype { .. } => mistype_count += 1,
            borg::audit::AuditFinding::Blocked { .. } => blocked_count += 1,
            borg::audit::AuditFinding::RawTitle { .. } => raw_title_count += 1,
            borg::audit::AuditFinding::Duplicate { .. } => duplicate_count += 1,
            borg::audit::AuditFinding::OrphanReplace { .. } => orphan_count += 1,
            borg::audit::AuditFinding::GithubCreatorMissing { .. } => github_creator_count += 1,
        }
    }

    println!("Audit Results:");
    if mistype_count > 0 {
        println!("  {mistype_count} misclassified types");
    }
    if blocked_count > 0 {
        println!("  {blocked_count} blocked content saved as completed");
    }
    if raw_title_count > 0 {
        println!("  {raw_title_count} raw URL titles");
    }
    if duplicate_count > 0 {
        println!("  {duplicate_count} duplicate note pairs");
    }
    if orphan_count > 0 {
        println!("  {orphan_count} orphaned replacements (replaced but no new ✅)");
    }
    if github_creator_count > 0 {
        println!("  {github_creator_count} github notes missing a creator (repo owner)");
    }

    println!();
    println!("Details:");
    for finding in &report.findings {
        println!("  {finding}");
    }

    if !fix {
        let total =
            mistype_count + blocked_count + raw_title_count + duplicate_count + orphan_count + github_creator_count;
        println!();
        println!("Run with --fix to address all {total} finding(s), or --fix <kinds...> to target specific classes.");
        println!("  Kinds: mistype | orphan-replace | blocked | raw-title | duplicate | github-creator-missing");
    }
}

pub(super) fn print_migrate_report(r: &borg::migrate::MigrateReport) {
    let mode = if r.apply { "APPLY" } else { "DRY-RUN" };
    println!("Migration mode: {mode}");
    println!("Vault: {}", r.vault_root.display());
    println!("Found {} markdown files to check", r.files_scanned);
    for rel in &r.changed {
        println!("  {mode}: {rel}");
    }
    if r.seeded_ledger > 0 {
        println!("Seeded Borg Ledger with {} entries.", r.seeded_ledger);
    }
    println!();
    println!("{mode} complete: {} files would be changed", r.changed.len());
    if !r.apply && !r.changed.is_empty() {
        println!("Run with --apply to write changes.");
    }
}

pub(super) fn print_reingest_failed_event(event: &borg::migrate::ReingestFailedEvent) {
    use borg::migrate::ReingestFailedEvent;
    match event {
        ReingestFailedEvent::NoMatches => {
            println!("reingest-failed: no failed-fetch notes found");
        }
        ReingestFailedEvent::Dispatching { source } => println!("  -> {source}"),
        ReingestFailedEvent::Ok { title } => {
            println!("     ok: {}", title.as_deref().unwrap_or("(no title)"));
        }
        ReingestFailedEvent::Duplicate => println!("     duplicate (unchanged)"),
        ReingestFailedEvent::Failed { reason } => println!("     failed: {reason}"),
        ReingestFailedEvent::Queued => println!("     queued"),
        ReingestFailedEvent::ParseError { path, error } => {
            println!("     {}: response parse error: {error}", path.display());
        }
        ReingestFailedEvent::HttpError { path, error } => {
            println!("     {}: HTTP error: {error}", path.display());
        }
    }
}

pub(super) fn print_reingest_failed_report(r: &borg::migrate::ReingestFailedReport) {
    if r.matched.is_empty() {
        return; // NoMatches event already printed
    }
    let mode = if r.dry_run { "[dry-run] " } else { "" };
    // For apply mode the dispatching events already streamed; we only
    // print the summary header + path list at the end (matches the
    // pre-refactor output where the list preceded the per-item lines).
    if r.dry_run {
        println!("{mode}reingest-failed: {} matching note(s)", r.matched.len());
        for (path, source) in &r.matched {
            let rel = path.strip_prefix(&r.vault_root).unwrap_or(path);
            println!("  {}  <- {}", rel.display(), source);
        }
    }
}

/// Render the daemon startup banner from the typed snapshot. Lines that
/// previously went to stdout/stderr go through here so the lib stays
/// stdout-clean.
pub(super) fn print_server_banner(s: &borg::ServerStartup) {
    use borg::SubsystemStatus;

    let arrow = "-->".to_string();
    match &s.telegram {
        SubsystemStatus::Active => println!("{} telegram notifier active", arrow.green()),
        SubsystemStatus::SkippedNoToken => {
            eprintln!("{} telegram notifier skipped (token not available)", arrow.yellow())
        }
        _ => {}
    }

    match &s.desktop {
        SubsystemStatus::Active => println!("{} desktop notifier active", arrow.green()),
        SubsystemStatus::SkippedHostMismatch => {
            eprintln!("{} desktop notifier skipped (host mismatch)", arrow.yellow())
        }
        _ => {}
    }

    println!("{} http server on {}", arrow.green(), s.addr.to_string().cyan());

    match &s.telegram_bot {
        SubsystemStatus::Active => println!("{} telegram bot active", arrow.green()),
        SubsystemStatus::SkippedHostMismatch => {
            eprintln!("{} telegram bot skipped (host mismatch)", arrow.yellow())
        }
        SubsystemStatus::SkippedNoToken => {
            eprintln!("{} telegram bot skipped (token not available)", arrow.yellow())
        }
        _ => {}
    }

    match &s.discord {
        SubsystemStatus::Active => println!("{} discord bot active", arrow.green()),
        SubsystemStatus::SkippedHostMismatch => {
            eprintln!("{} discord bot skipped (host mismatch)", arrow.yellow())
        }
        SubsystemStatus::SkippedNoToken => {
            eprintln!("{} discord bot skipped (token not available)", arrow.yellow())
        }
        _ => {}
    }

    match &s.ntfy {
        SubsystemStatus::ActiveWithDetail(detail) => {
            println!("{} ntfy subscriber active ({})", arrow.green(), detail)
        }
        SubsystemStatus::Active => println!("{} ntfy subscriber active", arrow.green()),
        SubsystemStatus::SkippedHostMismatch => {
            eprintln!("{} ntfy subscriber skipped (host mismatch)", arrow.yellow())
        }
        _ => {}
    }

    if matches!(s.watchdog, SubsystemStatus::Active) {
        println!("{} watchdog active", arrow.green());
    }
}

/// Format and emit a borg `IngestOutcome`. Failed outcomes write to stderr
/// and exit with code 1 to preserve the prior shell contract.
pub(super) fn print_ingest_outcome(outcome: &borg::IngestOutcome) -> Result<()> {
    match outcome {
        borg::IngestOutcome::Captured { title, path } => {
            println!("Captured: \"{title}\" -> {path}");
        }
        borg::IngestOutcome::Duplicate { original_date } => {
            println!("Duplicate: already ingested on {original_date}");
        }
        borg::IngestOutcome::Queued => {
            println!("Queued for processing.");
        }
        borg::IngestOutcome::Failed { reason } => {
            eprintln!("Error: {reason}");
            // Already printed; signal exit-1 to main via the typed marker.
            return Err(crate::error::SilentFailure.into());
        }
    }
    Ok(())
}
