use std::path::Path;

use eyre::Result;

use crate::config::Config;

/// Refuse an install whose stop budget cannot hold. Validates the config the
/// installed service loads (`config_path` is `vault::paths::cortex_config()`,
/// the path `desired_systemd_unit` pins), not whatever `--config` this install
/// ran with.
pub fn ensure_stop_budget(config_path: &Path) -> Result<()> {
    log::debug!("ensure_stop_budget: config_path={}", config_path.display());
    let service_config = Config::load_service_config(config_path)?;
    match service_config.stop_budget_shortfall() {
        Some(shortfall) => Err(eyre::eyre!("refusing to install cortex.service: {shortfall}")),
        None => Ok(()),
    }
}

#[cfg(test)]
mod tests;
