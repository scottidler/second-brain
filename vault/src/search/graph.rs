//! Materialized edge graph: read/write helpers for the `edges` table and the
//! graph-expansion traversal oracle uses for `mode=graph`/`graph-hybrid`.
//!
//! Phase 1 of the graph-augmented-memory design
//! (`docs/design/2026-06-05-graph-augmented-memory.md`).
//!
//! The cortex graph pass *writes* edges here (deterministic semantic-kNN /
//! wikilink / rarity-weighted shared-tag / metadata edges); oracle only ever
//! *reads* them via [`SearchIndex::expand_graph`]. This module is not feature
//! gated — the `edges` table and traversal do not require embeddings; the
//! semantic-edge *source* (`note_embeddings`) is the only `vec`-gated input
//! and lives in `vector.rs`.

use eyre::Result;
use rusqlite::params;

use super::{SearchIndex, optional_row, warn_row};

/// One edge to insert into the `edges` table. `src` and `dst` are
/// vault-relative note paths. Every edge is *owned by its `src`*: the graph
/// pass refreshes a note's edges with delete-by-`src` then insert, and
/// undirected kinds are made undirected at read time by [`expand_graph`]'s
/// `src IN seeds OR dst IN seeds` query rather than by writing reverse rows.
#[derive(Debug, Clone, PartialEq)]
pub struct Edge {
    pub src: String,
    pub dst: String,
    pub kind: String,
    pub weight: f32,
    /// `""` for deterministic kinds; a relation string for Phase-5 `fact` edges.
    pub predicate: String,
    /// Provenance: the note a typed edge was derived from (Phase 5); `""`
    /// for deterministic kinds.
    pub src_note: String,
}

impl Edge {
    /// Construct a deterministic edge (empty `predicate`/`src_note`).
    pub fn deterministic(src: impl Into<String>, dst: impl Into<String>, kind: impl Into<String>, weight: f32) -> Self {
        Self {
            src: src.into(),
            dst: dst.into(),
            kind: kind.into(),
            weight,
            predicate: String::new(),
            src_note: String::new(),
        }
    }

    /// Construct a Phase-5 typed `fact` edge: `kind = "fact"`, the relation in
    /// `predicate`, and the originating note in `src_note` for provenance.
    pub fn fact(
        src: impl Into<String>,
        dst: impl Into<String>,
        predicate: impl Into<String>,
        weight: f32,
        src_note: impl Into<String>,
    ) -> Self {
        Self {
            src: src.into(),
            dst: dst.into(),
            kind: "fact".to_string(),
            weight,
            predicate: predicate.into(),
            src_note: src_note.into(),
        }
    }
}

/// One materialized `fact` edge (Phase 5), with provenance.
#[derive(Debug, Clone, PartialEq)]
pub struct FactEdge {
    pub src: String,
    pub dst: String,
    pub predicate: String,
    pub src_note: String,
}

/// The columns the cortex graph pass needs to derive deterministic edges,
/// read straight from the `notes` table (the signals already live in the
/// index — no second filesystem walk).
#[derive(Debug, Clone)]
pub struct GraphNoteRow {
    pub path: String,
    pub tags: Vec<String>,
    pub source: String,
    pub creator: String,
    pub domain: String,
    pub body: String,
    pub modified_at: i64,
    /// Canonical `<org>/<repo>` anchor (harvest-clyde-sessions Phase 9), empty
    /// when the note has no repo. Feeds the Phase 10 `repo-member` hub edge.
    pub repo: String,
    /// Every repo the session touched (harvest-completion Phase 4), feeding the
    /// deterministic multi-repo-member hub edge. Flattened to the SET of repos
    /// here, mirroring `repo` (empty = no bridge), because the edge is a set
    /// operation: `None` and `Some(vec![])` both yield an empty Vec (no extra
    /// edge). The load-bearing three-state distinction lives where it is
    /// consumed semantically -- the `notes.repos_touched` column (NULL vs `'[]'`)
    /// and `Frontmatter::repos_touched` -- not at this edge-building seam.
    pub repos_touched: Vec<String>,
}

/// One entity row's mutable columns: `(kind, hub_path, ontotype)`.
pub type EntityRow = (String, Option<String>, Option<String>);

/// One reaching of a neighbor during graph expansion. A neighbor reached by
/// several paths yields several `GraphReach` rows; the caller (oracle)
/// aggregates them into an `expansion_score`. `weight` is the accumulated
/// edge weight along the path (product across hops); `hop` is 1-based.
#[derive(Debug, Clone, PartialEq)]
pub struct GraphReach {
    pub path: String,
    /// The original seed note this reaching traces back to.
    pub origin_seed: String,
    pub hop: u8,
    pub weight: f32,
    pub kind: String,
    pub predicate: String,
}

impl SearchIndex {
    /// Create the graph-augmented-memory tables: the materialized `edges`
    /// table, the always-present `graph_state` key/value store, the per-note
    /// `edge_build_state` incremental watermarks, and the `entities` table
    /// (Phase 3/5). All are plain tables (no FTS / virtual table) and exist in
    /// every build, `vec` or not — the `edges` read path does not require
    /// embeddings. The `dst`/`src` foreign keys with `ON DELETE CASCADE`
    /// mirror `note_embeddings`: when `index_vault` removes a deleted note from
    /// `notes`, its incident edges and build-state row vanish natively, so
    /// traversal never surfaces a path that no longer exists. Idempotent via
    /// `IF NOT EXISTS`; called from `ensure_schema`.
    pub(super) fn ensure_graph_schema(&self) -> Result<()> {
        self.conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS edges (
                src        TEXT NOT NULL,
                dst        TEXT NOT NULL,
                kind       TEXT NOT NULL,
                weight     REAL NOT NULL,
                predicate  TEXT NOT NULL DEFAULT '',
                src_note   TEXT NOT NULL DEFAULT '',
                PRIMARY KEY (src, dst, kind, predicate),
                FOREIGN KEY (src) REFERENCES notes(path) ON DELETE CASCADE,
                FOREIGN KEY (dst) REFERENCES notes(path) ON DELETE CASCADE
            );
            CREATE INDEX IF NOT EXISTS idx_edges_src ON edges(src);
            CREATE INDEX IF NOT EXISTS idx_edges_dst ON edges(dst);
            CREATE INDEX IF NOT EXISTS idx_edges_kind ON edges(kind);

            CREATE TABLE IF NOT EXISTS graph_state (
                key   TEXT PRIMARY KEY,
                value TEXT NOT NULL
            );

            -- Per-note incremental build watermarks. Mirrors the per-row
            -- staleness pattern `stale_embedding_targets` uses: a note is a
            -- content-edge target when `notes.modified_at > content_built_at`,
            -- and a semantic-edge target when its newest summary embedding's
            -- `produced_at > semantic_built_at`. Keying semantic on
            -- `produced_at` (not `modified_at`) is what prevents stranding a
            -- note whose embedding lands after it was skipped.
            CREATE TABLE IF NOT EXISTS edge_build_state (
                note_path        TEXT PRIMARY KEY,
                content_built_at INTEGER NOT NULL DEFAULT 0,
                semantic_built_at INTEGER NOT NULL DEFAULT 0,
                FOREIGN KEY (note_path) REFERENCES notes(path) ON DELETE CASCADE
            );

            CREATE TABLE IF NOT EXISTS entities (
                id        TEXT PRIMARY KEY,
                kind      TEXT NOT NULL,
                hub_path  TEXT,
                ontotype  TEXT
            );",
        )?;
        Ok(())
    }

    /// True when `path` exists as a row in `notes`. The graph pass calls this
    /// to enforce the universal resolve-`dst`-or-skip rule before inserting an
    /// edge (see [`insert_edges`](Self::insert_edges)).
    pub fn note_path_exists(&self, path: &str) -> Result<bool> {
        let n: i64 = self
            .conn
            .query_row("SELECT 1 FROM notes WHERE path = ?1", params![path], |row| row.get(0))
            .unwrap_or(0);
        Ok(n == 1)
    }

    /// Resolve a wikilink slug to a real note path, or `None` if it dangles.
    /// Public wrapper over the private resolver so the graph pass can build
    /// resolved-target-only `wikilink` edges.
    pub fn resolve_note_path(&self, target: &str) -> Result<Option<String>> {
        self.resolve_wikilink(target)
    }

    /// Distinct edge kinds present in the `edges` table. The eval's fact-layer
    /// ablation builds a non-fact include-list from this (`edge_kinds` is an
    /// allow-list, not an exclude-list).
    pub fn edge_kinds(&self) -> Result<Vec<String>> {
        let mut stmt = self.conn.prepare("SELECT DISTINCT kind FROM edges ORDER BY kind")?;
        let kinds = stmt
            .query_map([], |row| row.get::<_, String>(0))?
            .collect::<rusqlite::Result<Vec<String>>>()?;
        Ok(kinds)
    }

    /// Read a `graph_state` value by key (`None` when unset).
    pub fn graph_state_get(&self, key: &str) -> Result<Option<String>> {
        optional_row(
            self.conn
                .query_row("SELECT value FROM graph_state WHERE key = ?1", params![key], |row| {
                    row.get(0)
                }),
        )
    }

    /// Write a `graph_state` value (upsert).
    pub fn graph_state_set(&self, key: &str, value: &str) -> Result<()> {
        self.conn.execute(
            "INSERT INTO graph_state (key, value) VALUES (?1, ?2)
             ON CONFLICT(key) DO UPDATE SET value = excluded.value",
            params![key, value],
        )?;
        Ok(())
    }

    /// Delete every edge owned by `src`. The incremental graph pass calls this
    /// before re-inserting a note's edges so a refresh is a clean replace.
    pub fn delete_edges_by_src(&self, src: &str) -> Result<usize> {
        let n = self.conn.execute("DELETE FROM edges WHERE src = ?1", params![src])?;
        Ok(n)
    }

    /// Drop every row in `edges` (used by a full rebuild before re-deriving).
    pub fn clear_edges(&self) -> Result<usize> {
        let n = self.conn.execute("DELETE FROM edges", [])?;
        Ok(n)
    }

    /// Member note paths of a hub: the distinct `src` of every edge whose `dst`
    /// is the hub note path (harvest-clyde-sessions design, Phase 12 - feeds
    /// `cortex hub --synthesize`). Sorted for deterministic synthesis input.
    pub fn hub_members(&self, hub_path: &str) -> Result<Vec<String>> {
        let mut stmt = self
            .conn
            .prepare("SELECT DISTINCT src FROM edges WHERE dst = ?1 ORDER BY src")?;
        let rows = stmt.query_map(params![hub_path], |row| row.get::<_, String>(0))?;
        let mut out = Vec::new();
        for r in rows {
            out.push(r?);
        }
        Ok(out)
    }

    /// Upsert an `entities` row (Phase 3). `id` is the entity slug; `kind` is
    /// `concept`/`creator`/`source`/`tag`; `hub_path` is the stubbed hub note's
    /// vault path (when one exists); `ontotype` is the Phase-5 ontology class.
    pub fn upsert_entity(&self, id: &str, kind: &str, hub_path: Option<&str>, ontotype: Option<&str>) -> Result<()> {
        self.conn.execute(
            "INSERT INTO entities (id, kind, hub_path, ontotype) VALUES (?1, ?2, ?3, ?4)
             ON CONFLICT(id) DO UPDATE SET kind = excluded.kind,
                hub_path = excluded.hub_path, ontotype = excluded.ontotype",
            params![id, kind, hub_path, ontotype],
        )?;
        Ok(())
    }

    /// Count `entities` rows (test/diagnostic helper).
    pub fn count_entities(&self) -> Result<i64> {
        let n: i64 = self
            .conn
            .query_row("SELECT COUNT(*) FROM entities", [], |row| row.get(0))?;
        Ok(n)
    }

    /// Read one entity's `(kind, hub_path, ontotype)` by id (test helper).
    pub fn get_entity(&self, id: &str) -> Result<Option<EntityRow>> {
        optional_row(self.conn.query_row(
            "SELECT kind, hub_path, ontotype FROM entities WHERE id = ?1",
            params![id],
            |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, Option<String>>(1)?,
                    r.get::<_, Option<String>>(2)?,
                ))
            },
        ))
    }

    /// Insert a batch of edges inside a single bounded transaction.
    ///
    /// Enforces the **universal resolve-endpoint-or-skip rule**: any edge whose
    /// `src` OR `dst` is absent from `notes` is skipped (and logged at debug),
    /// never inserted, so neither foreign key can abort the batch. (For
    /// deterministic edges `src` is always the note being processed and exists;
    /// the `src` check matters for Phase-5 `fact` edges whose `src` is an entity
    /// hub that may not be stubbed.) Self-edges (`src == dst`) are likewise
    /// skipped. Returns `(inserted, skipped)`.
    pub fn insert_edges(&mut self, edges: &[Edge]) -> Result<(usize, usize)> {
        let tx = self.conn.transaction()?;
        let mut inserted = 0usize;
        let mut skipped = 0usize;
        {
            let mut exists = tx.prepare("SELECT 1 FROM notes WHERE path = ?1")?;
            let mut ins = tx.prepare(
                "INSERT OR REPLACE INTO edges (src, dst, kind, weight, predicate, src_note)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            )?;
            for edge in edges {
                if edge.src == edge.dst {
                    skipped += 1;
                    continue;
                }
                let endpoints_present = exists.exists(params![edge.src])? && exists.exists(params![edge.dst])?;
                if !endpoints_present {
                    log::debug!(
                        "insert_edges: skipping edge with absent endpoint src={} dst={} kind={}",
                        edge.src,
                        edge.dst,
                        edge.kind
                    );
                    skipped += 1;
                    continue;
                }
                ins.execute(params![
                    edge.src,
                    edge.dst,
                    edge.kind,
                    edge.weight,
                    edge.predicate,
                    edge.src_note,
                ])?;
                inserted += 1;
            }
        }
        tx.commit()?;
        Ok((inserted, skipped))
    }

    /// Read every note's graph-relevant columns from the `notes` table.
    /// `tags` is parsed from the JSON-encoded `tags` column (empty Vec when
    /// absent or unparseable).
    pub fn graph_note_rows(&self) -> Result<Vec<GraphNoteRow>> {
        let mut stmt = self
            .conn
            .prepare("SELECT path, tags, source, creator, domain, body, modified_at, repo, repos_touched FROM notes")?;
        let rows = stmt.query_map([], |row| {
            let path: String = row.get(0)?;
            let tags_json: String = row.get::<_, Option<String>>(1)?.unwrap_or_default();
            let source: String = row.get::<_, Option<String>>(2)?.unwrap_or_default();
            let creator: String = row.get::<_, Option<String>>(3)?.unwrap_or_default();
            let domain: String = row.get::<_, Option<String>>(4)?.unwrap_or_default();
            let body: String = row.get::<_, Option<String>>(5)?.unwrap_or_default();
            let modified_at: i64 = row.get::<_, Option<i64>>(6)?.unwrap_or(0);
            let repo: String = row.get::<_, Option<String>>(7)?.unwrap_or_default();
            // NULL (`None`) and `'[]'` (`Some(vec![])`) both arrive as an empty
            // touched set here; only the populated case drives an extra edge.
            let repos_touched_json: Option<String> = row.get::<_, Option<String>>(8)?;
            Ok((
                path,
                tags_json,
                source,
                creator,
                domain,
                body,
                modified_at,
                repo,
                repos_touched_json,
            ))
        })?;
        let mut out = Vec::new();
        for r in rows {
            let (path, tags_json, source, creator, domain, body, modified_at, repo, repos_touched_json) = r?;
            let tags: Vec<String> = match serde_json::from_str(&tags_json) {
                Ok(t) => t,
                Err(e) => {
                    log::warn!("graph_note_rows: unparseable tags JSON for {path}, treating as no tags: {e}");
                    Vec::new()
                }
            };
            let repos_touched: Vec<String> = match repos_touched_json.as_deref() {
                None | Some("") => Vec::new(),
                Some(json) => match serde_json::from_str(json) {
                    Ok(v) => v,
                    Err(e) => {
                        log::warn!(
                            "graph_note_rows: unparseable repos_touched JSON for {path}, treating as no touched repos: {e}"
                        );
                        Vec::new()
                    }
                },
            };
            out.push(GraphNoteRow {
                path,
                tags,
                source,
                creator,
                domain,
                body,
                modified_at,
                repo,
                repos_touched,
            });
        }
        Ok(out)
    }

    /// Note paths whose content (body/frontmatter) changed since their
    /// content edges were last built: `notes.modified_at > content_built_at`
    /// (a note with no `edge_build_state` row defaults to 0, so every new note
    /// is a target). Drives the wikilink/shared-tag/metadata incremental
    /// trigger.
    pub fn content_edge_targets(&self) -> Result<Vec<String>> {
        let mut stmt = self.conn.prepare(
            "SELECT n.path FROM notes n
             LEFT JOIN edge_build_state s ON s.note_path = n.path
             WHERE n.modified_at > COALESCE(s.content_built_at, 0)",
        )?;
        let rows = stmt
            .query_map([], |row| row.get::<_, String>(0))?
            .filter_map(warn_row)
            .collect();
        Ok(rows)
    }

    /// Record that `path`'s edges were just rebuilt: persist the source
    /// timestamps that selected it so it is not reprocessed until it changes
    /// again. `content_built_at` is the note's `modified_at`; `semantic_built_at`
    /// is its newest summary-embedding `produced_at` (0 when unembedded).
    pub fn record_edge_build(&self, path: &str, content_built_at: i64, semantic_built_at: i64) -> Result<()> {
        self.conn.execute(
            "INSERT INTO edge_build_state (note_path, content_built_at, semantic_built_at)
             VALUES (?1, ?2, ?3)
             ON CONFLICT(note_path) DO UPDATE SET
                content_built_at = excluded.content_built_at,
                semantic_built_at = excluded.semantic_built_at",
            params![path, content_built_at, semantic_built_at],
        )?;
        Ok(())
    }

    /// Insert a note row with full graph-relevant fields, for graph tests in
    /// other crates (cortex). `tags` is JSON-encoded into the `tags` column so
    /// `graph_note_rows` parses it back; `summary` is set to `body` so the
    /// FTS5 path stays populated.
    pub fn insert_test_note_graph(
        &self,
        path: &str,
        tags: &[&str],
        source: &str,
        creator: &str,
        domain: &str,
        body: &str,
        modified_at: i64,
    ) -> Result<()> {
        let tags_json = serde_json::to_string(tags).unwrap_or_else(|_| "[]".to_string());
        self.conn.execute(
            "INSERT INTO notes (path, title, domain, note_type, origin, status, date, tags, source, creator, body, summary, modified_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)",
            params![
                path, "T", domain, "article", "assisted", "", "2026-06-05",
                tags_json, source, creator, body, body, modified_at,
            ],
        )?;
        Ok(())
    }

    /// Delete a `notes` row by path, for tests in other crates that need to
    /// simulate `index_vault` dropping a deleted note (and exercise the edge
    /// `ON DELETE CASCADE`). Production deletion goes through `index_vault`'s
    /// `remove_stale_notes`, never this.
    pub fn delete_note_for_test(&self, path: &str) -> Result<()> {
        self.conn.execute("DELETE FROM notes WHERE path = ?1", params![path])?;
        Ok(())
    }

    /// All `fact` edges (Phase 5), for consolidation passes. Ordered by
    /// `(src, predicate)` so contradiction detection can group functional
    /// predicates with multiple distinct objects.
    pub fn fact_edges(&self) -> Result<Vec<FactEdge>> {
        let mut stmt = self.conn.prepare(
            "SELECT src, dst, predicate, src_note FROM edges WHERE kind = 'fact'
             ORDER BY src, predicate, dst",
        )?;
        let rows = stmt
            .query_map([], |r| {
                Ok(FactEdge {
                    src: r.get(0)?,
                    dst: r.get(1)?,
                    predicate: r.get(2)?,
                    src_note: r.get(3)?,
                })
            })?
            .filter_map(warn_row)
            .collect();
        Ok(rows)
    }

    /// Delete one specific `fact` edge (noise removal). Keyed on the full PK
    /// (src, dst, kind='fact', predicate).
    pub fn delete_fact_edge(&self, src: &str, dst: &str, predicate: &str) -> Result<usize> {
        let n = self.conn.execute(
            "DELETE FROM edges WHERE src = ?1 AND dst = ?2 AND kind = 'fact' AND predicate = ?3",
            params![src, dst, predicate],
        )?;
        Ok(n)
    }

    /// Note paths that have NO incident edge of any kind (fully isolated in the
    /// graph). Cluster bridging targets these. Excludes nothing else.
    pub fn notes_without_edges(&self) -> Result<Vec<String>> {
        let mut stmt = self.conn.prepare(
            "SELECT n.path FROM notes n
             WHERE NOT EXISTS (SELECT 1 FROM edges e WHERE e.src = n.path OR e.dst = n.path)",
        )?;
        let rows = stmt
            .query_map([], |r| r.get::<_, String>(0))?
            .filter_map(warn_row)
            .collect();
        Ok(rows)
    }

    /// Count edges, optionally filtered by `kind`. Test/diagnostic helper.
    pub fn count_edges(&self, kind: Option<&str>) -> Result<i64> {
        let count: i64 = match kind {
            Some(k) => self
                .conn
                .query_row("SELECT COUNT(*) FROM edges WHERE kind = ?1", params![k], |row| {
                    row.get(0)
                })?,
            None => self
                .conn
                .query_row("SELECT COUNT(*) FROM edges", [], |row| row.get(0))?,
        };
        Ok(count)
    }

    /// Expand a seed set up to `hops` hops along `edges`, returning one
    /// [`GraphReach`] per path that reaches a non-seed neighbor.
    ///
    /// A neighbor is the endpoint of a matching edge that is not currently in
    /// the frontier; the query matches `src IN frontier OR dst IN frontier` so
    /// every edge is traversed undirected regardless of which way it was
    /// written. `edge_kinds = None` matches all kinds; `min_weight` filters on
    /// `weight >= min_weight`. Seeds themselves are never returned. Multi-hop
    /// `weight` is the product of edge weights along the path; `origin_seed`
    /// traces back to the seed that started the path.
    pub fn expand_graph(
        &self,
        seed_paths: &[String],
        hops: u8,
        edge_kinds: Option<&[String]>,
        min_weight: f32,
    ) -> Result<Vec<GraphReach>> {
        use std::collections::HashSet;

        log::debug!(
            "expand_graph: seeds={} hops={} edge_kinds={:?} min_weight={}",
            seed_paths.len(),
            hops,
            edge_kinds,
            min_weight
        );

        let seed_set: HashSet<String> = seed_paths.iter().cloned().collect();
        let mut visited: HashSet<String> = seed_set.clone();
        let mut reaches: Vec<GraphReach> = Vec::new();

        // Frontier carries (path, origin_seed, accumulated_weight). Seeds start
        // as their own origin with unit accumulated weight.
        let mut frontier: Vec<(String, String, f32)> =
            seed_paths.iter().map(|p| (p.clone(), p.clone(), 1.0_f32)).collect();

        for hop in 1..=hops {
            let mut next: Vec<(String, String, f32)> = Vec::new();
            // Dedup the frontier AT PUSH TIME within this hop: a node reachable
            // from several current-frontier nodes was previously pushed once per
            // reaching edge (visited was only updated AFTER the hop), so the next
            // hop expanded it multiple times - multiplicative waste in dense
            // regions. The first reach wins its frontier slot (and its weight).
            let mut next_set: HashSet<String> = HashSet::new();
            for (node, origin, acc) in &frontier {
                let neighbors = self.edge_neighbors(node, edge_kinds, min_weight)?;
                for (neighbor, kind, weight, predicate) in neighbors {
                    if seed_set.contains(&neighbor) {
                        continue;
                    }
                    let path_weight = acc * weight;
                    reaches.push(GraphReach {
                        path: neighbor.clone(),
                        origin_seed: origin.clone(),
                        hop,
                        weight: path_weight,
                        kind,
                        predicate,
                    });
                    if !visited.contains(&neighbor) && next_set.insert(neighbor.clone()) {
                        next.push((neighbor.clone(), origin.clone(), path_weight));
                    }
                }
            }
            for (node, _, _) in &next {
                visited.insert(node.clone());
            }
            frontier = next;
            if frontier.is_empty() {
                break;
            }
        }

        log::debug!("expand_graph: produced {} reach(es)", reaches.len());
        Ok(reaches)
    }

    /// One indexed lookup over `idx_edges_src`/`idx_edges_dst`: every edge
    /// incident to `node`, returning the *other* endpoint, the edge kind, its
    /// weight, and predicate.
    fn edge_neighbors(
        &self,
        node: &str,
        edge_kinds: Option<&[String]>,
        min_weight: f32,
    ) -> Result<Vec<(String, String, f32, String)>> {
        let mut sql = String::from(
            "SELECT src, dst, kind, weight, predicate FROM edges
             WHERE (src = ?1 OR dst = ?1) AND weight >= ?2",
        );
        let mut params_vec: Vec<Box<dyn rusqlite::types::ToSql>> =
            vec![Box::new(node.to_string()), Box::new(min_weight as f64)];
        if let Some(kinds) = edge_kinds
            && !kinds.is_empty()
        {
            // A filter value matches either the edge `kind` (e.g. "semantic",
            // "fact") or, for Phase-5 typed edges, the `predicate` (e.g.
            // "uses", "released-on") — so callers can target a relation
            // directly. Each value is bound twice (kind list and predicate list).
            let kind_ph: Vec<String> = (0..kinds.len()).map(|i| format!("?{}", i + 3)).collect();
            let pred_ph: Vec<String> = (0..kinds.len()).map(|i| format!("?{}", i + 3 + kinds.len())).collect();
            sql.push_str(&format!(
                " AND (kind IN ({}) OR predicate IN ({}))",
                kind_ph.join(", "),
                pred_ph.join(", ")
            ));
            for k in kinds {
                params_vec.push(Box::new(k.clone()));
            }
            for k in kinds {
                params_vec.push(Box::new(k.clone()));
            }
        }

        let mut stmt = self.conn.prepare(&sql)?;
        let param_refs: Vec<&dyn rusqlite::types::ToSql> = params_vec.iter().map(|p| p.as_ref()).collect();
        let rows = stmt.query_map(param_refs.as_slice(), |row| {
            let src: String = row.get(0)?;
            let dst: String = row.get(1)?;
            let kind: String = row.get(2)?;
            let weight: f64 = row.get(3)?;
            let predicate: String = row.get(4)?;
            Ok((src, dst, kind, weight as f32, predicate))
        })?;

        let mut out = Vec::new();
        for r in rows {
            let (src, dst, kind, weight, predicate) = r?;
            let neighbor = if src == node { dst } else { src };
            out.push((neighbor, kind, weight, predicate));
        }
        Ok(out)
    }
}

// Tests reuse the `insert_test_note_*` helpers, which live in the
// vec-gated `vector` module; gate the test module to match so a non-vec
// build still compiles cleanly. CI builds `--features vec`.
#[cfg(all(test, feature = "vec"))]
mod tests;
