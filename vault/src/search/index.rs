use super::*;

impl super::SearchIndex {
    /// Index the vault, only updating notes whose mtime has changed.
    /// Parses frontmatter fields through vault enums for normalization.
    pub fn index_vault(&self, vault_root: &Path) -> Result<IndexStats> {
        self.index_vault_force(vault_root, false)
    }

    /// Index the vault. When `force` is true the mtime gate is bypassed and
    /// every note is re-`index_one`'d unconditionally. This is the deploy-time
    /// repopulation path for additive columns (e.g. the `trace` block): adding
    /// a column with a default does NOT backfill existing rows, and the normal
    /// mtime gate would skip every unchanged note, leaving them at the column
    /// default until each note's mtime happens to change. A forced pass fixes
    /// the whole back-catalogue in one run.
    pub fn index_vault_force(&self, vault_root: &Path, force: bool) -> Result<IndexStats> {
        log::debug!("search::index_vault: vault_root={} force={force}", vault_root.display());
        let scan_config = ScanConfig::default();
        let notes = scan_vault(vault_root, &scan_config)?;

        // Wrap the whole pass (all upserts + stale removal) in ONE transaction.
        // Previously each `index_one` autocommitted (~2.3k commits per reindex)
        // and a concurrent reader could observe a half-built index. On any error
        // the work rolls back atomically. (`scan_vault` above is pure I/O and
        // stays outside the write transaction.)
        self.conn.execute_batch("BEGIN IMMEDIATE")?;
        let result = (|| -> Result<IndexStats> {
            let mut inserted = 0u64;
            let mut updated = 0u64;
            let mut unchanged = 0u64;

            for note in &notes {
                let abs_path = vault_root.join(&note.path);
                let mtime = std::fs::metadata(&abs_path)
                    .and_then(|m| m.modified())
                    .map(|t| t.duration_since(std::time::UNIX_EPOCH).unwrap_or_default().as_secs())
                    .unwrap_or(0) as i64;

                let path_str = note.path.to_string_lossy();

                let existing_mtime: Option<i64> = optional_row(self.conn.query_row(
                    "SELECT modified_at FROM notes WHERE path = ?1",
                    params![path_str.as_ref()],
                    |row| row.get(0),
                ))?;

                if !force && existing_mtime == Some(mtime) {
                    unchanged += 1;
                    continue;
                }

                match self.index_one(note, mtime)? {
                    IndexAction::Inserted => inserted += 1,
                    IndexAction::Updated => updated += 1,
                }
            }

            let all_paths: Vec<String> = notes.iter().map(|n| n.path.to_string_lossy().to_string()).collect();
            let removed = self.remove_stale_notes(&all_paths)?;

            Ok(IndexStats {
                total_scanned: notes.len() as u64,
                inserted,
                updated,
                unchanged,
                removed,
            })
        })();

        match result {
            Ok(stats) => {
                self.conn.execute_batch("COMMIT")?;
                Ok(stats)
            }
            Err(e) => {
                let _ = self.conn.execute_batch("ROLLBACK");
                Err(e)
            }
        }
    }

    /// Index a single note from its vault file. Single SQLite writer for the
    /// `notes` table: VaultWatcher mtime updates and full-walk reindex both
    /// flow through here. Existing rows are UPDATEd in place (vault-derived
    /// columns only; signal columns stay untouched); new rows are INSERTed
    /// with signal columns zeroed.
    pub fn index_one(&self, note: &Note, mtime: i64) -> Result<IndexAction> {
        let fm = &note.frontmatter;
        let path_str = note.path.to_string_lossy();
        log::debug!(
            "search::index_one: path={} mtime={} title={:?}",
            path_str,
            mtime,
            fm.title
        );

        let summary = parse_body_summary(&note.body).unwrap_or_else(|| detail::extract_summary(&note.body));
        let claims_flat = parse_body_claims(&note.body)
            .into_iter()
            .map(|c| c.text)
            .collect::<Vec<_>>()
            .join("\n");

        // Phase 9: the operator's capture annotation (rendered as
        // `capture-note:` frontmatter by Phase 8). Not a known Frontmatter
        // field, so it lands in `extra`; persisted here so the summary embed
        // path can splice it into the embed text (title + capture-note +
        // summary) without any file I/O.
        let capture_note = extract_cortex_string(&fm.extra, "capture-note");

        let tags_json = fm
            .tags
            .as_ref()
            .map(|t| serde_json::to_string(t).unwrap_or_default())
            .unwrap_or_default();

        let domain = normalize_enum::<Domain>(fm.domain.as_deref(), "domain", &path_str);
        let note_type = normalize_enum::<NoteType>(fm.note_type.as_deref(), "note_type", &path_str);
        let origin = normalize_enum::<Origin>(fm.origin.as_deref(), "origin", &path_str);
        let status = normalize_enum::<Status>(fm.status.as_deref(), "status", &path_str);

        let quality = extract_cortex_string(&fm.extra, "cortex-quality");
        let classified = extract_cortex_bool(&fm.extra, "cortex-classified");
        let classified_by = extract_cortex_string(&fm.extra, "cortex-classified-by");
        let confidence = extract_cortex_string(&fm.extra, "cortex-confidence");
        let needs_review = extract_cortex_bool(&fm.extra, "cortex-needs-review");
        let duplicate_group = extract_cortex_string(&fm.extra, "cortex-duplicate-group");

        let repo_stars = extract_cortex_optional_i64(&fm.extra, "cortex-repo-stars");
        let repo_last_commit = extract_cortex_optional_string(&fm.extra, "cortex-repo-last-commit");
        let repo_primary_language = extract_cortex_optional_string(&fm.extra, "cortex-repo-primary-language");
        let video_duration_seconds = extract_cortex_optional_i64(&fm.extra, "cortex-video-duration-seconds");
        let video_channel = extract_cortex_optional_string(&fm.extra, "cortex-video-channel");
        let video_published_at = extract_cortex_optional_string(&fm.extra, "cortex-video-published-at");
        let thread_platform = extract_cortex_optional_string(&fm.extra, "cortex-thread-platform");
        let thread_post_count = extract_cortex_optional_i64(&fm.extra, "cortex-thread-post-count");
        let thread_author = extract_cortex_optional_string(&fm.extra, "cortex-thread-author");

        let exists: bool = self
            .conn
            .query_row(
                "SELECT 1 FROM notes WHERE path = ?1",
                params![path_str.as_ref()],
                |_| Ok::<_, rusqlite::Error>(()),
            )
            .is_ok();

        let title = fm.title.as_deref().unwrap_or("");
        // Normalize to canonical YYYY-MM-DD (or `` when unparseable) so the
        // `date` column is structurally trustworthy for lexical comparison;
        // the cold sweep's date floor rests on this.
        let date = normalize_date(fm.date.as_deref().unwrap_or(""));
        let source = fm.source.as_deref().unwrap_or("");
        let creator = fm.creator.as_deref().unwrap_or("");
        // Promoted borg join keys. Vault-derived (the user/borg writes them in
        // frontmatter), so they ride the same UPDATE/INSERT path as the rest.
        let trace = fm.trace.as_deref().unwrap_or("");
        let ingested = fm.ingested.as_deref().unwrap_or("");
        let trace_expires = fm.trace_expires.as_deref().unwrap_or("");

        // `pinned` is vault-derived: the user edits `pinned: true` in their
        // note's frontmatter. None or false -> 0; true -> 1. The flip-test
        // in `index_one_pinned_clears_when_frontmatter_drops_field` locks
        // the UPDATE path's responsibility for clearing the flag.
        let pinned_value: i64 = fm.pinned.unwrap_or(false) as i64;

        if exists {
            // UPDATE only vault-derived columns. Signal columns
            // (search_hit_count, last_accessed_at, inbound_link_count) are
            // intentionally excluded so reindex never clobbers Doc 3 state.
            // `pinned` IS vault-derived so it IS updated.
            self.conn.execute(
                "UPDATE notes SET
                    title = ?2, domain = ?3, note_type = ?4, origin = ?5, status = ?6,
                    date = ?7, tags = ?8, source = ?9, creator = ?10, body = ?11,
                    summary = ?12, claims = ?13, modified_at = ?14,
                    quality = ?15, classified = ?16, classified_by = ?17,
                    confidence = ?18, needs_review = ?19, duplicate_group = ?20,
                    cortex_repo_stars = ?21, cortex_repo_last_commit = ?22,
                    cortex_repo_primary_language = ?23,
                    cortex_video_duration_seconds = ?24, cortex_video_channel = ?25,
                    cortex_video_published_at = ?26,
                    cortex_thread_platform = ?27, cortex_thread_post_count = ?28,
                    cortex_thread_author = ?29,
                    pinned = ?30,
                    trace = ?31, ingested = ?32, trace_expires = ?33,
                    capture_note = ?34
                 WHERE path = ?1",
                params![
                    path_str.as_ref(),
                    title,
                    domain,
                    note_type,
                    origin,
                    status,
                    date,
                    tags_json,
                    source,
                    creator,
                    &note.body,
                    summary,
                    claims_flat,
                    mtime,
                    quality,
                    classified,
                    classified_by,
                    confidence,
                    needs_review,
                    duplicate_group,
                    repo_stars,
                    repo_last_commit,
                    repo_primary_language,
                    video_duration_seconds,
                    video_channel,
                    video_published_at,
                    thread_platform,
                    thread_post_count,
                    thread_author,
                    pinned_value,
                    trace,
                    ingested,
                    trace_expires,
                    capture_note,
                ],
            )?;
            Ok(IndexAction::Updated)
        } else {
            self.conn.execute(
                "INSERT INTO notes (
                    path, title, domain, note_type, origin, status, date, tags,
                    source, creator, body, summary, claims, modified_at,
                    quality, classified, classified_by, confidence, needs_review,
                    duplicate_group,
                    cortex_repo_stars, cortex_repo_last_commit,
                    cortex_repo_primary_language,
                    cortex_video_duration_seconds, cortex_video_channel,
                    cortex_video_published_at,
                    cortex_thread_platform, cortex_thread_post_count,
                    cortex_thread_author,
                    search_hit_count, last_accessed_at, inbound_link_count,
                    pinned,
                    trace, ingested, trace_expires,
                    capture_note
                ) VALUES (
                    ?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14,
                    ?15, ?16, ?17, ?18, ?19, ?20,
                    ?21, ?22, ?23, ?24, ?25, ?26, ?27, ?28, ?29,
                    0, NULL, 0,
                    ?30,
                    ?31, ?32, ?33,
                    ?34
                )",
                params![
                    path_str.as_ref(),
                    title,
                    domain,
                    note_type,
                    origin,
                    status,
                    date,
                    tags_json,
                    source,
                    creator,
                    &note.body,
                    summary,
                    claims_flat,
                    mtime,
                    quality,
                    classified,
                    classified_by,
                    confidence,
                    needs_review,
                    duplicate_group,
                    repo_stars,
                    repo_last_commit,
                    repo_primary_language,
                    video_duration_seconds,
                    video_channel,
                    video_published_at,
                    thread_platform,
                    thread_post_count,
                    thread_author,
                    pinned_value,
                    trace,
                    ingested,
                    trace_expires,
                    capture_note,
                ],
            )?;
            Ok(IndexAction::Inserted)
        }
    }

    /// Incrementally reindex only the given (absolute) paths - the watcher's
    /// change set - instead of walking the whole vault. Each existing file is
    /// parsed and `index_one`'d (mtime-gated, same as the full walk); a path
    /// whose file no longer exists has its `notes` row deleted (mirroring
    /// `remove_stale_notes`, which only touches the `notes` table). A parse
    /// failure on one path is logged and skipped so it can't abort the batch.
    pub fn index_changed(&self, vault_root: &Path, changed_paths: &[PathBuf]) -> Result<IndexStats> {
        let mut inserted = 0u64;
        let mut updated = 0u64;
        let mut unchanged = 0u64;
        let mut removed = 0u64;
        let mut scanned = 0u64;

        for abs_path in changed_paths {
            if !abs_path.exists() {
                let relative = abs_path.strip_prefix(vault_root).unwrap_or(abs_path);
                let path_str = relative.to_string_lossy();
                removed += self
                    .conn
                    .execute("DELETE FROM notes WHERE path = ?1", params![path_str.as_ref()])?
                    as u64;
                continue;
            }

            let note = match crate::note::parse_note(vault_root, abs_path) {
                Ok(n) => n,
                Err(e) => {
                    log::warn!("index_changed: failed to parse {}: {e}", abs_path.display());
                    continue;
                }
            };
            scanned += 1;

            let mtime = std::fs::metadata(abs_path)
                .and_then(|m| m.modified())
                .map(|t| t.duration_since(std::time::UNIX_EPOCH).unwrap_or_default().as_secs())
                .unwrap_or(0) as i64;

            let path_str = note.path.to_string_lossy();
            let existing_mtime: Option<i64> = optional_row(self.conn.query_row(
                "SELECT modified_at FROM notes WHERE path = ?1",
                params![path_str.as_ref()],
                |row| row.get(0),
            ))?;
            if existing_mtime == Some(mtime) {
                unchanged += 1;
                continue;
            }

            match self.index_one(&note, mtime)? {
                IndexAction::Inserted => inserted += 1,
                IndexAction::Updated => updated += 1,
            }
        }

        Ok(IndexStats {
            total_scanned: scanned,
            inserted,
            updated,
            unchanged,
            removed,
        })
    }

    pub(crate) fn remove_stale_notes(&self, current_paths: &[String]) -> Result<u64> {
        let mut stmt = self.conn.prepare("SELECT path FROM notes")?;
        let db_paths: Vec<String> = stmt.query_map([], |row| row.get(0))?.filter_map(warn_row).collect();

        // O(1) membership instead of a linear `current_paths.contains` per DB row
        // (was ~5M string compares on a full-vault reindex).
        let current: std::collections::HashSet<&str> = current_paths.iter().map(String::as_str).collect();
        let mut removed = 0u64;
        for path in &db_paths {
            if !current.contains(path.as_str()) {
                self.conn.execute("DELETE FROM notes WHERE path = ?1", params![path])?;
                removed += 1;
            }
        }
        Ok(removed)
    }
}
