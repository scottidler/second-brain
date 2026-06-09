use super::*;

impl super::SearchIndex {
    /// Select notes that satisfy every cold-rule floor. A note that scores
    /// anywhere on any axis is excluded:
    ///
    /// - `search_hit_count = 0` AND `last_accessed_at IS NULL`: never read
    ///   via oracle.
    /// - `inbound_link_count = 0`: nothing else in the vault links here.
    /// - `pinned = 0`: not promoted.
    /// - `date < before_date`: content older than the floor. Undated notes
    ///   (`date = ''`) are excluded - age cannot be inferred, and they are
    ///   the lint/quality sweep's responsibility, not cold's.
    ///
    /// Ordered by `date ASC` so the oldest cold notes surface first.
    pub fn cold_notes(&self, q: &ColdQuery) -> Result<Vec<ColdNote>> {
        log::debug!("cold_notes: before_date={} limit={}", q.before_date, q.limit);
        // The cold report governs ingested *knowledge*, not the daily journal.
        // Exclude `type: daily` notes and the `journal/` subtree (both: a daily
        // note misfiled outside journal/ is still excluded by type, and an
        // untyped note inside journal/ is still excluded by path).
        let mut stmt = self.conn.prepare(
            "SELECT path, title, domain, date
             FROM notes
             WHERE search_hit_count = 0
               AND last_accessed_at IS NULL
               AND inbound_link_count = 0
               AND pinned = 0
               AND date != ''
               AND date IS NOT NULL
               AND date < ?1
               AND (note_type IS NULL OR note_type != 'daily')
               AND path NOT LIKE 'journal/%'
             ORDER BY date ASC
             LIMIT ?2",
        )?;
        let rows = stmt
            .query_map(params![q.before_date, q.limit as i64], |row| {
                Ok(ColdNote {
                    path: row.get(0)?,
                    title: row.get::<_, String>(1).unwrap_or_default(),
                    domain: row.get::<_, String>(2).unwrap_or_default(),
                    date: row.get::<_, String>(3).unwrap_or_default(),
                })
            })?
            .filter_map(|r| r.ok())
            .collect();
        Ok(rows)
    }

    /// Count rows that would have qualified for the cold report except
    /// they are pinned. Surfaces visibility into how often the promotion
    /// floor rescues notes from the report. Uses the identical age predicate
    /// as `cold_notes` so the two numbers describe the same population.
    pub fn count_pinned_excluded(&self, before_date: &str) -> Result<u64> {
        let count: i64 = self.conn.query_row(
            "SELECT COUNT(*) FROM notes
             WHERE search_hit_count = 0
               AND last_accessed_at IS NULL
               AND inbound_link_count = 0
               AND pinned = 1
               AND date != ''
               AND date IS NOT NULL
               AND date < ?1
               AND (note_type IS NULL OR note_type != 'daily')
               AND path NOT LIKE 'journal/%'",
            params![before_date],
            |row| row.get(0),
        )?;
        Ok(count as u64)
    }

    /// Total number of rows in the `notes` table; cheap to fetch
    /// alongside the cold report so callers can publish "scanned N"
    /// stats without a second prepare.
    pub fn count_notes(&self) -> Result<u64> {
        let count: i64 = self
            .conn
            .query_row("SELECT COUNT(*) FROM notes", [], |row| row.get(0))?;
        Ok(count as u64)
    }
}
