use super::query::push_tags_filter;
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
            mapped.filter_map(warn_row).collect()
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

        let tag_counts = self.top_tags(20)?;
        let distinct_tags = self.distinct_tag_count()?;
        let type_counts = self.count_by_column("note_type")?;
        let status_counts = self.count_by_column("status")?;

        let schema_gaps = self.compute_schema_gaps()?;

        Ok(VaultStats {
            total_notes: total,
            by_tag: tag_counts,
            distinct_tags,
            by_type: type_counts,
            by_status: status_counts,
            schema_gaps,
        })
    }

    /// Top `limit` tags by note count, over the `note_tags` facet (an index
    /// lookup, not the `tag_stats()` full-table JSON scan - `vault_overview`
    /// only ever needs the top 20, per the design's `by_tag` bullet).
    fn top_tags(&self, limit: u32) -> Result<Vec<(String, u64)>> {
        let mut stmt = self
            .conn
            .prepare("SELECT tag, COUNT(*) as cnt FROM note_tags GROUP BY tag ORDER BY cnt DESC LIMIT ?1")?;
        let rows = stmt
            .query_map(params![limit], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, u64>(1)?))
            })?
            .filter_map(warn_row)
            .collect();
        Ok(rows)
    }

    /// Number of distinct tags across the `note_tags` facet. `by_tag` is
    /// capped at the top 20, so anything reporting "N tags" reads this, not
    /// `by_tag.len()`.
    fn distinct_tag_count(&self) -> Result<u64> {
        let count: u64 = self
            .conn
            .query_row("SELECT COUNT(DISTINCT tag) FROM note_tags", [], |row| row.get(0))?;
        Ok(count)
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
            .filter_map(warn_row)
            .collect();
        Ok(rows)
    }

    fn compute_schema_gaps(&self) -> Result<Vec<(String, u64)>> {
        // `status` dropped (Phase 7, F5): optional per `status-values.md` and
        // `frontmatter.md`, so an empty `status` is not a gap. The other two
        // stay required raw-index counts for oracle's `vault_overview`; `tags`
        // joins them below.
        let fields = ["note_type", "origin"];
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

        // `tags` is a facet, not a scalar TEXT column: index time normalizes
        // `tags = ''` to `'[]'` (P5), so the empty-string check above cannot
        // see it. "No tags" means no `note_tags` row for the path at all.
        let tags_gap: u64 = self.conn.query_row(
            "SELECT COUNT(*) FROM notes WHERE NOT EXISTS (SELECT 1 FROM note_tags WHERE note_tags.path = notes.path)",
            [],
            |row| row.get(0),
        )?;
        if tags_gap > 0 {
            gaps.push(("tags".to_string(), tags_gap));
        }

        Ok(gaps)
    }

    /// Get notes for a specific tag with stats. Membership is the `note_tags`
    /// facet (an index lookup), not a JSON scan.
    pub fn tag_brief(&self, tag: &str, limit: Option<u32>) -> Result<TagBrief> {
        let limit = limit.unwrap_or(10);
        let tags_arg = [tag.to_string()];

        let total: u64 = self.conn.query_row(
            "SELECT COUNT(*) FROM notes WHERE EXISTS (SELECT 1 FROM note_tags t WHERE t.path = notes.path AND t.tag = ?1)",
            params![tag],
            |row| row.get(0),
        )?;

        let unread: u64 = self.conn.query_row(
            &format!(
                "SELECT COUNT(*) FROM notes WHERE status = '{}' AND EXISTS (SELECT 1 FROM note_tags t WHERE t.path = notes.path AND t.tag = ?1)",
                crate::schema::Status::Unread.as_str()
            ),
            params![tag],
            |row| row.get(0),
        )?;

        let starred: u64 = self.conn.query_row(
            &format!(
                "SELECT COUNT(*) FROM notes WHERE status = '{}' AND EXISTS (SELECT 1 FROM note_tags t WHERE t.path = notes.path AND t.tag = ?1)",
                crate::schema::Status::Starred.as_str()
            ),
            params![tag],
            |row| row.get(0),
        )?;

        let type_counts = {
            let mut stmt = self.conn.prepare(
                "SELECT note_type, COUNT(*) FROM notes WHERE note_type != '' AND EXISTS (SELECT 1 FROM note_tags t WHERE t.path = notes.path AND t.tag = ?1) GROUP BY note_type ORDER BY COUNT(*) DESC",
            )?;
            stmt.query_map(params![tag], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, u64>(1)?))
            })?
            .filter_map(warn_row)
            .collect()
        };

        let recent_notes = self.list_notes(Some(&tags_arg), false, None, None, None, None, Some(limit))?;

        Ok(TagBrief {
            tag: tag.to_string(),
            total_notes: total,
            unread,
            starred,
            by_type: type_counts,
            recent: recent_notes,
        })
    }

    /// Find notes matching a specific tag, optionally filtered by a `tags`
    /// sibling filter (OR/AND per `tags_all`). `tag` itself stays a Rust-side
    /// prefix/exact match over the JSON column (the `note_tags` facet has no
    /// prefix index); `tags` is the exact-match facet filter shared with
    /// `search`/`list_notes`/`recent_notes`.
    pub fn tag_search(
        &self,
        tag: &str,
        tags: Option<&[String]>,
        tags_all: bool,
        limit: Option<u32>,
    ) -> Result<Vec<NoteRow>> {
        log::debug!("search::tag_search: tag={tag} limit={limit:?}");
        let limit = limit.unwrap_or(20);

        // Tags are stored as JSON arrays, use Rust-side filtering
        let mut sql = String::from(
            "SELECT path, title, note_type, origin, status, date, tags, source, creator, body, summary, trace, ingested, trace_expires
             FROM notes WHERE tags != ''",
        );
        let mut param_values: Vec<Box<dyn rusqlite::types::ToSql>> = vec![];
        let mut param_idx = 1;

        push_tags_filter(&mut sql, &mut param_values, &mut param_idx, "notes", tags, tags_all);
        let _ = param_idx;

        sql.push_str(" ORDER BY date DESC");

        let params_refs: Vec<&dyn rusqlite::types::ToSql> = param_values.iter().map(|p| p.as_ref()).collect();
        let mut stmt = self.conn.prepare(&sql)?;

        let tag_lower = tag.to_lowercase();
        let is_prefix = tag_lower.ends_with('*');
        let prefix = if is_prefix { &tag_lower[..tag_lower.len() - 1] } else { &tag_lower };

        let rows: Vec<NoteRow> = stmt
            .query_map(params_refs.as_slice(), NoteRow::from_row)?
            .filter_map(warn_row)
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

    /// Get all tags with their note counts
    pub fn tag_stats(&self) -> Result<Vec<TagStat>> {
        let mut stmt = self.conn.prepare("SELECT tags FROM notes WHERE tags != ''")?;

        let mut tag_counts: HashMap<String, u64> = HashMap::new();

        let rows = stmt.query_map([], |row| row.get::<_, String>(0))?;

        for tags_json in rows.flatten() {
            if let Ok(tags) = serde_json::from_str::<Vec<String>>(&tags_json) {
                for tag in tags {
                    *tag_counts.entry(tag).or_insert(0) += 1;
                }
            }
        }

        let mut stats: Vec<TagStat> = tag_counts
            .into_iter()
            .map(|(tag, count)| TagStat { tag, count })
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
            .filter_map(warn_row)
            .collect();
        Ok(rows)
    }

    /// Get notes by a specific creator
    pub fn notes_by_creator(
        &self,
        creator: &str,
        tags: Option<&[String]>,
        tags_all: bool,
        limit: Option<u32>,
    ) -> Result<Vec<NoteRow>> {
        let limit = limit.unwrap_or(20);
        let mut sql = String::from(
            "SELECT path, title, note_type, origin, status, date, tags, source, creator, body, summary, trace, ingested, trace_expires
             FROM notes WHERE LOWER(creator) LIKE ?1",
        );
        let mut param_values: Vec<Box<dyn rusqlite::types::ToSql>> =
            vec![Box::new(format!("%{}%", creator.to_lowercase()))];
        let mut param_idx = 2;

        push_tags_filter(&mut sql, &mut param_values, &mut param_idx, "notes", tags, tags_all);
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

    /// Get source-host statistics (host -> count), sorted by count
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

    /// Get notes from a specific source host
    pub fn notes_by_source_domain(
        &self,
        host: &str,
        tags: Option<&[String]>,
        tags_all: bool,
        limit: Option<u32>,
    ) -> Result<Vec<NoteRow>> {
        let limit = limit.unwrap_or(20);
        let mut sql = String::from(
            "SELECT path, title, note_type, origin, status, date, tags, source, creator, body, summary, trace, ingested, trace_expires
             FROM notes WHERE source LIKE ?1",
        );
        let mut param_values: Vec<Box<dyn rusqlite::types::ToSql>> =
            vec![Box::new(format!("%{}%", host.to_lowercase()))];
        let mut param_idx = 2;

        push_tags_filter(&mut sql, &mut param_values, &mut param_idx, "notes", tags, tags_all);
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

    /// Get notes currently in the inbox
    pub fn inbox_notes(&self, limit: Option<u32>) -> Result<Vec<NoteRow>> {
        let limit = limit.unwrap_or(50);
        let mut stmt = self.conn.prepare(&format!(
            "SELECT path, title, note_type, origin, status, date, tags, source, creator, body, summary, trace, ingested, trace_expires
                 FROM notes WHERE path LIKE 'inbox/%' ORDER BY date DESC LIMIT {limit}"
        ))?;
        let rows = stmt.query_map([], NoteRow::from_row)?.filter_map(warn_row).collect();
        Ok(rows)
    }

    /// Get the oldest (by `modified_at`) note still sitting in the inbox,
    /// excluding dotfiles (`inbox/.claude/...`) which are tooling, not
    /// unclassified content. Used by `sb doctor` to Warn on notes stuck past
    /// the daemon's classify cadence.
    pub fn inbox_oldest(&self) -> Result<Option<(String, i64)>> {
        let mut stmt = self.conn.prepare(
            "SELECT path, modified_at FROM notes WHERE path LIKE 'inbox/%' AND path NOT LIKE 'inbox/.%' ORDER BY modified_at ASC LIMIT 1",
        )?;
        let row = stmt
            .query_map([], |row| Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?)))?
            .next()
            .transpose()?;
        Ok(row)
    }

    /// Get notes that need review (cortex-needs-review = true)
    pub fn notes_needing_review(&self, limit: Option<u32>) -> Result<Vec<NoteRow>> {
        let limit = limit.unwrap_or(50);
        let mut stmt = self.conn.prepare(&format!(
            "SELECT path, title, note_type, origin, status, date, tags, source, creator, body, summary, trace, ingested, trace_expires
                 FROM notes WHERE needs_review = 1 ORDER BY date DESC LIMIT {limit}"
        ))?;
        let rows = stmt.query_map([], NoteRow::from_row)?.filter_map(warn_row).collect();
        Ok(rows)
    }

    /// Get quality score distribution and notes filtered by quality level
    pub fn quality_distribution(&self) -> Result<Vec<(String, u64)>> {
        let mut stmt = self.conn.prepare(
            "SELECT quality, COUNT(*) as cnt FROM notes WHERE quality != '' GROUP BY quality ORDER BY cnt DESC",
        )?;
        let rows = stmt
            .query_map([], |row| Ok((row.get::<_, String>(0)?, row.get::<_, u64>(1)?)))?
            .filter_map(warn_row)
            .collect();
        Ok(rows)
    }

    /// Get notes at a specific quality level
    pub fn notes_by_quality(&self, quality: &str, limit: Option<u32>) -> Result<Vec<NoteRow>> {
        let limit = limit.unwrap_or(20);
        let mut stmt = self.conn.prepare(&format!(
            "SELECT path, title, note_type, origin, status, date, tags, source, creator, body, summary, trace, ingested, trace_expires
                 FROM notes WHERE LOWER(quality) = ?1 ORDER BY date DESC LIMIT {limit}"
        ))?;
        let rows = stmt
            .query_map(params![quality.to_lowercase()], NoteRow::from_row)?
            .filter_map(warn_row)
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

    /// `classify_stats`'s shared filter builder: `base_where` AND an optional
    /// `tags` facet filter (`push_tags_filter`). Returns the finished SQL plus
    /// its bound parameters, ready for `query_row` (a `COUNT(*)`) or
    /// `query_map` (a `GROUP BY`) callers.
    fn classify_filtered_sql(
        &self,
        select: &str,
        base_where: &str,
        tags: Option<&[String]>,
        tags_all: bool,
        group_by: Option<&str>,
    ) -> (String, Vec<Box<dyn rusqlite::types::ToSql>>) {
        let mut sql = format!("SELECT {select} FROM notes WHERE {base_where}");
        let mut param_values: Vec<Box<dyn rusqlite::types::ToSql>> = vec![];
        let mut param_idx = 1;
        push_tags_filter(&mut sql, &mut param_values, &mut param_idx, "notes", tags, tags_all);
        let _ = param_idx;
        if let Some(g) = group_by {
            sql.push_str(&format!(" GROUP BY {g} ORDER BY COUNT(*) DESC"));
        }
        (sql, param_values)
    }

    /// Get classification pipeline statistics. `tags` narrows
    /// `total_classified`, `by_method`, and `by_confidence` -
    /// `pending_review`, `inbox_count`, and `unclassified` stay unfiltered
    /// breakdowns. `unclassified` counts notes carrying no `tags` (the
    /// tags-only "not yet classified" signal), excluding daily/system notes.
    pub fn classify_stats(&self, tags: Option<&[String]>, tags_all: bool) -> Result<ClassifyStats> {
        let (sql, params) = self.classify_filtered_sql("COUNT(*)", "classified = 1", tags, tags_all, None);
        let params_refs: Vec<&dyn rusqlite::types::ToSql> = params.iter().map(|p| p.as_ref()).collect();
        let total_classified: u64 = self.conn.query_row(&sql, params_refs.as_slice(), |row| row.get(0))?;

        let by_method = {
            let (sql, params) = self.classify_filtered_sql(
                "classified_by, COUNT(*)",
                "classified = 1 AND classified_by != ''",
                tags,
                tags_all,
                Some("classified_by"),
            );
            let params_refs: Vec<&dyn rusqlite::types::ToSql> = params.iter().map(|p| p.as_ref()).collect();
            let mut stmt = self.conn.prepare(&sql)?;
            stmt.query_map(params_refs.as_slice(), |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, u64>(1)?))
            })?
            .filter_map(warn_row)
            .collect()
        };

        let by_confidence = {
            let (sql, params) = self.classify_filtered_sql(
                "confidence, COUNT(*)",
                "classified = 1 AND confidence != ''",
                tags,
                tags_all,
                Some("confidence"),
            );
            let params_refs: Vec<&dyn rusqlite::types::ToSql> = params.iter().map(|p| p.as_ref()).collect();
            let mut stmt = self.conn.prepare(&sql)?;
            stmt.query_map(params_refs.as_slice(), |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, u64>(1)?))
            })?
            .filter_map(warn_row)
            .collect()
        };

        let (sql, params) = self.classify_filtered_sql("COUNT(*)", "needs_review = 1", tags, tags_all, None);
        let params_refs: Vec<&dyn rusqlite::types::ToSql> = params.iter().map(|p| p.as_ref()).collect();
        let pending_review: u64 = self.conn.query_row(&sql, params_refs.as_slice(), |row| row.get(0))?;

        let inbox_count: u64 =
            self.conn
                .query_row("SELECT COUNT(*) FROM notes WHERE path LIKE 'inbox/%'", [], |row| {
                    row.get(0)
                })?;

        let unclassified: u64 = self.conn.query_row(
            &format!(
                "SELECT COUNT(*) FROM notes WHERE NOT EXISTS (SELECT 1 FROM note_tags WHERE note_tags.path = notes.path) AND note_type NOT IN ('{}', '{}')",
                crate::schema::NoteType::Daily.as_str(),
                crate::schema::NoteType::System.as_str()
            ),
            [],
            |row| row.get(0),
        )?;

        Ok(ClassifyStats {
            total_classified,
            by_method,
            by_confidence,
            pending_review,
            inbox_count,
            unclassified,
        })
    }
}
