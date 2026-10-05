use super::*;

fn text(s: &str) -> Value {
    Value::Text(s.to_string())
}

fn tags(list: &[&str]) -> Vec<String> {
    list.iter().map(|s| s.to_string()).collect()
}

#[test]
fn and_eq_appends_a_bare_placeholder_and_its_param() {
    let f = Filter::new("SELECT 1 WHERE x = ?", vec![text("base")]).and_eq("n.status", Some("done"));
    assert_eq!(f.sql, "SELECT 1 WHERE x = ? AND n.status = ?");
    assert_eq!(f.params, vec![text("base"), text("done")]);
}

#[test]
fn and_eq_none_is_a_no_op() {
    let f = Filter::new("SELECT 1", vec![]).and_eq("status", None);
    assert_eq!(f.sql, "SELECT 1");
    assert!(f.params.is_empty());
}

#[test]
fn and_cmp_uses_the_given_operator() {
    let f = Filter::new("W", vec![])
        .and_cmp("date", ">=", Some("2026-01-01"))
        .and_cmp("date", "<=", Some("2026-02-01"));
    assert_eq!(f.sql, "W AND date >= ? AND date <= ?");
    assert_eq!(f.params, vec![text("2026-01-01"), text("2026-02-01")]);
}

#[test]
fn and_tags_or_mode_is_exists_over_the_facet() {
    let t = tags(&["a", "b"]);
    let f = Filter::new("W", vec![]).and_tags("notes", Some(&t), false);
    assert_eq!(
        f.sql,
        "W AND EXISTS (SELECT 1 FROM note_tags t WHERE t.path = notes.path AND t.tag IN (?, ?))"
    );
    assert_eq!(f.params, vec![text("a"), text("b")]);
}

#[test]
fn and_tags_all_mode_counts_distinct_matches() {
    let t = tags(&["a", "b"]);
    let f = Filter::new("W", vec![]).and_tags("n", Some(&t), true);
    assert_eq!(
        f.sql,
        "W AND (SELECT count(DISTINCT t.tag) FROM note_tags t WHERE t.path = n.path AND t.tag IN (?, ?)) = 2"
    );
    assert_eq!(f.params, vec![text("a"), text("b")]);
}

#[test]
fn and_tags_dedups_so_all_mode_counts_distinct_tags() {
    let t = tags(&["rust", "rust"]);
    let f = Filter::new("W", vec![]).and_tags("n", Some(&t), true);
    assert!(f.sql.ends_with("IN (?)) = 1"), "{}", f.sql);
    assert_eq!(f.params, vec![text("rust")]);
}

#[test]
fn and_tags_none_and_empty_are_no_ops() {
    let empty: Vec<String> = vec![];
    assert_eq!(Filter::new("W", vec![]).and_tags("n", None, true).sql, "W");
    assert_eq!(Filter::new("W", vec![]).and_tags("n", Some(&empty), true).sql, "W");
}

#[test]
fn params_stay_in_placeholder_order_across_mixed_filters() {
    let t = tags(&["x"]);
    let f = Filter::new("B = ?", vec![text("base")])
        .and_tags("n", Some(&t), false)
        .and_eq("n.note_type", Some("article"))
        .then(" LIMIT 5");
    assert_eq!(f.sql.matches('?').count(), f.params.len());
    assert_eq!(f.params, vec![text("base"), text("x"), text("article")]);
}
