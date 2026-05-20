//! Self-RSS reader.
//!
//! Reads `VmRSS` from `/proc/self/status` so daemons can log their resident
//! set size on tick boundaries without pulling in a procfs crate. Linux-only
//! by design; on macOS the reader returns `None` and callers should skip
//! the log line rather than guess.
//!
//! The cortex embed loop pairs an entry-side and exit-side reading to size
//! per-tick allocator deltas; see `docs/design/2026-05-19-cortex-embed-memory-bounding.md`
//! and Phase 7 of `docs/design/2026-05-20-shakedown-v0.8.5-cleanup.md`.

/// Resident set size in bytes for the current process, or `None` if the
/// platform doesn't expose `/proc/self/status` (macOS, Windows) or the
/// file is unreadable.
pub fn read_self_rss() -> Option<u64> {
    #[cfg(target_os = "linux")]
    {
        let content = std::fs::read_to_string("/proc/self/status").ok()?;
        for line in content.lines() {
            if let Some(rest) = line.strip_prefix("VmRSS:") {
                // Format: "VmRSS:     12345 kB"
                let kb: u64 = rest.split_whitespace().next()?.parse().ok()?;
                return Some(kb * 1024);
            }
        }
        None
    }
    #[cfg(not(target_os = "linux"))]
    {
        None
    }
}

/// Format a byte count for log output: "1.23 GB" / "456.7 MB" / "789 KB".
pub fn human_bytes(bytes: u64) -> String {
    const UNITS: &[&str] = &["B", "KB", "MB", "GB", "TB"];
    let mut v = bytes as f64;
    let mut i = 0;
    while v >= 1024.0 && i < UNITS.len() - 1 {
        v /= 1024.0;
        i += 1;
    }
    if i == 0 {
        format!("{bytes} {}", UNITS[i])
    } else {
        format!("{v:.2} {}", UNITS[i])
    }
}

#[cfg(test)]
mod tests;
