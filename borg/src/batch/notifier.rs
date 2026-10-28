//! The bulk-summary notifier: one task, spawned at daemon start, that owns the
//! desktop and Telegram sinks and sends each [`BatchSummary`] it receives once
//! per sink. The summary is one best-effort attempt; each sink's outcome is
//! logged with the batch id.

use std::sync::Arc;

use tokio::sync::mpsc::UnboundedReceiver;

use super::BatchSummary;
use crate::notify::{self, Announced, Desktop, MessageSink, Telegram};

/// A sink the notifier can hold across awaits.
pub type Sink = Arc<dyn MessageSink + Send + Sync>;

/// What one sink did with one summary.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SinkOutcome {
    Sent,
    Failed,
    /// No sink configured on this host, or notifications disabled.
    Disabled,
}

impl SinkOutcome {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Sent => "sent",
            Self::Failed => "failed",
            Self::Disabled => "disabled",
        }
    }
}

/// Both sinks' outcomes for one summary.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Delivery {
    pub desktop: SinkOutcome,
    pub telegram: SinkOutcome,
}

async fn send_one(sink: Option<&Sink>, name: &str, batch_id: &str, text: &str, disabled: bool) -> SinkOutcome {
    let Some(sink) = sink else {
        return SinkOutcome::Disabled;
    };
    match notify::announce_unless(sink.as_ref(), text, disabled).await {
        Announced::Sent => SinkOutcome::Sent,
        Announced::Suppressed => SinkOutcome::Disabled,
        Announced::Failed(why) => {
            log::warn!("batch {batch_id}: {name} summary failed: {why}");
            SinkOutcome::Failed
        }
    }
}

/// Send `summary` once to each configured sink, unless `disabled`, and log
/// `batch <id>: desktop <outcome>, telegram <outcome>`.
pub async fn deliver(
    summary: &BatchSummary,
    desktop: Option<&Sink>,
    telegram: Option<&Sink>,
    disabled: bool,
) -> Delivery {
    let text = summary.text();
    let id = &summary.batch_id;
    let delivery = Delivery {
        desktop: send_one(desktop, "desktop", id, &text, disabled).await,
        telegram: send_one(telegram, "telegram", id, &text, disabled).await,
    };
    log::info!(
        "batch {id}: desktop {}, telegram {}",
        delivery.desktop.as_str(),
        delivery.telegram.as_str()
    );
    delivery
}

/// Drain `rx` until every sender is gone, delivering each summary. `disabled`
/// is a parameter so a test can open the gate with counting sinks.
pub async fn run_with(
    mut rx: UnboundedReceiver<BatchSummary>,
    desktop: Option<Sink>,
    telegram: Option<Sink>,
    disabled: bool,
) {
    log::info!(
        "batch notifier: starting (desktop={}, telegram={})",
        desktop.is_some(),
        telegram.is_some()
    );
    while let Some(summary) = rx.recv().await {
        deliver(&summary, desktop.as_ref(), telegram.as_ref(), disabled).await;
    }
    log::info!("batch notifier: channel closed, stopping");
}

/// The daemon's notifier task: the `AppState` sinks behind the process-wide
/// disable gate ([`notify::real_notifications_disabled`]).
pub async fn run(rx: UnboundedReceiver<BatchSummary>, desktop: Option<Desktop>, telegram: Option<Telegram>) {
    let desktop = desktop.map(|d| Arc::new(d) as Sink);
    let telegram = telegram.map(|t| Arc::new(t) as Sink);
    run_with(rx, desktop, telegram, notify::real_notifications_disabled()).await;
}
