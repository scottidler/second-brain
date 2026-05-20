use eyre::Result;

use crate::cli::checks::{Severity, all_sections};

#[derive(clap::Args)]
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
    Ok(())
}
