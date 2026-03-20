pub fn resolve_log_level(cli_level: Option<&str>, config_level: &str) -> String {
    vault::logging::resolve_log_level(cli_level, Some(config_level))
}

pub fn setup_logging(log_level: &str) -> eyre::Result<()> {
    vault::logging::setup_logging("cortex", log_level)
}
