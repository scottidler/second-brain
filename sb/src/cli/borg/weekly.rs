//! `sb borg eval --weekly`: the CLI shell over `borg::eval::weekly`. One
//! clustering call, one report file, one Telegram message.

use eyre::{Context, Result};

use borg::eval::weekly::{self, cluster::FabricClusterer};
use borg::notify::{Announced, Telegram};

use super::args;

/// The clustering model: `eval.report.model`, else the default judge (a vendor
/// `llm.api-keys` already carries a key for), else fabric's own default.
fn clustering_model(config: &borg::config::Config) -> String {
    config
        .eval
        .report
        .model
        .clone()
        .or_else(|| config.eval.default_judge_model().map(str::to_string))
        .unwrap_or_default()
}

pub(super) async fn run(config: &borg::config::Config, opts: &borg::eval::EvalOpts, a: &args::EvalArgs) -> Result<()> {
    borg::eval::backlog::preflight_keys(&opts.api_keys)?;
    let receipts = borg::receipts::open_default().context("opening the receipts db")?;
    let cache_path = vault::paths::borg_eval_cache_path();
    let store = borg::eval::scores::ScoreStore::open(&cache_path)?;
    let trust_rows = borg::eval::trust::TrustStore::open(&cache_path)?.all()?;
    let reports_dir = vault::paths::borg_eval_reports_dir();
    let staged_root = config.staging.root.clone();
    let inputs = weekly::Inputs {
        receipts: &receipts,
        store: &store,
        staged_root: &staged_root,
        eval: &config.eval,
        trust_rows: &trust_rows,
        reports_dir: &reports_dir,
    };

    let model = clustering_model(config);
    let clusterer = FabricClusterer {
        binary: "fabric".to_string(),
        model: model.clone(),
        api_keys: opts.api_keys.clone(),
        pattern: borg::eval::pattern_in(opts.patterns_dir.as_deref(), "eval-cluster-failures")?,
    };
    let today = chrono::Local::now().date_naive();
    let report = weekly::build(&inputs, &clusterer, &model, today)?;
    let path = report.write(&reports_dir)?;
    let rendered = report.render();
    print!("{rendered}");
    if let Some(copy) = &a.report {
        std::fs::write(copy, &rendered).with_context(|| format!("writing report to {}", copy.display()))?;
        println!("\nreport copy written to {}", copy.display());
    }
    println!("\nreport written to {}", path.display());
    notify(config, &report.message(&path)).await;
    Ok(())
}

/// Send the run's one summary. The report is already on disk, so a missing
/// or failing Telegram sink is said on stderr, never a failed run.
async fn notify(config: &borg::config::Config, message: &str) {
    let Some(tg_config) = &config.telegram else {
        eprintln!("telegram is not configured in borg.yml: no summary sent");
        return;
    };
    let token = match borg::config::resolve_secret(&tg_config.bot_token) {
        Ok(token) => token,
        Err(e) => {
            eprintln!("telegram bot token does not resolve: no summary sent ({e:#})");
            return;
        }
    };
    let Some(telegram) = Telegram::new(&token, tg_config) else {
        eprintln!("telegram has no notification chat: no summary sent");
        return;
    };
    match borg::notify::announce(&telegram, message).await {
        Announced::Sent => eprintln!("telegram summary sent"),
        Announced::Suppressed => eprintln!("notifications are disabled (BORG_DISABLE_NOTIFICATIONS): no summary sent"),
        Announced::Failed(e) => eprintln!("telegram summary NOT sent: {e}"),
    }
}
