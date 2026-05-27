#!/usr/bin/env bash
# Migrate facet's ledger schema for the v2 redesign by creating the v2
# tables (gems, interaction_turns, narratives, narrative_axes) alongside
# the existing v1 tables (judgment_moments, etc.).
#
# Per the user rule "NEVER write schema-migration / legacy-format-changeover
# code in Rust" and the design doc at
# docs/design/2026-05-26-facet-v2-gems-and-narrative-spectra.md.
#
# Idempotent. Safe to re-run. Refuses to run if facet.service is active.
# No data migration of moments-to-gems is attempted; the fossil record
# (JSONL transcripts) re-extracts cleanly into v2 via `sb facet harvest`
# once Phase 3 lands.
#
# NOTE: There is NO `dreams` table. Dreams are derived, regenerable
# artifacts rendered directly to markdown each pass. See Phase 6 of the
# design doc.
#
# Usage:
#   bin/migrate-facet-v2.sh                 # create v2 tables in the default DB
#   bin/migrate-facet-v2.sh --db <path>     # override the ledger DB path
#   bin/migrate-facet-v2.sh --dry-run       # print the DDL without applying
#   bin/migrate-facet-v2.sh --force         # bypass the facet.service active check

set -euo pipefail

FACET_DB_DEFAULT="$HOME/.local/share/sb/facet/state.db"
FACET_DB="$FACET_DB_DEFAULT"
DRY_RUN=0
FORCE=0

usage() {
    grep -E '^# ' "$0" | sed 's/^# \{0,1\}//'
    exit 1
}

while (($#)); do
    case "$1" in
        --db)       FACET_DB="$2"; shift 2 ;;
        --dry-run)  DRY_RUN=1; shift ;;
        --force)    FORCE=1; shift ;;
        -h|--help)  usage ;;
        *)          echo "unknown arg: $1" >&2; usage ;;
    esac
done

if [[ "$FORCE" -ne 1 ]] && systemctl --user is-active --quiet facet.service 2>/dev/null; then
    echo "ERROR: facet.service is active. Stop it first:" >&2
    echo "  sb facet daemon --stop" >&2
    echo "  bin/migrate-facet-v2.sh" >&2
    echo "  sb facet daemon --start" >&2
    exit 1
fi

read -r -d '' V2_DDL <<'SQL' || true
-- facet v2 schema. Tables coexist with v1 (judgment_moments, etc.) during
-- the cutover. See docs/design/2026-05-26-facet-v2-gems-and-narrative-spectra.md.

-- Multi-turn dialog-slice gem. One row per gem extracted from a session.
-- Idempotency key is (workitem_id, content_hash) where content_hash is a
-- sha256 over the sorted turn UUIDs the gem covers; boundary UUIDs are
-- stored for inspection only and do not participate in uniqueness.
CREATE TABLE IF NOT EXISTS gems (
    id INTEGER PRIMARY KEY,
    workitem_id INTEGER NOT NULL REFERENCES work_items(id) ON DELETE CASCADE,
    session_uuid TEXT NOT NULL,
    content_hash TEXT NOT NULL,
    first_user_turn_uuid TEXT NOT NULL,
    last_user_turn_uuid TEXT NOT NULL,
    task TEXT NOT NULL,
    context_loaded TEXT NOT NULL,
    context_missing TEXT NOT NULL,
    review_accepted TEXT,
    review_rejected TEXT,
    review_verified_manually TEXT,
    review_rewrote_by_hand TEXT,
    tags TEXT NOT NULL,
    why_it_matters TEXT NOT NULL,
    extractor_model TEXT NOT NULL,
    extracted_at TEXT NOT NULL,
    UNIQUE (workitem_id, content_hash)
);

CREATE INDEX IF NOT EXISTS idx_gems_session  ON gems(session_uuid);
CREATE INDEX IF NOT EXISTS idx_gems_workitem ON gems(workitem_id);

-- One row per interaction turn inside a gem. `seq` orders turns within
-- the gem starting at 0.
CREATE TABLE IF NOT EXISTS interaction_turns (
    id INTEGER PRIMARY KEY,
    gem_id INTEGER NOT NULL REFERENCES gems(id) ON DELETE CASCADE,
    seq INTEGER NOT NULL,
    ai_says TEXT NOT NULL,
    ai_turn_uuid TEXT NOT NULL,
    user_says TEXT NOT NULL,
    user_turn_uuid TEXT NOT NULL,
    tags TEXT NOT NULL,
    UNIQUE (gem_id, seq)
);

CREATE INDEX IF NOT EXISTS idx_interaction_turns_gem ON interaction_turns(gem_id);

-- One row per discovered narrative (Session Arc, Cross-Session Arc, or
-- evergreen mode rollup). `cluster_key` is the stable identity per
-- cluster (session_uuid / sha256-derived xs-... / mode-<name>) and is
-- the idempotency key; titles may drift on re-narrate. `gem_ids` is a
-- JSON array of citations into the gems table.
CREATE TABLE IF NOT EXISTS narratives (
    id INTEGER PRIMARY KEY,
    cluster_key TEXT NOT NULL UNIQUE,
    slug TEXT NOT NULL,
    title TEXT NOT NULL,
    thesis TEXT NOT NULL,
    body_md TEXT NOT NULL,
    gem_ids TEXT NOT NULL,
    archetype TEXT NOT NULL,
    synthesised_at TEXT NOT NULL,
    synthesiser_model TEXT NOT NULL,
    revision INTEGER NOT NULL DEFAULT 1
);

CREATE INDEX IF NOT EXISTS idx_narratives_slug      ON narratives(slug);
CREATE INDEX IF NOT EXISTS idx_narratives_archetype ON narratives(archetype);

-- Sidecar of narrative metadata describing what holds the cluster
-- together. One row per narrative.
CREATE TABLE IF NOT EXISTS narrative_axes (
    narrative_id INTEGER PRIMARY KEY REFERENCES narratives(id) ON DELETE CASCADE,
    semantic_cluster_id INTEGER,
    mode_mix TEXT NOT NULL,
    time_window_start TEXT,
    time_window_end TEXT,
    repos TEXT NOT NULL,
    workitem_ids TEXT NOT NULL
);
SQL

if [[ "$DRY_RUN" -eq 1 ]]; then
    echo "# Would apply the following DDL to $FACET_DB:"
    echo ""
    echo "$V2_DDL"
    exit 0
fi

if [[ ! -f "$FACET_DB" ]]; then
    echo "ERROR: ledger DB not found at $FACET_DB" >&2
    echo "Run \`sb facet harvest\` once to initialise the v1 schema, then re-run this script." >&2
    exit 1
fi

before_count=$(sqlite3 "$FACET_DB" "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name IN ('gems','interaction_turns','narratives','narrative_axes');")

sqlite3 "$FACET_DB" <<SQL
BEGIN;
$V2_DDL
COMMIT;
SQL

after_count=$(sqlite3 "$FACET_DB" "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name IN ('gems','interaction_turns','narratives','narrative_axes');")

echo "facet v2 schema applied to $FACET_DB"
echo "  v2 tables present before: $before_count / 4"
echo "  v2 tables present after:  $after_count / 4"
if [[ "$before_count" == "$after_count" ]]; then
    echo "  (idempotent re-run; no changes)"
fi
