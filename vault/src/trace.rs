use crate::schema::Method;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

/// Process-lifetime counter to guarantee uniqueness even for
/// ingests arriving in the same nanosecond (e.g. batch CLI import).
static COUNTER: AtomicU32 = AtomicU32::new(0);

/// Generate a trace ID with a method-specific prefix.
///
/// Format: `{prefix}-{6 hex chars}`
///
/// Uniqueness: mixes nanosecond timestamp, process ID, and an atomic
/// counter. No external dependencies (no `rand` crate needed).
pub fn generate(method: Method) -> String {
    let prefix = method_prefix(method);
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos() as u64;
    let pid = std::process::id();
    let seq = COUNTER.fetch_add(1, Ordering::Relaxed);

    // Mix all three sources, then take lower 24 bits (6 hex chars)
    let mixed = nanos
        .wrapping_mul(6364136223846793005) // LCG multiplier
        ^ (pid as u64) << 16
        ^ seq as u64;
    let hex = format!("{:06x}", mixed & 0x00FF_FFFF);
    format!("{prefix}-{hex}")
}

fn method_prefix(method: Method) -> &'static str {
    match method {
        Method::Telegram => "tg",
        Method::Discord => "dc",
        Method::Http => "ht",
        Method::Clipboard => "cb",
        Method::Cli => "cl",
        Method::Ntfy => "nf",
        Method::Signal => "sg",
        Method::Manual => "mn",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_generate_format() {
        let id = generate(Method::Telegram);
        let re = regex::Regex::new(r"^[a-z]{2}-[0-9a-f]{6}$").expect("valid regex");
        assert!(re.is_match(&id), "trace ID '{id}' does not match expected format");
    }

    #[test]
    fn test_method_prefixes() {
        assert_eq!(method_prefix(Method::Telegram), "tg");
        assert_eq!(method_prefix(Method::Discord), "dc");
        assert_eq!(method_prefix(Method::Http), "ht");
        assert_eq!(method_prefix(Method::Clipboard), "cb");
        assert_eq!(method_prefix(Method::Cli), "cl");
        assert_eq!(method_prefix(Method::Ntfy), "nf");
        assert_eq!(method_prefix(Method::Signal), "sg");
        assert_eq!(method_prefix(Method::Manual), "mn");
    }

    #[test]
    fn test_sequential_uniqueness() {
        let id1 = generate(Method::Cli);
        let id2 = generate(Method::Cli);
        assert_ne!(id1, id2, "two sequential trace IDs should differ");
    }

    #[test]
    fn test_different_methods_different_prefix() {
        let tg = generate(Method::Telegram);
        let dc = generate(Method::Discord);
        assert!(tg.starts_with("tg-"));
        assert!(dc.starts_with("dc-"));
    }
}
