use eyre::{Context, Result};
use serde_json::Value;

use crate::types::IngestRequest;

pub fn build_schema() -> Result<Value> {
    log::debug!("extension::schema::build_schema");
    serde_json::to_value(schemars::schema_for!(IngestRequest)).context("serialize IngestRequest schema")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn schema_round_trips_through_serde_json() {
        let schema = build_schema().expect("build_schema");
        let rendered = serde_json::to_string(&schema).expect("serialize");
        let reparsed: Value = serde_json::from_str(&rendered).expect("reparse");
        assert_eq!(schema, reparsed, "schema lost fidelity through serde_json round-trip");
    }

    #[test]
    fn schema_root_is_a_json_object() {
        let schema = build_schema().expect("build_schema");
        assert!(schema.is_object(), "schema root must be a JSON object, got {schema}");
    }

    #[test]
    fn schema_lists_url_as_required() {
        let schema = build_schema().expect("build_schema");
        let required = schema
            .get("required")
            .and_then(|v| v.as_array())
            .expect("schema.required must exist");
        let names: Vec<&str> = required.iter().filter_map(|v| v.as_str()).collect();
        assert!(
            names.contains(&"url"),
            "schema.required must include 'url' (the field background.js sends); got {names:?}"
        );
    }
}
