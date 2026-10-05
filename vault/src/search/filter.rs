//! The one dynamic SQL filter builder. Every placeholder is a bare positional
//! `?`, base clause included, so a numbered `?N` can never collide with it and
//! the params vector order is exactly the textual order of the `?`s.

use rusqlite::types::Value;

use super::query::dedup_tags;

/// A SQL fragment under construction plus its bound parameters.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Filter {
    pub sql: String,
    pub params: Vec<Value>,
}

impl Filter {
    /// Start from a base statement whose own `?` placeholders are bound by `params`.
    pub fn new(base_sql: &str, params: Vec<Value>) -> Self {
        Filter {
            sql: base_sql.to_string(),
            params,
        }
    }

    /// ` AND {column} = ?` when `value` is set; a no-op otherwise.
    pub fn and_eq(self, column: &str, value: Option<&str>) -> Self {
        self.and_cmp(column, "=", value)
    }

    /// ` AND {column} {op} ?` when `value` is set; a no-op otherwise. `op` is a
    /// trusted literal (`=`, `>=`, `<=`), never user input.
    pub fn and_cmp(mut self, column: &str, op: &str, value: Option<&str>) -> Self {
        if let Some(v) = value {
            self.sql.push_str(&format!(" AND {column} {op} ?"));
            self.params.push(Value::Text(v.to_string()));
        }
        self
    }

    /// Append a `tags` filter against the `note_tags` facet so it is an index
    /// lookup rather than a JSON scan.
    ///
    /// `all = false` is OR across the list: the note carries at least one of
    /// them. `all = true` is AND: it carries all of them. `None` or an empty
    /// list is no filter, so a caller passing `Some(&[])` does not silently get
    /// zero rows.
    ///
    /// `alias` is the `notes` alias in the caller's query (`n` or `notes`), since
    /// the correlated subquery has to join back to it.
    pub fn and_tags(mut self, alias: &str, tags: Option<&[String]>, all: bool) -> Self {
        let Some(tags) = tags.filter(|t| !t.is_empty()) else {
            return self;
        };
        // Dedup before both the placeholder list and the AND-mode `= N` count:
        // `note_tags` holds one row per (path, tag), so `["rust", "rust"]` with
        // `all` would demand two distinct matches and never return a row.
        let tags = dedup_tags(tags);
        let list = vec!["?"; tags.len()].join(", ");
        if all {
            self.sql.push_str(&format!(
                " AND (SELECT count(DISTINCT t.tag) FROM note_tags t WHERE t.path = {alias}.path AND t.tag IN ({list})) = {}",
                tags.len()
            ));
        } else {
            self.sql.push_str(&format!(
                " AND EXISTS (SELECT 1 FROM note_tags t WHERE t.path = {alias}.path AND t.tag IN ({list}))"
            ));
        }
        self.params.extend(tags.into_iter().map(|t| Value::Text(t.clone())));
        self
    }

    /// Append a trailing clause (`ORDER BY`, `LIMIT`) that binds nothing.
    pub fn then(mut self, tail: &str) -> Self {
        self.sql.push_str(tail);
        self
    }
}

#[cfg(test)]
mod tests;
