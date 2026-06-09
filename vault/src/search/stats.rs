use super::*;

impl super::SearchIndex {
    /// Walk every note's body, count wikilink targets, materialize the
    /// `inbound_link_count` column for every row. Idempotent, bounded by
    /// vault size, single pass.
    ///
    /// **Key normalization is symmetric**: HashMap keys are
    /// `target.to_ascii_lowercase()` (taking the last `/`-segment so that
    /// `[[folder/note]]` matches a row whose path stem is `note`); the
    /// per-row lookup key is `file_stem(path).to_ascii_lowercase()`. Both
    /// sides are lowercased before the lookup, so any case parity is
    /// automatic. Anything that compares stems without lowercasing first
    /// is a bug.
    ///
    /// Self-links are NOT counted: a note whose body contains `[[self]]`
    /// gets no structural credit for it.
    ///
    /// **Sole intended caller: oracle's 10-minute periodic background
    /// task.** Must NOT be called from `index_vault` / the watcher path:
    /// the watcher fires sub-second on every Obsidian auto-save, and at
    /// three-year scale a full-table wikilink scan holding the SearchIndex
    /// mutex would block every concurrent `note_read` / `knowledge_search`.
    ///
    /// Returns the number of rows whose stored count changed.
    pub fn recompute_inbound_link_counts(&mut self) -> Result<usize> {
        log::debug!("recompute_inbound_link_counts: scanning vault");

        let rows: Vec<(String, String, i64)> = {
            let mut stmt = self.conn.prepare("SELECT path, body, inbound_link_count FROM notes")?;
            let mapped = stmt.query_map([], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, i64>(2)?,
                ))
            })?;
            mapped.filter_map(|r| r.ok()).collect()
        };

        let mut counts: HashMap<String, u64> = HashMap::new();
        for (path, body, _stored) in &rows {
            let source_stem = Path::new(path)
                .file_stem()
                .and_then(|s| s.to_str())
                .unwrap_or("")
                .to_ascii_lowercase();
            for raw_target in extract_wikilinks(body) {
                // `[[folder/note]]` -> "note"; everything is lowercased so
                // the per-row lookup key matches symmetrically.
                let target_stem = raw_target.rsplit('/').next().unwrap_or("").to_ascii_lowercase();
                if target_stem.is_empty() {
                    continue;
                }
                if target_stem == source_stem {
                    // Self-link: no structural signal.
                    continue;
                }
                *counts.entry(target_stem).or_insert(0) += 1;
            }
        }

        let tx = self.conn.transaction()?;
        let mut changed: usize = 0;
        {
            let mut stmt = tx.prepare("UPDATE notes SET inbound_link_count = ?1 WHERE path = ?2")?;
            for (path, _body, stored) in &rows {
                let row_stem = Path::new(path)
                    .file_stem()
                    .and_then(|s| s.to_str())
                    .unwrap_or("")
                    .to_ascii_lowercase();
                let new_count = *counts.get(&row_stem).unwrap_or(&0) as i64;
                if new_count != *stored {
                    stmt.execute(params![new_count, path])?;
                    changed += 1;
                }
            }
        }
        tx.commit()?;

        log::debug!(
            "recompute_inbound_link_counts: scanned={} changed={}",
            rows.len(),
            changed
        );
        Ok(changed)
    }

    /// Increment `search_hit_count` and stamp `last_accessed_at = now` for `path`.
    ///
    /// **Sole intended caller: `oracle::note_read`.** Counting `knowledge_search`
    /// matches as access would create a positive-feedback loop where high-BM25-
    /// scoring notes become immortal and the entire decay premise collapses
    /// (parent roadmap, decay-signals section). The
    /// `knowledge_search_does_not_bump_access` oracle test is the load-bearing
    /// regression guard for this rule.
    ///
    /// Best-effort signal: a missing row (the note was deleted between read
    /// and bump) results in `rows_affected = 0` and `Ok(())`; not surfaced.
    pub fn bump_access(&self, path: &str) -> Result<()> {
        log::debug!("bump_access: path={path}");
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs() as i64;
        let updated = self.conn.execute(
            "UPDATE notes
                SET search_hit_count = search_hit_count + 1,
                    last_accessed_at = ?2
              WHERE path = ?1",
            params![path, now],
        )?;
        if updated == 0 {
            log::trace!("bump_access: path={path} not present in index, ignored");
        }
        Ok(())
    }

    /// Get vault statistics including schema gaps
    pub fn stats(&self) -> Result<VaultStats> {
        let total: u64 = self
            .conn
            .query_row("SELECT COUNT(*) FROM notes", [], |row| row.get(0))?;

        let domain_counts = self.count_by_column("domain")?;
        let type_counts = self.count_by_column("note_type")?;
        let status_counts = self.count_by_column("status")?;

        let schema_gaps = self.compute_schema_gaps()?;

        Ok(VaultStats {
            total_notes: total,
            by_domain: domain_counts,
            by_type: type_counts,
            by_status: status_counts,
            schema_gaps,
        })
    }

    /// Coverage of the `note_embeddings` table relative to `notes`. Used by
    /// `sb status` / `sb doctor` to surface how many notes have been embedded.
    pub fn embedding_coverage(&self) -> Result<EmbeddingCoverage> {
        let total_notes: u64 = self
            .conn
            .query_row("SELECT COUNT(*) FROM notes", [], |row| row.get(0))?;
        let embedded_notes: u64 =
            self.conn
                .query_row("SELECT COUNT(DISTINCT note_path) FROM note_embeddings", [], |row| {
                    row.get(0)
                })?;
        Ok(EmbeddingCoverage {
            total_notes,
            embedded_notes,
        })
    }

    fn count_by_column(&self, column: &str) -> Result<Vec<(String, u64)>> {
        let sql = format!(
            "SELECT {column}, COUNT(*) as cnt FROM notes WHERE {column} != '' GROUP BY {column} ORDER BY cnt DESC"
        );
        let mut stmt = self.conn.prepare(&sql)?;
        let rows = stmt
            .query_map([], |row| Ok((row.get::<_, String>(0)?, row.get::<_, u64>(1)?)))?
            .filter_map(|r| r.ok())
            .collect();
        Ok(rows)
    }

    fn compute_schema_gaps(&self) -> Result<Vec<(String, u64)>> {
        let fields = ["domain", "note_type", "origin", "status"];
        let mut gaps = Vec::new();
        for field in fields {
            let count: u64 =
                self.conn
                    .query_row(&format!("SELECT COUNT(*) FROM notes WHERE {field} = ''"), [], |row| {
                        row.get(0)
                    })?;
            if count > 0 {
                gaps.push((field.to_string(), count));
            }
        }
        Ok(gaps)
    }

    /// Get notes for a specific domain with stats
    pub fn domain_brief(&self, domain: &str, limit: Option<u32>) -> Result<DomainBrief> {
        let limit = limit.unwrap_or(10);

        let total: u64 =
            self.conn
                .query_row("SELECT COUNT(*) FROM notes WHERE domain = ?1", params![domain], |row| {
                    row.get(0)
                })?;

        let unread: u64 = self.conn.query_row(
            "SELECT COUNT(*) FROM notes WHERE domain = ?1 AND status = 'unread'",
            params![domain],
            |row| row.get(0),
        )?;

        let starred: u64 = self.conn.query_row(
            "SELECT COUNT(*) FROM notes WHERE domain = ?1 AND status = 'starred'",
            params![domain],
            |row| row.get(0),
        )?;

        let type_counts = {
            let mut stmt = self.conn.prepare(
                "SELECT note_type, COUNT(*) FROM notes WHERE domain = ?1 AND note_type != '' GROUP BY note_type ORDER BY COUNT(*) DESC",
            )?;
            stmt.query_map(params![domain], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, u64>(1)?))
            })?
            .filter_map(|r| r.ok())
            .collect()
        };

        let recent_notes = self.list_notes(Some(domain), None, None, None, None, Some(limit))?;

        Ok(DomainBrief {
            domain: domain.to_string(),
            total_notes: total,
            unread,
            starred,
            by_type: type_counts,
            recent: recent_notes,
        })
    }

    /// Get domain distribution: how many notes per domain
    pub fn domain_stats(&self) -> Result<HashMap<String, u64>> {
        let counts = self.count_by_column("domain")?;
        Ok(counts.into_iter().collect())
    }

    /// Get tag-domain correlation: for each tag, which domains it appears in and how often.
    /// Returns a map of tag -> (domain -> count).
    pub fn tag_domain_map(&self) -> Result<HashMap<String, HashMap<String, u64>>> {
        let mut stmt = self
            .conn
            .prepare("SELECT tags, domain FROM notes WHERE tags != '' AND domain != ''")?;

        let mut result: HashMap<String, HashMap<String, u64>> = HashMap::new();

        let rows = stmt.query_map([], |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)))?;

        for row in rows.flatten() {
            let (tags_json, domain) = row;
            if let Ok(tags) = serde_json::from_str::<Vec<String>>(&tags_json) {
                for tag in tags {
                    let domain_counts = result.entry(tag).or_default();
                    *domain_counts.entry(domain.clone()).or_insert(0) += 1;
                }
            }
        }

        Ok(result)
    }

    /// Get exemplar notes for a domain (recent, well-classified notes)
    pub fn domain_exemplars(&self, domain: &str, limit: usize) -> Result<Vec<NoteRow>> {
        self.list_notes(Some(domain), None, None, None, None, Some(limit as u32))
    }

    /// Find notes matching a specific tag, optionally filtered by domain
    pub fn tag_search(&self, tag: &str, domain: Option<&str>, limit: Option<u32>) -> Result<Vec<NoteRow>> {
        let limit = limit.unwrap_or(20);

        // Tags are stored as JSON arrays, use Rust-side filtering
        let mut sql = String::from(
            "SELECT path, title, domain, note_type, origin, status, date, tags, source, creator, body, summary
             FROM notes WHERE tags != ''",
        );
        let mut param_values: Vec<Box<dyn rusqlite::types::ToSql>> = vec![];
        let mut param_idx = 1;

        if let Some(d) = domain {
            sql.push_str(&format!(" AND domain = ?{param_idx}"));
            param_values.push(Box::new(d.to_string()));
            param_idx += 1;
        }
        let _ = param_idx;

        sql.push_str(" ORDER BY date DESC");

        let params_refs: Vec<&dyn rusqlite::types::ToSql> = param_values.iter().map(|p| p.as_ref()).collect();
        let mut stmt = self.conn.prepare(&sql)?;

        let tag_lower = tag.to_lowercase();
        let is_prefix = tag_lower.ends_with('*');
        let prefix = if is_prefix { &tag_lower[..tag_lower.len() - 1] } else { &tag_lower };

        let rows: Vec<NoteRow> = stmt
            .query_map(params_refs.as_slice(), NoteRow::from_row)?
            .filter_map(|r| r.ok())
            .filter(|note| {
                if let Ok(tags) = serde_json::from_str::<Vec<String>>(&note.tags) {
                    tags.iter().any(|t| {
                        let t_lower = t.to_lowercase();
                        if is_prefix { t_lower.starts_with(prefix) } else { t_lower == *prefix }
                    })
                } else {
                    false
                }
            })
            .take(limit as usize)
            .collect();

        Ok(rows)
    }

    /// Get all tags with their counts and domain distribution
    pub fn tag_stats(&self) -> Result<Vec<TagStat>> {
        let mut stmt = self.conn.prepare("SELECT tags, domain FROM notes WHERE tags != ''")?;

        let mut tag_info: HashMap<String, (u64, HashMap<String, u64>)> = HashMap::new();

        let rows = stmt.query_map([], |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)))?;

        for row in rows.flatten() {
            let (tags_json, domain) = row;
            if let Ok(tags) = serde_json::from_str::<Vec<String>>(&tags_json) {
                for tag in tags {
                    let entry = tag_info.entry(tag).or_insert_with(|| (0, HashMap::new()));
                    entry.0 += 1;
                    if !domain.is_empty() {
                        *entry.1.entry(domain.clone()).or_insert(0) += 1;
                    }
                }
            }
        }

        let mut stats: Vec<TagStat> = tag_info
            .into_iter()
            .map(|(tag, (count, domains))| {
                let domain_list: Vec<String> = domains.keys().cloned().collect();
                TagStat {
                    tag,
                    count,
                    domains: domain_list,
                }
            })
            .collect();

        stats.sort_by_key(|b| std::cmp::Reverse(b.count));
        Ok(stats)
    }

    /// Find tags that co-occur with the given tag, sorted by frequency
    pub fn tag_cooccurrence(&self, tag: &str) -> Result<Vec<(String, u64)>> {
        let mut stmt = self.conn.prepare("SELECT tags FROM notes WHERE tags != ''")?;

        let tag_lower = tag.to_lowercase();
        let mut cooccur: HashMap<String, u64> = HashMap::new();

        let rows = stmt.query_map([], |row| row.get::<_, String>(0))?;

        for row in rows.flatten() {
            if let Ok(tags) = serde_json::from_str::<Vec<String>>(&row) {
                let has_target = tags.iter().any(|t| t.to_lowercase() == tag_lower);
                if has_target {
                    for t in &tags {
                        let t_lower = t.to_lowercase();
                        if t_lower != tag_lower {
                            *cooccur.entry(t_lower).or_insert(0) += 1;
                        }
                    }
                }
            }
        }

        let mut result: Vec<(String, u64)> = cooccur.into_iter().collect();
        result.sort_by_key(|b| std::cmp::Reverse(b.1));
        Ok(result)
    }

    /// Get creator statistics (name -> count), sorted by count
    pub fn creator_stats(&self) -> Result<Vec<(String, u64)>> {
        let mut stmt = self.conn.prepare(
            "SELECT creator, COUNT(*) as cnt FROM notes WHERE creator != '' GROUP BY creator ORDER BY cnt DESC",
        )?;
        let rows = stmt
            .query_map([], |row| Ok((row.get::<_, String>(0)?, row.get::<_, u64>(1)?)))?
            .filter_map(|r| r.ok())
            .collect();
        Ok(rows)
    }

    /// Get notes by a specific creator
    pub fn notes_by_creator(&self, creator: &str, domain: Option<&str>, limit: Option<u32>) -> Result<Vec<NoteRow>> {
        let limit = limit.unwrap_or(20);
        let mut sql = String::from(
            "SELECT path, title, domain, note_type, origin, status, date, tags, source, creator, body, summary
             FROM notes WHERE LOWER(creator) LIKE ?1",
        );
        let mut param_values: Vec<Box<dyn rusqlite::types::ToSql>> =
            vec![Box::new(format!("%{}%", creator.to_lowercase()))];
        let mut param_idx = 2;

        if let Some(d) = domain {
            sql.push_str(&format!(" AND domain = ?{param_idx}"));
            param_values.push(Box::new(d.to_string()));
            param_idx += 1;
        }
        let _ = param_idx;

        sql.push_str(&format!(" ORDER BY date DESC LIMIT {limit}"));

        let params_refs: Vec<&dyn rusqlite::types::ToSql> = param_values.iter().map(|p| p.as_ref()).collect();
        let mut stmt = self.conn.prepare(&sql)?;
        let rows = stmt
            .query_map(params_refs.as_slice(), NoteRow::from_row)?
            .filter_map(|r| r.ok())
            .collect();
        Ok(rows)
    }

    /// Get source domain statistics (host -> count), sorted by count
    pub fn source_domain_stats(&self) -> Result<Vec<(String, u64)>> {
        let mut stmt = self.conn.prepare("SELECT source FROM notes WHERE source != ''")?;

        let mut host_counts: HashMap<String, u64> = HashMap::new();
        let rows = stmt.query_map([], |row| row.get::<_, String>(0))?;

        for row in rows.flatten() {
            if let Some(host) = extract_host(&row) {
                *host_counts.entry(host).or_insert(0) += 1;
            }
        }

        let mut result: Vec<(String, u64)> = host_counts.into_iter().collect();
        result.sort_by_key(|b| std::cmp::Reverse(b.1));
        Ok(result)
    }

    /// Get notes from a specific source domain
    pub fn notes_by_source_domain(&self, host: &str, domain: Option<&str>, limit: Option<u32>) -> Result<Vec<NoteRow>> {
        let limit = limit.unwrap_or(20);
        let mut sql = String::from(
            "SELECT path, title, domain, note_type, origin, status, date, tags, source, creator, body, summary
             FROM notes WHERE source LIKE ?1",
        );
        let mut param_values: Vec<Box<dyn rusqlite::types::ToSql>> =
            vec![Box::new(format!("%{}%", host.to_lowercase()))];
        let mut param_idx = 2;

        if let Some(d) = domain {
            sql.push_str(&format!(" AND domain = ?{param_idx}"));
            param_values.push(Box::new(d.to_string()));
            param_idx += 1;
        }
        let _ = param_idx;

        sql.push_str(&format!(" ORDER BY date DESC LIMIT {limit}"));

        let params_refs: Vec<&dyn rusqlite::types::ToSql> = param_values.iter().map(|p| p.as_ref()).collect();
        let mut stmt = self.conn.prepare(&sql)?;
        let rows = stmt
            .query_map(params_refs.as_slice(), NoteRow::from_row)?
            .filter_map(|r| r.ok())
            .collect();
        Ok(rows)
    }

    /// Get notes currently in the inbox
    pub fn inbox_notes(&self, limit: Option<u32>) -> Result<Vec<NoteRow>> {
        let limit = limit.unwrap_or(50);
        let mut stmt = self.conn.prepare(&format!(
            "SELECT path, title, domain, note_type, origin, status, date, tags, source, creator, body, summary
                 FROM notes WHERE path LIKE 'inbox/%' ORDER BY date DESC LIMIT {limit}"
        ))?;
        let rows = stmt.query_map([], NoteRow::from_row)?.filter_map(|r| r.ok()).collect();
        Ok(rows)
    }

    /// Get notes that need review (cortex-needs-review = true)
    pub fn notes_needing_review(&self, limit: Option<u32>) -> Result<Vec<NoteRow>> {
        let limit = limit.unwrap_or(50);
        let mut stmt = self.conn.prepare(&format!(
            "SELECT path, title, domain, note_type, origin, status, date, tags, source, creator, body, summary
                 FROM notes WHERE needs_review = 1 ORDER BY date DESC LIMIT {limit}"
        ))?;
        let rows = stmt.query_map([], NoteRow::from_row)?.filter_map(|r| r.ok()).collect();
        Ok(rows)
    }

    /// Get quality score distribution and notes filtered by quality level
    pub fn quality_distribution(&self) -> Result<Vec<(String, u64)>> {
        let mut stmt = self.conn.prepare(
            "SELECT quality, COUNT(*) as cnt FROM notes WHERE quality != '' GROUP BY quality ORDER BY cnt DESC",
        )?;
        let rows = stmt
            .query_map([], |row| Ok((row.get::<_, String>(0)?, row.get::<_, u64>(1)?)))?
            .filter_map(|r| r.ok())
            .collect();
        Ok(rows)
    }

    /// Get notes at a specific quality level
    pub fn notes_by_quality(&self, quality: &str, limit: Option<u32>) -> Result<Vec<NoteRow>> {
        let limit = limit.unwrap_or(20);
        let mut stmt = self.conn.prepare(&format!(
            "SELECT path, title, domain, note_type, origin, status, date, tags, source, creator, body, summary
                 FROM notes WHERE LOWER(quality) = ?1 ORDER BY date DESC LIMIT {limit}"
        ))?;
        let rows = stmt
            .query_map(params![quality.to_lowercase()], NoteRow::from_row)?
            .filter_map(|r| r.ok())
            .collect();
        Ok(rows)
    }

    /// Cortex-assigned quality level for one note (`low` / `medium` / `high`,
    /// or `""` when unscored). `None` when the note is not in the index.
    ///
    /// This is the only stubness signal queryable from the `notes` table: the
    /// richer `cortex-quality-issues` frontmatter (which carries the
    /// `[stub-body]` marker) is not a column here. Oracle's exclude filter uses
    /// `quality = low` as its stub proxy.
    pub fn note_quality(&self, path: &str) -> Result<Option<String>> {
        let q: Option<String> = self
            .conn
            .query_row("SELECT quality FROM notes WHERE path = ?1", params![path], |row| {
                row.get(0)
            })
            .ok();
        Ok(q)
    }

    /// Get duplicate note groups
    pub fn duplicate_groups(&self) -> Result<Vec<DuplicateGroup>> {
        let mut stmt = self.conn.prepare(
            "SELECT duplicate_group, path, title FROM notes WHERE duplicate_group != '' ORDER BY duplicate_group, path",
        )?;

        let mut groups: HashMap<String, Vec<(String, String)>> = HashMap::new();
        let rows = stmt.query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
            ))
        })?;

        for row in rows.flatten() {
            let (group_id, path, title) = row;
            groups.entry(group_id).or_default().push((path, title));
        }

        let mut result: Vec<DuplicateGroup> = groups
            .into_iter()
            .filter(|(_, notes)| notes.len() > 1)
            .map(|(group_id, notes)| DuplicateGroup {
                group_id,
                note_count: notes.len() as u64,
                notes: notes
                    .into_iter()
                    .map(|(path, title)| DuplicateNote { path, title })
                    .collect(),
            })
            .collect();

        result.sort_by_key(|b| std::cmp::Reverse(b.note_count));
        Ok(result)
    }

    /// Get classification pipeline statistics
    pub fn classify_stats(&self, domain: Option<&str>) -> Result<ClassifyStats> {
        let domain_filter = domain.map(|d| format!(" AND domain = '{d}'")).unwrap_or_default();

        let total_classified: u64 = self.conn.query_row(
            &format!("SELECT COUNT(*) FROM notes WHERE classified = 1{domain_filter}"),
            [],
            |row| row.get(0),
        )?;

        let by_method = {
            let mut stmt = self.conn.prepare(&format!(
                "SELECT classified_by, COUNT(*) FROM notes WHERE classified = 1 AND classified_by != ''{domain_filter} GROUP BY classified_by ORDER BY COUNT(*) DESC"
            ))?;
            stmt.query_map([], |row| Ok((row.get::<_, String>(0)?, row.get::<_, u64>(1)?)))?
                .filter_map(|r| r.ok())
                .collect()
        };

        let by_confidence = {
            let mut stmt = self.conn.prepare(&format!(
                "SELECT confidence, COUNT(*) FROM notes WHERE classified = 1 AND confidence != ''{domain_filter} GROUP BY confidence ORDER BY COUNT(*) DESC"
            ))?;
            stmt.query_map([], |row| Ok((row.get::<_, String>(0)?, row.get::<_, u64>(1)?)))?
                .filter_map(|r| r.ok())
                .collect()
        };

        let by_domain = {
            let mut stmt = self.conn.prepare(
                "SELECT domain, COUNT(*) FROM notes WHERE classified = 1 AND domain != '' GROUP BY domain ORDER BY COUNT(*) DESC",
            )?;
            stmt.query_map([], |row| Ok((row.get::<_, String>(0)?, row.get::<_, u64>(1)?)))?
                .filter_map(|r| r.ok())
                .collect()
        };

        let pending_review: u64 = self.conn.query_row(
            &format!("SELECT COUNT(*) FROM notes WHERE needs_review = 1{domain_filter}"),
            [],
            |row| row.get(0),
        )?;

        let inbox_count: u64 =
            self.conn
                .query_row("SELECT COUNT(*) FROM notes WHERE path LIKE 'inbox/%'", [], |row| {
                    row.get(0)
                })?;

        let unclassified: u64 = self.conn.query_row(
            "SELECT COUNT(*) FROM notes WHERE domain = '' AND note_type NOT IN ('daily', 'system')",
            [],
            |row| row.get(0),
        )?;

        Ok(ClassifyStats {
            total_classified,
            by_method,
            by_confidence,
            by_domain,
            pending_review,
            inbox_count,
            unclassified,
        })
    }
}
