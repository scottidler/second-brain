use super::*;

impl super::SearchIndex {
    /// Full-text search across notes
    pub fn search(
        &self,
        query: &str,
        domain: Option<&str>,
        note_type: Option<&str>,
        status: Option<&str>,
        limit: Option<u32>,
    ) -> Result<Vec<NoteRow>> {
        log::debug!(
            "search::search: query={query} domain={domain:?} note_type={note_type:?} status={status:?} limit={limit:?}"
        );
        let limit = limit.unwrap_or(20);

        let mut sql = String::from(
            "SELECT n.path, n.title, n.domain, n.note_type, n.origin, n.status, n.date, n.tags, n.source, n.creator, n.body, n.summary, n.trace, n.ingested, n.trace_expires
             FROM notes n
             JOIN notes_fts f ON n.rowid = f.rowid
             WHERE notes_fts MATCH ?1",
        );
        let mut param_values: Vec<Box<dyn rusqlite::types::ToSql>> = vec![Box::new(query.to_string())];
        let mut param_idx = 2;

        if let Some(d) = domain {
            sql.push_str(&format!(" AND n.domain = ?{param_idx}"));
            param_values.push(Box::new(d.to_string()));
            param_idx += 1;
        }
        if let Some(t) = note_type {
            sql.push_str(&format!(" AND n.note_type = ?{param_idx}"));
            param_values.push(Box::new(t.to_string()));
            param_idx += 1;
        }
        if let Some(s) = status {
            sql.push_str(&format!(" AND n.status = ?{param_idx}"));
            param_values.push(Box::new(s.to_string()));
            param_idx += 1;
        }
        let _ = param_idx;

        sql.push_str(&format!(" ORDER BY rank LIMIT {limit}"));

        let params_refs: Vec<&dyn rusqlite::types::ToSql> = param_values.iter().map(|p| p.as_ref()).collect();
        let mut stmt = self.conn.prepare(&sql)?;
        let rows = stmt
            .query_map(params_refs.as_slice(), NoteRow::from_row)?
            .filter_map(warn_row)
            .collect();

        Ok(rows)
    }

    /// Find notes most similar to the given content using FTS5 term matching
    pub fn find_similar(&self, content: &str, limit: usize) -> Result<Vec<NoteRow>> {
        // Extract significant words from content for FTS5 query
        let terms = extract_search_terms(content, 20);
        if terms.is_empty() {
            return Ok(vec![]);
        }

        // Build OR query from extracted terms
        let fts_query = terms.join(" OR ");

        self.search(&fts_query, None, None, None, Some(limit as u32))
    }

    /// List notes with optional filters (no full-text search)
    pub fn list_notes(
        &self,
        domain: Option<&str>,
        note_type: Option<&str>,
        status: Option<&str>,
        after: Option<&str>,
        before: Option<&str>,
        limit: Option<u32>,
    ) -> Result<Vec<NoteRow>> {
        log::debug!(
            "search::list_notes: domain={domain:?} note_type={note_type:?} status={status:?} after={after:?} before={before:?} limit={limit:?}"
        );
        let limit = limit.unwrap_or(50);
        let mut sql = String::from(
            "SELECT path, title, domain, note_type, origin, status, date, tags, source, creator, body, summary, trace, ingested, trace_expires
             FROM notes WHERE 1=1",
        );
        let mut param_values: Vec<Box<dyn rusqlite::types::ToSql>> = vec![];
        let mut param_idx = 1;

        if let Some(d) = domain {
            sql.push_str(&format!(" AND domain = ?{param_idx}"));
            param_values.push(Box::new(d.to_string()));
            param_idx += 1;
        }
        if let Some(t) = note_type {
            sql.push_str(&format!(" AND note_type = ?{param_idx}"));
            param_values.push(Box::new(t.to_string()));
            param_idx += 1;
        }
        if let Some(s) = status {
            sql.push_str(&format!(" AND status = ?{param_idx}"));
            param_values.push(Box::new(s.to_string()));
            param_idx += 1;
        }
        if let Some(a) = after {
            sql.push_str(&format!(" AND date >= ?{param_idx}"));
            param_values.push(Box::new(a.to_string()));
            param_idx += 1;
        }
        if let Some(b) = before {
            sql.push_str(&format!(" AND date <= ?{param_idx}"));
            param_values.push(Box::new(b.to_string()));
            param_idx += 1;
        }
        let _ = param_idx;

        sql.push_str(&format!(" ORDER BY date DESC LIMIT {limit}"));

        let params_refs: Vec<&dyn rusqlite::types::ToSql> = param_values.iter().map(|p| p.as_ref()).collect();
        let mut stmt = self.conn.prepare(&sql)?;
        let rows = stmt
            .query_map(params_refs.as_slice(), NoteRow::from_row)?
            .filter_map(warn_row)
            .collect();

        Ok(rows)
    }

    /// Get a single note by path
    pub fn get_note(&self, path: &str) -> Result<Option<NoteRow>> {
        optional_row(self.conn.query_row(
            "SELECT path, title, domain, note_type, origin, status, date, tags, source, creator, body, summary, trace, ingested, trace_expires
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

    /// Get recent notes across the vault, optionally filtered by domain and/or note type
    pub fn recent_notes(
        &self,
        days: Option<u32>,
        domain: Option<&str>,
        note_type: Option<&str>,
        limit: Option<u32>,
    ) -> Result<Vec<NoteRow>> {
        let days = days.unwrap_or(7);
        let limit = limit.unwrap_or(20);

        let cutoff = chrono::Local::now()
            .date_naive()
            .checked_sub_days(chrono::Days::new(u64::from(days)))
            .map(|d| d.format("%Y-%m-%d").to_string())
            .unwrap_or_default();

        self.list_notes(domain, note_type, None, Some(&cutoff), None, Some(limit))
    }

    /// Find outbound wikilinks from a note's body
    pub fn find_outbound_links(&self, path: &str) -> Result<Vec<OutboundLink>> {
        let note = self.get_note(path)?;
        let body = match note {
            Some(n) => n.body,
            None => return Ok(vec![]),
        };

        let targets = extract_wikilinks(&body);
        let mut links = Vec::new();

        for target in targets {
            // Try to resolve the target to an actual note path
            let resolved = self.resolve_wikilink(&target)?;
            links.push(OutboundLink {
                target: target.clone(),
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
            "SELECT path, title, domain, note_type, origin, status, date, tags, source, creator, body, summary, trace, ingested, trace_expires
             FROM notes WHERE body LIKE ?1",
        )?;

        let pattern = format!("%[[{stem}%");
        let rows: Vec<NoteRow> = stmt
            .query_map(params![pattern], NoteRow::from_row)?
            .filter_map(warn_row)
            .filter(|note| {
                // Verify with exact wikilink parsing
                let links = extract_wikilinks(&note.body);
                links.iter().any(|l| l.eq_ignore_ascii_case(stem))
            })
            .collect();

        Ok(rows)
    }

    /// Find notes with no inbound links (orphans)
    pub fn orphan_notes(&self, limit: Option<u32>) -> Result<Vec<NoteRow>> {
        let limit = limit.unwrap_or(50);

        // Get all notes
        let mut stmt = self.conn.prepare(
            "SELECT path, title, domain, note_type, origin, status, date, tags, source, creator, body, summary, trace, ingested, trace_expires
             FROM notes ORDER BY date DESC",
        )?;
        let all_notes: Vec<NoteRow> = stmt.query_map([], NoteRow::from_row)?.filter_map(warn_row).collect();

        // Collect all wikilink targets across the vault
        let mut linked_stems: std::collections::HashSet<String> = std::collections::HashSet::new();
        for note in &all_notes {
            for link in extract_wikilinks(&note.body) {
                linked_stems.insert(link.to_lowercase());
            }
        }

        // Notes whose stem is never referenced
        let orphans: Vec<NoteRow> = all_notes
            .into_iter()
            .filter(|note| {
                let stem = Path::new(&note.path)
                    .file_stem()
                    .and_then(|s| s.to_str())
                    .unwrap_or("")
                    .to_lowercase();
                !linked_stems.contains(&stem)
            })
            .take(limit as usize)
            .collect();

        Ok(orphans)
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
