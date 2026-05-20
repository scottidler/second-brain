use eyre::Result;

use crate::cli::checks::{Section, all_sections};

#[derive(clap::Args)]
pub struct StatusArgs {}

pub fn run(_args: StatusArgs) -> Result<()> {
    let sections = all_sections();
    print_report(&sections);
    Ok(())
}

fn print_report(sections: &[Section]) {
    for section in sections {
        println!("[{}]", section.name);
        for f in &section.findings {
            println!("  {} {}", f.severity.icon(), f.message);
        }
        println!();
    }
}
