//! Test-only: capture WARN-and-above log lines process-wide so a test can assert
//! on the WARN a code path emits. Lines carry the path or trace id, so a test
//! filters to its own.

struct WarnCapture(std::sync::Mutex<Vec<String>>);

impl log::Log for WarnCapture {
    fn enabled(&self, meta: &log::Metadata) -> bool {
        meta.level() <= log::Level::Warn
    }
    fn log(&self, record: &log::Record) {
        if self.enabled(record.metadata()) {
            self.0.lock().unwrap().push(format!("{}", record.args()));
        }
    }
    fn flush(&self) {}
}

static WARNS: WarnCapture = WarnCapture(std::sync::Mutex::new(Vec::new()));

pub fn install() {
    // set_logger fails only when a logger is already installed: ours, from an
    // earlier test in this process. Any other logger would make the WARN
    // assertions fail loudly rather than pass vacuously.
    let _ = log::set_logger(&WARNS);
    log::set_max_level(log::LevelFilter::Warn);
}

pub fn warns_containing(needle: &str) -> Vec<String> {
    WARNS
        .0
        .lock()
        .unwrap()
        .iter()
        .filter(|l| l.contains(needle))
        .cloned()
        .collect()
}
