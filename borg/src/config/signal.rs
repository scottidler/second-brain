use serde::{Deserialize, Serialize};

/// Signal transport configuration. The presence of this section enables Signal
/// ingest (mirrors `telegram`, `discord`, `ntfy`). `host` is mandatory because
/// Signal-Server fans out Note-to-Self envelopes to every linked device and has
/// no polling-lock equivalent to Telegram's `TerminatedByOtherGetUpdates` -
/// leaving `host` unset on a multi-machine install would silently double-ingest.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "kebab-case")]
pub struct SignalConfig {
    /// ACI UUIDs (string form) allowed to send borg peer DMs.
    /// Note-to-Self is structural and never gated by this list.
    #[serde(default)]
    pub allowed_senders: Vec<String>,

    /// Default reply target for cross-method notifications (e.g. an ntfy
    /// ingest acknowledged via Signal). `None` = `SelfSync`; `Some(<ACI UUID>)`
    /// = peer.
    #[serde(default)]
    pub notification_recipient: Option<String>,

    /// Pin Signal ingest to a specific hostname. Mandatory when the `signal:`
    /// block is present; config-load fails if missing or empty.
    pub host: String,

    /// Maximum accepted Note-to-Self envelopes per hour before the rate gate
    /// trips and pauses ingest until the daemon is restarted. Backstops an
    /// upstream `signal-rs` regression in the wire-ACI to `Recipient::SelfSync`
    /// mapping. Peer DMs are not counted.
    #[serde(default = "default_signal_rate_threshold")]
    pub notetoself_rate_threshold_per_hour: u32,
}

fn default_signal_rate_threshold() -> u32 {
    100
}
