//! Structured output for agent-facing commands: yaml for humans on a TTY, json
//! when piped, `--format` to override. Only `sb borg queue` uses it today.

use std::io::IsTerminal;

use clap::ValueEnum;
use eyre::{Context, Result};
use serde::Serialize;

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
#[value(rename_all = "kebab-case")]
pub enum Format {
    Yaml,
    Json,
}

/// The explicit `--format`, else yaml on a TTY and json when piped.
pub fn resolve_format(explicit: Option<Format>, stdout_is_tty: bool) -> Format {
    match explicit {
        Some(format) => format,
        None if stdout_is_tty => Format::Yaml,
        None => Format::Json,
    }
}

/// Render `value` as text ending in exactly one newline.
pub fn render<T: Serialize>(value: &T, format: Format) -> Result<String> {
    log::debug!("output::render: format={format:?}");
    match format {
        Format::Yaml => serde_yaml::to_string(value).context("render yaml"),
        Format::Json => {
            let mut text = serde_json::to_string(value).context("render json")?;
            text.push('\n');
            Ok(text)
        }
    }
}

/// Print `value` to stdout in the resolved format.
pub fn emit<T: Serialize>(value: &T, explicit: Option<Format>) -> Result<()> {
    let format = resolve_format(explicit, std::io::stdout().is_terminal());
    log::debug!("output::emit: explicit={explicit:?} resolved={format:?}");
    print!("{}", render(value, format)?);
    Ok(())
}

#[cfg(test)]
mod tests;
