//! Per-vendor credential map for the fabric child process (`llm.api-keys`).
//!
//! Fabric (the third-party Go binary) reads each vendor's credential from a
//! fixed env var name (`ANTHROPIC_API_KEY`, `GEMINI_API_KEY`,
//! `OPENAI_API_KEY`). The workspace stores credentials under persona-scoped
//! names (`ESCOTE_ANTHROPIC_API_KEY`) or files, so config maps each vendor to
//! the var (or file) that holds its key, and [`super::command`] exports every
//! mapped value under fabric's name on the child process only.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

/// A model vendor fabric can call, keyed by the name config uses.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Vendor {
    Anthropic,
    Gemini,
    #[serde(rename = "openai")]
    OpenAi,
}

impl Vendor {
    /// The env var fabric reads this vendor's credential from.
    pub fn fabric_env(self) -> &'static str {
        match self {
            Self::Anthropic => "ANTHROPIC_API_KEY",
            Self::Gemini => "GEMINI_API_KEY",
            Self::OpenAi => "OPENAI_API_KEY",
        }
    }
}

/// `vendor -> NAME of the env var (or a file path) holding its key`.
///
/// Values are references, never key material: they resolve through
/// [`crate::config::resolve_secret`] when the fabric child is built. An
/// unknown vendor name is a deserialize error, not a silently ignored entry.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ApiKeys(BTreeMap<Vendor, String>);

impl ApiKeys {
    /// A map holding only the Anthropic entry; an empty `var` is an empty map
    /// (fabric falls back to its own `.env`). The shape every single-key
    /// `run_pattern` caller passes.
    pub fn anthropic(var: &str) -> Self {
        let mut keys = Self::default();
        if !var.is_empty() {
            keys.0.insert(Vendor::Anthropic, var.to_string());
        }
        keys
    }

    /// The var (or file) mapped for `vendor`, if any.
    pub fn get(&self, vendor: Vendor) -> Option<&str> {
        self.0.get(&vendor).map(String::as_str)
    }

    /// Map `vendor` to `var`, replacing any earlier entry (builder style).
    pub fn with(mut self, vendor: Vendor, var: impl Into<String>) -> Self {
        self.0.insert(vendor, var.into());
        self
    }

    /// Map `vendor` to `var` in place.
    pub fn insert(&mut self, vendor: Vendor, var: impl Into<String>) {
        self.0.insert(vendor, var.into());
    }

    /// Every `(vendor, var)` pair, in vendor order.
    pub fn iter(&self) -> impl Iterator<Item = (Vendor, &str)> {
        self.0.iter().map(|(v, var)| (*v, var.as_str()))
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

#[cfg(test)]
mod tests;
