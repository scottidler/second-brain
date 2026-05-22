use eyre::{Context, Result};
use serde_json::Value;

use crate::types::IngestRequest;

pub fn build_schema() -> Result<Value> {
    log::debug!("extension::schema::build_schema");
    serde_json::to_value(schemars::schema_for!(IngestRequest)).context("serialize IngestRequest schema")
}
