//! Thread clustering (design doc Selection > Thread boundary rules, v1
//! deterministic). Survivors of the selection gate that share the cluster key
//! `(cwd, git-branch)` AND fall within the inter-session gap window merge into
//! one thread note. A thread of size 1 is a plain session note - the design
//! collapses to trivial at N=1.
//!
//! Boundaries are deterministic and never span harvest runs (the orchestrator
//! only ever passes one run's survivors here). Day-2 work in the same cwd is a
//! NEW note because it arrives in a later run, not because of anything this
//! module does. `git-branch` is part of the key so concurrent same-repo work
//! (frontend + backend in one monorepo, different branches) does not blindly
//! merge.

use std::collections::BTreeMap;

use chrono::{DateTime, Duration, FixedOffset};
use eyre::{Context, Result, eyre};

use super::contract::SessionRecord;

/// One clustered thread: 1+ sessions that became a single note. Members are
/// ordered by `created`; `primary` is the most-substantive session (most
/// messages), whose id anchors the note's `source:` and the watermark entry.
#[derive(Debug, Clone, PartialEq)]
pub struct Thread {
    pub members: Vec<SessionRecord>,
    primary_idx: usize,
}

impl Thread {
    /// The primary (most-messages) session; anchors identity.
    pub fn primary(&self) -> &SessionRecord {
        &self.members[self.primary_idx]
    }

    /// Every member's session id, in `created` order.
    pub fn member_ids(&self) -> Vec<String> {
        self.members.iter().map(|s| s.session_id.clone()).collect()
    }

    /// Sum of `n-msgs` across members - the thread's "how much content"
    /// identity signal for the watermark cheap filter.
    pub fn total_msgs(&self) -> i64 {
        self.members.iter().map(|s| s.n_msgs).sum()
    }
}

fn parse_ts(session: &SessionRecord, which: &str, raw: Option<&str>) -> Result<DateTime<FixedOffset>> {
    // A null `created` is guarded at the selection stage (`select.rs`) and
    // never reaches clustering; this `None` arm is the fail-loud backstop for a
    // caller that bypassed selection, never a silent drop.
    let raw = raw.ok_or_else(|| eyre!("session {} has a null {which} timestamp", session.session_id))?;
    DateTime::parse_from_rfc3339(raw).with_context(|| {
        format!(
            "session {} has an unparseable {which} timestamp {raw:?}",
            session.session_id
        )
    })
}

/// The cluster key. `git-branch` present-null collapses to a stable sentinel
/// so branch-less sessions still cluster on cwd alone. A null `cwd` (a record
/// that bypassed the selection gate's repo check) collapses to the empty
/// string so clustering never panics.
fn cluster_key(record: &SessionRecord) -> (String, String) {
    let branch = record.git_branch.clone().unwrap_or_else(|| "\u{0}none".to_string());
    (record.cwd.clone().unwrap_or_default(), branch)
}

/// Pick the primary index: most messages, ties broken by session id (stable).
fn primary_index(members: &[SessionRecord]) -> usize {
    members
        .iter()
        .enumerate()
        .max_by(|(_, a), (_, b)| a.n_msgs.cmp(&b.n_msgs).then_with(|| b.session_id.cmp(&a.session_id)))
        .map(|(i, _)| i)
        .unwrap_or(0)
}

/// Cluster selected survivors into threads. Loud on an unparseable timestamp
/// (never silently drops a session). Output threads are ordered by their
/// primary session's `created` time, then session id, for determinism.
pub fn cluster_threads(records: &[SessionRecord], window: Duration) -> Result<Vec<Thread>> {
    log::debug!(
        "harvest::cluster_threads: input_sessions={} window_secs={}",
        records.len(),
        window.num_seconds()
    );

    // Group by (cwd, git-branch) in a BTreeMap for deterministic iteration.
    let mut groups: BTreeMap<(String, String), Vec<SessionRecord>> = BTreeMap::new();
    for r in records {
        groups.entry(cluster_key(r)).or_default().push(r.clone());
    }

    let mut threads: Vec<Thread> = Vec::new();
    for ((cwd, branch), mut group) in groups {
        // Sort by created (then id) so gap detection is over a real timeline.
        let mut keyed: Vec<(DateTime<FixedOffset>, DateTime<FixedOffset>, SessionRecord)> =
            Vec::with_capacity(group.len());
        for r in group.drain(..) {
            let created = parse_ts(&r, "created", r.created.as_deref())?;
            let modified = parse_ts(&r, "modified", Some(&r.modified))?;
            keyed.push((created, modified, r));
        }
        keyed.sort_by(|a, b| a.0.cmp(&b.0).then_with(|| a.2.session_id.cmp(&b.2.session_id)));

        let mut current: Vec<SessionRecord> = Vec::new();
        let mut thread_last_modified: Option<DateTime<FixedOffset>> = None;
        for (created, modified, record) in keyed {
            let split = match thread_last_modified {
                Some(last) => created.signed_duration_since(last) > window,
                None => false,
            };
            if split && !current.is_empty() {
                let idx = primary_index(&current);
                threads.push(Thread {
                    members: std::mem::take(&mut current),
                    primary_idx: idx,
                });
                thread_last_modified = None;
            }
            thread_last_modified = Some(match thread_last_modified {
                Some(last) if last > modified => last,
                _ => modified,
            });
            log::trace!(
                "harvest::cluster_threads: cwd={cwd} branch={branch} session={} joins current thread (size={})",
                record.session_id,
                current.len() + 1
            );
            current.push(record);
        }
        if !current.is_empty() {
            let idx = primary_index(&current);
            threads.push(Thread {
                members: current,
                primary_idx: idx,
            });
        }
    }

    // Deterministic global order: by primary created, then primary id.
    threads.sort_by(|a, b| {
        a.primary()
            .created
            .cmp(&b.primary().created)
            .then_with(|| a.primary().session_id.cmp(&b.primary().session_id))
    });

    log::debug!("harvest::cluster_threads: produced {} thread(s)", threads.len());
    Ok(threads)
}

#[cfg(test)]
mod tests;
