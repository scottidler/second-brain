use clap::{Args, Subcommand};
use eyre::{Context, Result};
use std::path::PathBuf;

#[derive(Args)]
pub struct OracleCli {
    /// Path to config file
    #[arg(short, long)]
    pub config: Option<PathBuf>,

    #[command(subcommand)]
    pub command: Commands,
}

#[derive(Subcommand)]
pub enum Commands {
    /// Start the MCP server (stdio transport)
    Serve,

    /// Index the vault into SQLite (or reindex changed files)
    Index {
        /// Reindex every note, ignoring the mtime gate. Use after a schema
        /// change adds columns (e.g. the trace block) so existing rows are
        /// repopulated instead of staying at the column default.
        #[arg(long)]
        force: bool,
    },

    /// Show vault statistics
    Stats,

    /// Call a tool directly (no MCP transport)
    Call {
        /// Tool name (use --list to see available tools)
        #[arg(required_unless_present = "list")]
        tool: Option<String>,
        /// JSON arguments (default: {})
        #[arg(long)]
        json: Option<String>,
        /// List available tool names
        #[arg(long)]
        list: bool,
    },

    /// Measure relevance lift of graph retrieval vs hybrid (design 2026-06-06)
    Eval(EvalArgs),
}

#[derive(Args)]
pub struct EvalArgs {
    /// Path to the eval query set. The default is REPO-RELATIVE
    /// (`config/eval/queries.yml`): `sb oracle eval` is a developer command
    /// meant to run from the second-brain repo root. Pass an absolute path to
    /// run it elsewhere.
    #[arg(long, default_value = "config/eval/queries.yml")]
    pub queries: PathBuf,
    /// Pool/metric depth (e.g. nDCG@K)
    #[arg(long, default_value_t = 10)]
    pub k: u32,
    /// Judge model name (empty = fabric's default model)
    #[arg(long, default_value = "")]
    pub judge_model: String,
    /// Ignore and overwrite cached judgments
    #[arg(long)]
    pub rebuild_cache: bool,
    /// Write a fillable calibration sheet to this path and skip metrics
    #[arg(long)]
    pub emit_calibration: Option<PathBuf>,
    /// Also write the rendered report to this path
    #[arg(long)]
    pub report: Option<PathBuf>,
}

impl From<&EvalArgs> for oracle::eval::EvalOpts {
    fn from(a: &EvalArgs) -> Self {
        Self {
            queries_path: vault::paths::expand_tilde(&a.queries),
            k: a.k,
            judge_model: a.judge_model.clone(),
            rebuild_cache: a.rebuild_cache,
            emit_calibration: a.emit_calibration.as_ref().map(vault::paths::expand_tilde),
        }
    }
}

impl OracleCli {
    pub async fn run(self) -> Result<()> {
        let config = oracle::Config::load(self.config.as_deref()).context("Failed to load configuration")?;
        match self.command {
            Commands::Serve => oracle::serve(config).await,
            Commands::Index { force } => {
                let vault_root = config.vault_root()?;
                println!("Indexing vault: {}", vault_root.display());
                println!("Database: {}", config.db_path().display());
                let stats = oracle::index(&config, force)?;
                println!();
                println!("Scanned:   {}", stats.total_scanned);
                println!("Inserted:  {}", stats.inserted);
                println!("Updated:   {}", stats.updated);
                println!("Unchanged: {}", stats.unchanged);
                println!("Removed:   {}", stats.removed);
                Ok(())
            }
            Commands::Stats => {
                let stats = oracle::stats(&config)?;
                print_vault_stats(&config, &stats);
                Ok(())
            }
            Commands::Call { tool, json, list } => {
                if list {
                    print_tool_list(&oracle::tools());
                    Ok(())
                } else {
                    let tool_name = tool.as_deref().expect("clap enforces tool or --list");
                    let result = oracle::call(config, tool_name, json.as_deref()).await?;
                    print_call_result(&result)
                }
            }
            Commands::Eval(a) => {
                let opts = oracle::eval::EvalOpts::from(&a);
                match oracle::eval::run(&config, &opts)? {
                    oracle::eval::EvalOutcome::CalibrationSheet(path) => {
                        println!("wrote calibration sheet: {}", path.display());
                        println!(
                            "Fill the `human` scores, then copy them into the calibration maps in your queries.yml."
                        );
                    }
                    oracle::eval::EvalOutcome::Report(report) => {
                        let rendered = report.render();
                        print!("{rendered}");
                        if let Some(path) = a.report {
                            std::fs::write(&path, &rendered)
                                .with_context(|| format!("writing report to {}", path.display()))?;
                            println!("\nreport written to {}", path.display());
                        }
                    }
                }
                Ok(())
            }
        }
    }
}

fn print_vault_stats(config: &oracle::Config, stats: &vault::search::VaultStats) {
    match config.vault_root() {
        Ok(root) => println!("Vault: {}", root.display()),
        Err(e) => println!("Vault: (unresolved: {e})"),
    }
    println!("Total notes: {}", stats.total_notes);

    if !stats.schema_gaps.is_empty() {
        println!("\nSchema gaps:");
        for (field, count) in &stats.schema_gaps {
            println!("  missing {field:<10} {count}");
        }
    }

    println!("\nBy domain:");
    for (domain, count) in &stats.by_domain {
        println!("  {domain:<15} {count}");
    }

    println!("\nBy type:");
    for (note_type, count) in &stats.by_type {
        println!("  {note_type:<15} {count}");
    }

    println!("\nBy status:");
    for (status, count) in &stats.by_status {
        println!("  {status:<15} {count}");
    }
}

/// Fallback terminal width when stdout is not a tty (e.g. piped to a file).
const DEFAULT_TERM_WIDTH: usize = 80;
/// Gap between the name column and the description column.
const NAME_COL_GAP: usize = 2;
/// Floor on the description column so a narrow terminal still wraps sanely.
const MIN_DESC_WIDTH: usize = 20;

fn terminal_width() -> usize {
    use std::io::IsTerminal;
    if std::io::stdout().is_terminal() {
        terminal_size::terminal_size().map_or(DEFAULT_TERM_WIDTH, |(w, _)| w.0 as usize)
    } else {
        DEFAULT_TERM_WIDTH
    }
}

/// Greedy word-wrap on whitespace. A word longer than `width` overflows its
/// own line rather than being split.
fn wrap(text: &str, width: usize) -> Vec<String> {
    let mut lines = Vec::new();
    let mut current = String::new();
    for word in text.split_whitespace() {
        if current.is_empty() {
            current.push_str(word);
        } else if current.len() + 1 + word.len() <= width {
            current.push(' ');
            current.push_str(word);
        } else {
            lines.push(std::mem::take(&mut current));
            current.push_str(word);
        }
    }
    if !current.is_empty() {
        lines.push(current);
    }
    lines
}

fn print_tool_list(tools: &[rmcp::model::Tool]) {
    let name_col = tools.iter().map(|t| t.name.len()).max().unwrap_or(0) + NAME_COL_GAP;
    let desc_width = terminal_width().saturating_sub(name_col).max(MIN_DESC_WIDTH);
    for tool in tools {
        let lines = wrap(tool.description.as_deref().unwrap_or(""), desc_width);
        match lines.split_first() {
            Some((first, rest)) => {
                println!("{:<name_col$}{first}", tool.name);
                for line in rest {
                    println!("{:name_col$}{line}", "");
                }
            }
            None => println!("{}", tool.name),
        }
    }
}

/// Pure inspection of a tool result. Returns true if the wrapper should
/// translate this outcome into a non-zero CLI exit code.
///
/// Two failure shapes are recognized:
/// - `is_error == Some(true)`: MCP-level protocol error (invalid args, panic).
/// - Top-level JSON `"found": false`: domain-level "not found" — the tool ran
///   successfully but the requested item doesn't exist.
fn outcome_is_failure(result: &rmcp::model::CallToolResult) -> bool {
    if result.is_error == Some(true) {
        return true;
    }
    for content in &result.content {
        if let Some(text) = content.as_text()
            && let Ok(parsed) = serde_json::from_str::<serde_json::Value>(&text.text)
            && parsed.get("found").and_then(|v| v.as_bool()) == Some(false)
        {
            return true;
        }
    }
    false
}

fn print_call_result(result: &rmcp::model::CallToolResult) -> Result<()> {
    let failure = outcome_is_failure(result);
    let is_protocol_error = result.is_error == Some(true);

    for content in &result.content {
        if let Some(text) = content.as_text() {
            if is_protocol_error {
                eprintln!("{}", text.text);
            } else if let Ok(parsed) = serde_json::from_str::<serde_json::Value>(&text.text) {
                println!("{}", serde_json::to_string_pretty(&parsed)?);
            } else {
                println!("{}", text.text);
            }
        }
    }

    if failure {
        // Already printed above; signal exit-1 to main via the typed marker.
        return Err(crate::error::SilentFailure.into());
    }
    Ok(())
}

#[cfg(test)]
mod tests;
