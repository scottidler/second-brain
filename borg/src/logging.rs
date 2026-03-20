pub use vault::logging::resolve_log_level;

pub fn setup_logging(log_level: &str) -> eyre::Result<()> {
    vault::logging::setup_logging("borg", log_level)
}
