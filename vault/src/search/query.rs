use super::filter::Filter;
use super::*;
use rusqlite::types::Value;

/// First-occurrence dedup of a tag list, order preserved. Shared by the
/// filter builder and `index_one` so the facet rows, the JSON column, and
/// the AND-mode count all agree on what "N tags" means.
pub(crate) fn dedup_tags(tags: &[String]) -> Vec<&String> {
    let mut seen = std::collections::HashSet::with_capacity(tags.len());
    tags.iter().filter(|t| seen.insert(t.as_str())).collect()
}

impl super::SearchIndex {
    /// Full-text search across notes
    pub fn search(
        &self,
        query: &str,
        tags: Option<&[String]>,
        tags_all: bool,
        note_type: Option<&str>,
        status: Option<&str>,
        limit: Option<u32>,
    ) -> Result<Vec<NoteRow>> {
        log::debug!(
            "search::search: query={query} tags={tags:?} tags_all={tags_all} note_type={note_type:?} status={status:?} limit={limit:?}"
        );
        let limit = limit.unwrap_or(20);

        let f = search_filter(query, tags, tags_all, note_type, status).then(&format!(" ORDER BY rank LIMIT {limit}"));

        let mut stmt = self.conn.prepare(&f.sql)?;
        // Collect with propagation, NOT `filter_map(warn_row)`: sqlite reports a
        // malformed MATCH expression on the first step, so swallowing per row
        // turns "your query is invalid" into "there are no results" - a
        // fail-open search. Callers that can tolerate no results (classify's
        // similarity lookup) decide that for themselves.
        let rows = stmt
            .query_map(rusqlite::params_from_iter(&f.params), NoteRow::from_row)?
            .collect::<rusqlite::Result<Vec<NoteRow>>>()
            .wrap_err_with(|| format!("fts5 search failed for query {query:?}"))?;

        Ok(rows)
    }

    /// Find notes most similar to the given content using FTS5 term matching
    pub fn find_similar(&self, content: &str, limit: usize) -> Result<Vec<NoteRow>> {
        // Extract significant words from content for FTS5 query
        let terms = extract_search_terms(content, 20);
        if terms.is_empty() {
            return Ok(vec![]);
        }

        // Build OR query from extracted terms. Each term is QUOTED: they are
        // literals harvested from note bodies, so slugs, UUIDs and hyphenated
        // names are the norm, and an unquoted one takes the whole MATCH down.
        let fts_query = terms.iter().map(|t| fts_quote(t)).collect::<Vec<_>>().join(" OR ");

        self.search(&fts_query, None, false, None, None, Some(limit as u32))
    }

    /// `find_similar` for callers that treat "no similar notes" and "the query
    /// blew up" the same way (cortex's classify context). Logs the error at
    /// ERROR and yields an empty list, so the degradation is visible in the
    /// journal instead of looking like a vault with nothing similar in it.
    pub fn find_similar_lossy(&self, content: &str, limit: usize) -> Vec<NoteRow> {
        match self.find_similar(content, limit) {
            Ok(rows) => rows,
            Err(e) => {
                log::error!("find_similar failed, continuing with no similarity context: {e:#}");
                Vec::new()
            }
        }
    }

    /// List notes with optional filters (no full-text search)
    pub fn list_notes(
        &self,
        tags: Option<&[String]>,
        tags_all: bool,
        note_type: Option<&str>,
        status: Option<&str>,
        after: Option<&str>,
        before: Option<&str>,
        limit: Option<u32>,
    ) -> Result<Vec<NoteRow>> {
        log::debug!(
            "search::list_notes: tags={tags:?} tags_all={tags_all} note_type={note_type:?} status={status:?} after={after:?} before={before:?} limit={limit:?}"
        );
        let limit = limit.unwrap_or(50);
        let f = list_notes_filter(tags, tags_all, note_type, status, after, before)
            .then(&format!(" ORDER BY date DESC LIMIT {limit}"));

        let mut stmt = self.conn.prepare(&f.sql)?;
        let rows = stmt
            .query_map(rusqlite::params_from_iter(&f.params), NoteRow::from_row)?
            .filter_map(warn_row)
            .collect();

        Ok(rows)
    }

    /// Get a single note by path
    pub fn get_note(&self, path: &str) -> Result<Option<NoteRow>> {
        optional_row(self.conn.query_row(
            "SELECT path, title, note_type, origin, status, date, tags, source, creator, body, summary, trace, ingested, trace_expires
                 FROM notes WHERE path = ?1",
            params![path],
            NoteRow::from_row,
        ))
    }

    /// Read the Doc 3 signal triple for `path`: `(search_hit_count,
    /// last_accessed_at, inbound_link_count)`. Returns `None` if the path
    /// is not in the index. Used by callers that need to observe signal
    /// state without joining on the full row (e.g. tests, future
    /// signal-aware tooling).
    pub fn note_signals(&self, path: &str) -> Result<Option<(i64, Option<i64>, i64)>> {
        optional_row(self.conn.query_row(
            "SELECT search_hit_count, last_accessed_at, inbound_link_count
                 FROM notes WHERE path = ?1",
            params![path],
            |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, Option<i64>>(1)?,
                    row.get::<_, i64>(2)?,
                ))
            },
        ))
    }

    /// Get recent notes across the vault, optionally filtered by tags and/or note type
    pub fn recent_notes(
        &self,
        days: Option<u32>,
        tags: Option<&[String]>,
        tags_all: bool,
        note_type: Option<&str>,
        limit: Option<u32>,
    ) -> Result<Vec<NoteRow>> {
        log::debug!(
            "search::recent_notes: days={days:?} tags={tags:?} tags_all={tags_all} note_type={note_type:?} limit={limit:?}"
        );
        let days = days.unwrap_or(7);
        let limit = limit.unwrap_or(20);

        let cutoff = chrono::Local::now()
            .date_naive()
            .checked_sub_days(chrono::Days::new(u64::from(days)))
            .map(|d| d.format("%Y-%m-%d").to_string())
            .unwrap_or_default();

        self.list_notes(tags, tags_all, note_type, None, Some(&cutoff), None, Some(limit))
    }

    /// Find outbound wikilinks from a note's body
    pub fn find_outbound_links(&self, path: &str) -> Result<Vec<OutboundLink>> {
        let note = self.get_note(path)?;
        let body = match note {
            Some(n) => n.body,
            None => return Ok(vec![]),
        };

        let mut paths_stmt = self.conn.prepare("SELECT path FROM notes ORDER BY path")?;
        let all_paths: Vec<String> = paths_stmt
            .query_map([], |row| row.get(0))?
            .collect::<rusqlite::Result<_>>()?;
        let resolver = crate::wikilink::Resolver::new(all_paths);
        let mut links = Vec::new();

        // A same-note link (`[[#h]]`, empty target) points nowhere to resolve.
        for link in crate::wikilink::parse(&body).filter(|l| !l.target.is_empty()) {
            // Try to resolve the target to an actual note path
            let resolved = resolver.resolve(link.target).next().map(str::to_string);
            links.push(OutboundLink {
                target: link.target.to_string(),
                resolved_path: resolved.clone(),
                exists: resolved.is_some(),
            });
        }

        Ok(links)
    }

    /// Find notes that link TO the given note (inbound links)
    pub fn find_inbound_links(&self, path: &str) -> Result<Vec<NoteRow>> {
        // Extract the stem from the path (filename without extension)
        let stem = Path::new(path).file_stem().and_then(|s| s.to_str()).unwrap_or(path);

        let mut stmt = self.conn.prepare(
            "SELECT path, title, note_type, origin, status, date, tags, source, creator, body, summary, trace, ingested, trace_expires
             FROM notes WHERE body LIKE ?1",
        )?;

        // Not `%[[{stem}%`: that misses `[[dir/{stem}]]`. The prefilter only
        // narrows the scan; `Resolver` below decides what is really a link.
        let pattern = format!("%{stem}%");
        let resolver = crate::wikilink::Resolver::new([path]);
        let rows: Vec<NoteRow> = stmt
            .query_map(params![pattern], NoteRow::from_row)?
            .filter_map(warn_row)
            .filter(|note| crate::wikilink::parse(&note.body).any(|l| resolver.resolve(l.target).next().is_some()))
            .collect();

        Ok(rows)
    }

    /// Try to resolve a wikilink target to an actual note path in the index
    pub(crate) fn resolve_wikilink(&self, target: &str) -> Result<Option<String>> {
        // Try exact path match first
        let row: Option<String> = optional_row(self.conn.query_row(
            "SELECT path FROM notes WHERE path = ?1",
            params![target],
            |row| row.get(0),
        ))?;
        if row.is_some() {
            return Ok(row);
        }

        // Try matching by stem (filename without extension)
        let target_lower = target.to_lowercase();
        let row: Option<String> = optional_row(self.conn.query_row(
            "SELECT path FROM notes WHERE LOWER(path) LIKE ?1 LIMIT 1",
            params![format!("%/{target_lower}.md")],
            |row| row.get(0),
        ))?;
        if row.is_some() {
            return Ok(row);
        }

        // Try matching just the stem anywhere
        optional_row(self.conn.query_row(
            "SELECT path FROM notes WHERE LOWER(path) LIKE ?1 LIMIT 1",
            params![format!("%{target_lower}%")],
            |row| row.get(0),
        ))
    }
}

const SEARCH_SELECT: &str = "SELECT n.path, n.title, n.note_type, n.origin, n.status, n.date, n.tags, n.source, n.creator, n.body, n.summary, n.trace, n.ingested, n.trace_expires
             FROM notes n
             JOIN notes_fts f ON n.rowid = f.rowid
             WHERE notes_fts MATCH ?";

const LIST_SELECT: &str = "SELECT path, title, note_type, origin, status, date, tags, source, creator, body, summary, trace, ingested, trace_expires
             FROM notes WHERE 1=1";

/// `search`'s statement up to (not including) `ORDER BY`/`LIMIT`.
pub(super) fn search_filter(
    query: &str,
    tags: Option<&[String]>,
    tags_all: bool,
    note_type: Option<&str>,
    status: Option<&str>,
) -> Filter {
    Filter::new(SEARCH_SELECT, vec![Value::Text(query.to_string())])
        .and_tags("n", tags, tags_all)
        .and_eq("n.note_type", note_type)
        .and_eq("n.status", status)
}

/// `list_notes`'s statement up to (not including) `ORDER BY`/`LIMIT`.
pub(super) fn list_notes_filter(
    tags: Option<&[String]>,
    tags_all: bool,
    note_type: Option<&str>,
    status: Option<&str>,
    after: Option<&str>,
    before: Option<&str>,
) -> Filter {
    Filter::new(LIST_SELECT, vec![])
        .and_tags("notes", tags, tags_all)
        .and_eq("note_type", note_type)
        .and_eq("status", status)
        .and_cmp("date", ">=", after)
        .and_cmp("date", "<=", before)
}
