//! Test-only: capture WARN-and-above log lines process-wide so a test can assert
//! on the WARN a code path emits. Lines carry the path or trace id, so a test
//! filters to its own. A process holds one logger, so every WARN test in a crate
//! shares this one.

use std::sync::Mutex;

struct WarnCapture(Mutex<Vec<String>>);

impl log::Log for WarnCapture {
    fn enabled(&self, meta: &log::Metadata) -> bool {
        meta.level() <= log::Level::Warn
    }
    fn log(&self, record: &log::Record) {
        if self.enabled(record.metadata()) {
            self.0.lock().expect("warn capture").push(format!("{}", record.args()));
        }
    }
    fn flush(&self) {}
}

static WARNS: WarnCapture = WarnCapture(Mutex::new(Vec::new()));

/// Install the capturing logger. `set_logger` fails only when a logger is already
/// installed: ours, from an earlier test in this process. Any other logger would
/// make the WARN assertions fail loudly rather than pass vacuously.
pub fn install() {
    let _ = log::set_logger(&WARNS);
    log::set_max_level(log::LevelFilter::Warn);
}

/// Every captured WARN line containing `needle`.
pub fn warns_containing(needle: &str) -> Vec<String> {
    WARNS
        .0
        .lock()
        .expect("warn capture")
        .iter()
        .filter(|l| l.contains(needle))
        .cloned()
        .collect()
}

#[cfg(test)]
mod tests;
