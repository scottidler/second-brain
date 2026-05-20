use eyre::{Result, eyre};

use crate::cli::checks::{Finding, Severity, all_sections};

/// `sb doctor` exit-code contract:
///
/// - Exit 0 when there are no findings, or only `Ok`/`Info`/`Warn` findings.
///   Warnings are advisory, not blocking - same convention as `rustup doctor`
///   and `brew doctor`.
/// - Exit 1 only when at least one `Severity::Error` finding is present.
///
/// The human-readable "Issues detected" summary continues to print whenever a
/// `Warn`-or-worse finding is present, so warn-level operators still see a
/// nudge.
#[derive(clap::Args)]
#[command(long_about = "Run the same health checks as `sb status`, tagged with severity.\n\
                  \n\
                  Exits 1 only when an Error-severity issue is detected; warnings exit 0 \
                  (matches `rustup doctor` / `brew doctor`).")]
pub struct DoctorArgs {}

pub fn run(_args: DoctorArgs) -> Result<()> {
    let sections = all_sections();
    let mut all: Vec<_> = sections
        .into_iter()
        .flat_map(|s| s.findings.into_iter().map(move |f| (s.name, f)))
        .collect();
    // Errors first, then warnings, then info, then ok.
    all.sort_by(|a, b| b.1.severity.cmp(&a.1.severity));

    let mut had_issue = false;
    for (section, f) in &all {
        if f.severity >= Severity::Warn {
            had_issue = true;
        }
        println!("{} [{section}] {}", f.severity.icon(), f.message);
        if let Some(fix) = &f.suggested_fix {
            println!("    -> {fix}");
        }
    }

    if had_issue {
        println!();
        println!("\u{26a0}\u{fe0f}  Issues detected. See suggested fixes above.");
    }

    evaluate_findings(all.iter().map(|(_, f)| f))
}

/// Pure evaluation of findings into the contract-defined exit decision.
/// Returns `Err` when any `Severity::Error` finding is present.
fn evaluate_findings<'a, I>(findings: I) -> Result<()>
where
    I: IntoIterator<Item = &'a Finding>,
{
    let error_count = findings.into_iter().filter(|f| f.severity == Severity::Error).count();
    if error_count > 0 {
        return Err(eyre!(
            "doctor found {error_count} error-severity issue(s); see output above"
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests;
