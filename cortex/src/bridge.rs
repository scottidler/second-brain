//! `sb cortex bridge-backfill` / `sb cortex bridge-apply`: the ONE-TIME
//! historical multi-repo backfill (harvest-completion design, Phase 7).
//!
//! ## Why this exists
//!
//! Forward multi-repo bridging (Phase 4) is deterministic: a note carrying
//! `repos-touched: [X, Y]` joins hub `repo-X` AND `repo-Y` on every sweep, keyed
//! on clyde's `files-touched` set. But `files-touched` is populable only going
//! FORWARD and only for sessions whose transcripts still survive. The
//! highest-value historical multi-repo subjects (the motivating okta-auth-rs +
//! target-repo example) already happened, carry only a single `cwd`-derived
//! `repo:`, and get NO deterministic bridge (goals-doc §3b decision A2
//! retroactivity caveat).
//!
//! This module is the bounded semantic fallback Scott approved (goals-doc §3b,
//! harvest-completion Resolved Decisions 2026-07-20): a ONE-TIME LLM pass over
//! pre-`files-touched` sessions whose transcripts are still un-reaped, proposing
//! cross-repo bridges as APPROVE-GATED hub-body wikilink diffs. It never touches
//! a landed note (goals-doc §3a attachment mechanism: semantic membership lives
//! in the HUB body, notes stay immutable) and never applies silently — output is
//! a reviewable `bridge-proposals.yml` and an `--apply`-gated hub-body diff,
//! mirroring `entities::promote_concept` exactly.
//!
//! ## Discipline (per the design doc + goals-doc §3c)
//!
//! - **Fail-closed:** a failure of the LLM detector on ANY session aborts the
//!   whole pass with ZERO proposals and a visible error (see [`backfill`]). The
//!   pass never emits a partial, silently-incomplete bridge set — that would
//!   reproduce the "limp along silently" failure class harvest was built to kill.
//! - **Provenance:** every proposal carries the clyde `session_id`(s) that drove
//!   it, so a reviewer can trace a proposed bridge back to its evidence.
//! - **Bounded reach:** the pass reaches only sessions whose transcripts survive;
//!   the composition root logs what was unreachable/reaped. No guarantee of full
//!   history — this is a backfill, not a standing mechanism.
//!
//! ## Seam
//!
//! Cortex owns the LLM call (the [`BridgeDetector`] port) and the hub-body diff;
//! it stays free of the clyde coupling. borg owns the clyde reader (the
//! transcript source), and `sb` composes the two: it scans the vault for
//! [`candidate_members`], fetches each survivor's transcript via borg's reader,
//! and hands the assembled [`BackfillSession`]s to [`backfill`].

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use eyre::{Result, WrapErr};
use serde::{Deserialize, Serialize};

use crate::hub::repo_hub_path;
use crate::vault::Note;

/// Fabric pattern the production detector runs to extract the repos a session
/// touched from its transcript. DEFERRED PREREQUISITE for the real run: this
/// pattern must be authored + synced into `~/.config/sb/patterns/` before the
/// one-time backfill is executed (the pattern file is an ops artifact, like the
/// L2 distill patterns). The offline tests inject a mock detector and never
/// reach fabric.
pub const BRIDGE_DETECT_PATTERN: &str = "extract-repos-touched";

/// The `bridge-proposals.yml` schema-version-ish marker header comment is not
/// emitted; the file mirrors `entity-proposals.yml`'s flat shape.
///
/// One proposed cross-repo bridge awaiting human approval: the landed note
/// (`member`) should ALSO join the `repo` hub (a repo the session touched beyond
/// the note's primary `repo:`), realized as a `[[member]]` wikilink added to the
/// hub body. `sessions` is the provenance — the clyde session id(s) whose
/// transcript drove this bridge.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub struct BridgeProposal {
    /// Vault-relative path of the landed note to bridge (the hub gains a link TO
    /// this note; the note itself is never modified).
    pub member: String,
    /// The secondary repo (`<org>/<repo>`) whose hub should gather `member`.
    pub repo: String,
    /// Vault-relative path of that repo's hub note (`repo_hub_path(repo)`).
    pub hub_path: String,
    /// The Obsidian wikilink markup to add to the hub body.
    pub wikilink: String,
    /// Provenance: the clyde `session_id`(s) whose transcript drove this bridge.
    pub sessions: Vec<String>,
}

/// The `bridge-proposals.yml` document (mirrors `EntityProposalsFile`).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct BridgeProposalsFile {
    pub proposals: Vec<BridgeProposal>,
}

/// A candidate landed note eligible for the historical backfill, paired with its
/// surviving transcript. Assembled by the composition root (`sb`): the
/// `session_id`/`note_path`/`primary_repo` come from [`candidate_members`]
/// scanning the vault; the `transcript` is fetched from borg's clyde reader.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BackfillSession {
    /// clyde `session_id` (recovered from the note's `source: clyde://<id>`).
    pub session_id: String,
    /// Vault-relative path of the landed note.
    pub note_path: String,
    /// The note's existing primary `repo:` (`<org>/<repo>`), validated.
    pub primary_repo: String,
    /// The surviving transcript text (from clyde `--with-body`).
    pub transcript: String,
}

/// A vault-scan candidate for the backfill: a landed harvest note that carries a
/// `clyde://` source and a single primary `repo:` but NO `repos-touched` (i.e.
/// pre-`files-touched`, so it has no deterministic multi-repo bridge). Its
/// transcript still needs fetching before it becomes a [`BackfillSession`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CandidateMember {
    pub session_id: String,
    pub note_path: String,
    pub primary_repo: String,
}

/// Detects the FULL set of canonical `<org>/<repo>` repos a session touched from
/// its transcript. Injected (per the DI convention) so the backfill logic is
/// testable without a live LLM — cortex owns the LLM call, `vault` stays LLM-free.
///
/// FAIL-CLOSED CONTRACT: a hard LLM/subprocess failure returns `Err`. [`backfill`]
/// propagates it and emits ZERO proposals — never a partial set. Returning
/// `Ok(vec![])` (the session touched only its primary repo, or none resolvable)
/// is NOT a failure; it simply yields no bridge for that session.
pub trait BridgeDetector {
    fn detect(&self, session_id: &str, transcript: &str) -> Result<Vec<String>>;
}

/// Production detector: runs a Fabric pattern over the transcript and parses its
/// output into one `<org>/<repo>` repo per line. Unlike the entity/hub Fabric
/// adapters (which swallow errors to keep a standing pass alive), this one
/// PROPAGATES the error to honor the fail-closed contract of a one-time backfill.
pub struct FabricBridgeDetector<'a> {
    pub fabric: &'a crate::config::FabricConfig,
    pub pattern: &'a str,
    pub max_input_tokens: usize,
    pub timeout_secs: u64,
}

impl BridgeDetector for FabricBridgeDetector<'_> {
    fn detect(&self, session_id: &str, transcript: &str) -> Result<Vec<String>> {
        log::debug!(
            "cortex::bridge::FabricBridgeDetector::detect: session_id={session_id} pattern={} transcript_bytes={}",
            self.pattern,
            transcript.len()
        );
        let input = crate::fabric::truncate_input(transcript, self.max_input_tokens);
        let out = crate::fabric::run_pattern(self.fabric, self.pattern, input, self.timeout_secs)
            .wrap_err_with(|| format!("bridge detector failed for session {session_id}"))?;
        let repos: Vec<String> = out
            .lines()
            .map(|l| l.trim().trim_start_matches(['-', '*', '•']).trim())
            .filter(|l| !l.is_empty())
            .map(|l| l.to_string())
            .collect();
        log::debug!(
            "cortex::bridge::FabricBridgeDetector::detect: session_id={session_id} detected={} repo(s)",
            repos.len()
        );
        Ok(repos)
    }
}

/// The Obsidian wikilink markup that resolves to a landed member note. Uses the
/// full vault-relative path (minus `.md`) with the file stem as a display alias
/// — the same collision-safe full-path form repo-hub self-links use, so it
/// resolves unconditionally regardless of basename uniqueness.
fn member_wikilink(note_path: &str) -> String {
    let target = note_path.strip_suffix(".md").unwrap_or(note_path);
    let stem = Path::new(target).file_name().and_then(|s| s.to_str()).unwrap_or(target);
    format!("[[{target}|{stem}]]")
}

/// Scan already-loaded notes for the historical-backfill candidate set: landed
/// harvest notes (`source: clyde://<id>`) carrying a single validated primary
/// `repo:` but NO `repos-touched` key (pre-`files-touched`, so they have no
/// deterministic multi-repo bridge). Pure and deterministic (sorted by note
/// path). A note whose `repos-touched` is `Some(..)` is SKIPPED: the forward
/// Phase-4 path already bridges it, so re-bridging it semantically would be
/// redundant churn.
pub fn candidate_members(notes: &[Note]) -> Vec<CandidateMember> {
    log::debug!("cortex::bridge::candidate_members: scanning {} note(s)", notes.len());
    let mut candidates: Vec<CandidateMember> = Vec::new();
    for note in notes {
        let Some(source) = note.frontmatter.source.as_deref() else {
            continue;
        };
        let Some(session_id) = source.strip_prefix("clyde://") else {
            continue;
        };
        if session_id.is_empty() {
            continue;
        }
        // Pre-`files-touched` only: a present `repos-touched` means Phase 4 owns
        // the bridge deterministically.
        if note.frontmatter.repos_touched.is_some() {
            continue;
        }
        let Some(primary) = note.frontmatter.repo.as_deref() else {
            continue;
        };
        if !vault::schema::validate_repo_slug(primary) {
            log::warn!(
                "cortex::bridge::candidate_members: note {} has non-canonical repo {primary:?}; skipping",
                note.path.display()
            );
            continue;
        }
        candidates.push(CandidateMember {
            session_id: session_id.to_string(),
            note_path: note.path.to_string_lossy().to_string(),
            primary_repo: primary.to_string(),
        });
    }
    candidates.sort_by(|a, b| a.note_path.cmp(&b.note_path));
    log::info!("cortex::bridge::candidate_members: {} candidate(s)", candidates.len());
    candidates
}

/// Run the one-time backfill over `sessions` using `detector`, returning the set
/// of proposed cross-repo bridges. PURE aside from the injected detector.
///
/// FAIL-CLOSED: if `detector.detect` returns `Err` for ANY session, this returns
/// that `Err` and NO proposals — never a partial set (design doc Phase 7
/// success criterion: "a forced-failure of the LLM pass yields ZERO proposals +
/// a visible error, never silent partial output").
///
/// For each session, the detected repos are validated (`validate_repo_slug`) and
/// the note's primary repo is subtracted (that hub already gathers the note via
/// its `repo:` edge); each remaining SECONDARY repo becomes a bridge proposal.
/// Proposals are keyed on `(hub_path, member)` in a `BTreeMap`, so the output is
/// deterministic and a repo touched by several of a note's sessions accumulates
/// provenance rather than duplicating.
pub fn backfill<D: BridgeDetector>(sessions: &[BackfillSession], detector: &D) -> Result<Vec<BridgeProposal>> {
    log::debug!("cortex::bridge::backfill: {} session(s)", sessions.len());
    let mut acc: BTreeMap<(String, String), BridgeProposal> = BTreeMap::new();

    for session in sessions {
        if !vault::schema::validate_repo_slug(&session.primary_repo) {
            log::warn!(
                "cortex::bridge::backfill: session {} has non-canonical primary repo {:?}; skipping",
                session.session_id,
                session.primary_repo
            );
            continue;
        }
        // Fail-closed: any hard detector failure aborts the whole pass. The `?`
        // discards every proposal accumulated so far, so the caller writes NONE.
        let detected = detector.detect(&session.session_id, &session.transcript)?;

        let primary_hub = repo_hub_path(&session.primary_repo);
        let mut valid: BTreeSet<String> = BTreeSet::new();
        for repo in detected {
            if vault::schema::validate_repo_slug(&repo) {
                valid.insert(repo);
            } else {
                log::warn!(
                    "cortex::bridge::backfill: session {} detected non-canonical repo {repo:?}; ignoring",
                    session.session_id
                );
            }
        }

        for repo in valid {
            let hub_path = repo_hub_path(&repo);
            // The primary hub already gathers the note via its deterministic
            // `repo:` edge; only SECONDARY hubs need a bridge.
            if hub_path == primary_hub {
                continue;
            }
            let key = (hub_path.clone(), session.note_path.clone());
            let entry = acc.entry(key).or_insert_with(|| BridgeProposal {
                member: session.note_path.clone(),
                repo: repo.clone(),
                hub_path: hub_path.clone(),
                wikilink: member_wikilink(&session.note_path),
                sessions: Vec::new(),
            });
            if !entry.sessions.contains(&session.session_id) {
                entry.sessions.push(session.session_id.clone());
            }
        }
    }

    let proposals: Vec<BridgeProposal> = acc.into_values().collect();
    log::info!(
        "cortex::bridge::backfill: {} bridge proposal(s) across {} session(s)",
        proposals.len(),
        sessions.len()
    );
    Ok(proposals)
}

/// Write proposals to `bridge-proposals.yml`, MERGING with any existing file so a
/// human's in-progress review is not clobbered (mirrors `entities::write_proposals`).
/// Proposals are keyed on `(hub_path, member)`: an existing entry keeps its
/// fields but UNIONS in any new provenance session ids (a re-run that saw more
/// evidence for the same bridge grows the trail rather than overwriting it).
pub fn write_bridge_proposals(path: &Path, fresh: Vec<BridgeProposal>) -> Result<()> {
    log::debug!(
        "cortex::bridge::write_bridge_proposals: path={} fresh={}",
        path.display(),
        fresh.len()
    );
    let mut existing: BridgeProposalsFile = if path.exists() {
        let content = std::fs::read_to_string(path)?;
        serde_yaml::from_str(&content).unwrap_or_default()
    } else {
        BridgeProposalsFile::default()
    };

    let mut index: BTreeMap<(String, String), usize> = BTreeMap::new();
    for (i, p) in existing.proposals.iter().enumerate() {
        index.insert((p.hub_path.clone(), p.member.clone()), i);
    }
    for p in fresh {
        let key = (p.hub_path.clone(), p.member.clone());
        match index.get(&key) {
            Some(&i) => {
                let cur = &mut existing.proposals[i];
                for s in p.sessions {
                    if !cur.sessions.contains(&s) {
                        cur.sessions.push(s);
                    }
                }
            }
            None => {
                index.insert(key, existing.proposals.len());
                existing.proposals.push(p);
            }
        }
    }

    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).ok();
    }
    let yaml = serde_yaml::to_string(&existing)?;
    std::fs::write(path, yaml).wrap_err_with(|| format!("write {}", path.display()))?;
    Ok(())
}

/// Outcome of applying one bridge proposal to a hub body.
#[derive(Debug, Clone, PartialEq)]
pub struct BridgeApplyReport {
    pub member: String,
    pub repo: String,
    pub hub_path: String,
    /// True when `--apply` wrote the hub body; false on a dry run (the default).
    pub applied: bool,
    /// The wikilink was already present in the hub body — a no-op, no diff.
    pub already_present: bool,
    /// Human-readable diff of what would change / changed.
    pub diff: String,
}

/// Heading under which bridged-member wikilinks are appended to a hub body.
const BRIDGED_SECTION: &str = "## Bridged members (historical multi-repo backfill)";

/// Apply ONE pending bridge proposal as a REVIEWABLE hub-body diff (the inverse
/// of `write_bridge_proposals`, mirroring `entities::promote_concept`): dry-run
/// by default (writes nothing, returns the diff); `apply` appends the `[[member]]`
/// wikilink to the hub note's body and drops the promoted proposal.
///
/// ADD-ONLY and it touches ONLY the HUB note — the landed member note is NEVER
/// opened for writing (goals-doc §3a: notes stay immutable; membership lives in
/// the hub body). Errors if `(member, repo)` is not a pending proposal (an apply
/// must trace to a proposal) or if the hub note does not exist (it must be minted
/// by `cortex hub --apply` first).
pub fn apply_bridge(
    proposals_path: &Path,
    vault_root: &Path,
    member: &str,
    repo: &str,
    apply: bool,
) -> Result<BridgeApplyReport> {
    log::debug!(
        "cortex::bridge::apply_bridge: member={member} repo={repo} apply={apply} proposals={} vault_root={}",
        proposals_path.display(),
        vault_root.display()
    );
    let pf: BridgeProposalsFile = if proposals_path.exists() {
        serde_yaml::from_str(&std::fs::read_to_string(proposals_path)?)
            .wrap_err_with(|| format!("parse {}", proposals_path.display()))?
    } else {
        BridgeProposalsFile::default()
    };

    let Some(proposal) = pf
        .proposals
        .iter()
        .find(|p| p.member == member && p.repo == repo)
        .cloned()
    else {
        eyre::bail!(
            "no bridge proposal for member {member:?} + repo {repo:?} in {} - an apply must trace to a pending proposal",
            proposals_path.display()
        );
    };

    let hub_abs = vault_root.join(&proposal.hub_path);
    if !hub_abs.exists() {
        eyre::bail!(
            "hub note {} does not exist - mint it with `sb cortex hub --apply` before bridging into it",
            hub_abs.display()
        );
    }

    let current = std::fs::read_to_string(&hub_abs).wrap_err_with(|| format!("read hub {}", hub_abs.display()))?;
    if current.contains(&proposal.wikilink) {
        log::info!(
            "cortex::bridge::apply_bridge: {} already links {member}; no-op",
            hub_abs.display()
        );
        return Ok(BridgeApplyReport {
            member: proposal.member,
            repo: proposal.repo,
            hub_path: proposal.hub_path,
            applied: false,
            already_present: true,
            diff: String::new(),
        });
    }

    let diff = format!(
        "{hub}:  + {link}   (bridged from session(s): {sessions})\nbridge-proposals.yml:  - proposal ({member} -> {repo})",
        hub = proposal.hub_path,
        link = proposal.wikilink,
        sessions = proposal.sessions.join(", "),
    );

    if apply {
        let new_content = append_bridged_member(&current, &proposal.wikilink);
        std::fs::write(&hub_abs, new_content).wrap_err_with(|| format!("write hub {}", hub_abs.display()))?;
        // Drop the promoted proposal (matched on member+repo).
        let remaining: Vec<BridgeProposal> = pf
            .proposals
            .into_iter()
            .filter(|p| !(p.member == member && p.repo == repo))
            .collect();
        overwrite_bridge_proposals(proposals_path, &BridgeProposalsFile { proposals: remaining })?;
        log::info!(
            "cortex::bridge::apply_bridge: bridged {member} into {}",
            proposal.hub_path
        );
    }

    Ok(BridgeApplyReport {
        member: proposal.member,
        repo: proposal.repo,
        hub_path: proposal.hub_path,
        applied: apply,
        already_present: false,
        diff,
    })
}

/// Append a bridged-member wikilink to a hub body under [`BRIDGED_SECTION`],
/// creating the section if absent. ADD-ONLY: nothing existing is removed or
/// altered; the link is appended as a new `- [[..]]` list item.
fn append_bridged_member(current: &str, wikilink: &str) -> String {
    let trimmed = current.trim_end();
    if trimmed.contains(BRIDGED_SECTION) {
        format!("{trimmed}\n- {wikilink}\n")
    } else {
        format!("{trimmed}\n\n{BRIDGED_SECTION}\n\n- {wikilink}\n")
    }
}

/// Overwrite `bridge-proposals.yml` with the given set (used by an apply to drop
/// the promoted proposal). Distinct from `write_bridge_proposals`, which MERGES.
fn overwrite_bridge_proposals(path: &Path, file: &BridgeProposalsFile) -> Result<()> {
    let yaml = serde_yaml::to_string(file)?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).ok();
    }
    std::fs::write(path, yaml).wrap_err_with(|| format!("write {}", path.display()))?;
    Ok(())
}

#[cfg(test)]
mod tests;
