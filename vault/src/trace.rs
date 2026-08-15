use crate::schema::Method;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

/// Process-lifetime counter that decorrelates ingests arriving in the same
/// nanosecond (e.g. batch CLI import) - it does NOT guarantee global uniqueness
/// (see below).
static COUNTER: AtomicU32 = AtomicU32::new(0);

/// Generate a trace ID with a method-specific prefix.
///
/// Format: `{prefix}-{8 hex chars}`
///
/// Collision profile: the ID is the lower 32 bits of a mix of nanosecond
/// timestamp, PID, and an atomic counter - a 4,294,967,296-value space. This
/// is NOT a uniqueness guarantee: by the birthday bound, collisions become
/// likely around ~77,000 IDs sharing a prefix (widened from ~4,800 at the
/// prior 24-bit width - a delay, not an elimination; see the harvest note
/// identity design doc, which keys note replacement on this field and widens
/// it for that reason). It is a low-collision, dependency-free (no `rand`)
/// label, adequate because the receipts DB keys on the full trace string and
/// a rare collision only conflates two ingests in the log, not in the vault -
/// and, since the harvest identity design, the three-term confirmation guard
/// (`trace` + `source` + `harvest-body-hash`) is what makes a collision
/// non-destructive there too. If true uniqueness is ever required, widen the
/// field further or add a UUID dependency.
pub fn generate(method: Method) -> String {
    let prefix = method_prefix(method);
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos() as u64;
    let pid = std::process::id();
    let seq = COUNTER.fetch_add(1, Ordering::Relaxed);

    // Mix all three sources, then take lower 32 bits (8 hex chars)
    let mixed = nanos
        .wrapping_mul(6364136223846793005) // LCG multiplier
        ^ (pid as u64) << 16
        ^ seq as u64;
    let hex = format!("{:08x}", mixed & 0xFFFF_FFFF);
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
        Method::Harvest => "hv",
    }
}

#[cfg(test)]
mod tests;
