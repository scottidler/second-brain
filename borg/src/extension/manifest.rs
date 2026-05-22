use serde_json::{Value, json};

use crate::config::Config;

const DEFAULT_ORIGIN_PATTERNS: &[&str] = &["localhost", "*.lan", "*.local"];

pub fn origin_patterns(config: &Config) -> Vec<String> {
    if let Some(explicit) = &config.extension.origin_patterns
        && !explicit.is_empty()
    {
        return explicit.clone();
    }
    let mut patterns: Vec<String> = DEFAULT_ORIGIN_PATTERNS.iter().map(|s| s.to_string()).collect();
    let host = config.server.host.trim();
    if !host.is_empty()
        && host != "0.0.0.0"
        && host != "127.0.0.1"
        && !patterns.iter().any(|p| p == host)
        && !covered_by_wildcard(&patterns, host)
    {
        patterns.push(host.to_string());
    }
    patterns
}

fn covered_by_wildcard(patterns: &[String], host: &str) -> bool {
    patterns.iter().any(|p| {
        if let Some(suffix) = p.strip_prefix("*.") {
            host == suffix || host.ends_with(&format!(".{suffix}"))
        } else {
            false
        }
    })
}

pub fn host_permissions(patterns: &[String]) -> Vec<String> {
    patterns.iter().map(|p| format!("http://{p}/*")).collect()
}

pub fn csp_extension_pages(patterns: &[String]) -> String {
    let connect = patterns
        .iter()
        .map(|p| format!("http://{p}:*"))
        .collect::<Vec<_>>()
        .join(" ");
    format!("default-src 'self'; connect-src {connect}")
}

pub fn build_manifest(version: &str, config: &Config) -> Value {
    log::debug!("extension::manifest::build_manifest: version={version}");
    let patterns = origin_patterns(config);
    let host_perms = host_permissions(&patterns);
    let csp = csp_extension_pages(&patterns);

    json!({
        "manifest_version": 3,
        "name": "obsidian-borg Capture",
        "description": "Send the current tab URL to obsidian-borg for ingestion",
        "version": version,
        "icons": {
            "16": "icons/locutus-16.png",
            "48": "icons/locutus-48.png",
            "128": "icons/locutus-128.png"
        },
        "action": {
            "default_icon": {
                "16": "icons/locutus-16.png",
                "48": "icons/locutus-48.png",
                "128": "icons/locutus-128.png"
            }
        },
        "background": {
            "scripts": ["background.js"],
            "service_worker": "background.js"
        },
        "permissions": ["activeTab", "storage", "notifications"],
        "host_permissions": host_perms,
        "content_security_policy": { "extension_pages": csp },
        "options_ui": { "page": "options.html", "open_in_tab": false },
        "commands": {
            "capture-url": {
                "description": "Capture current tab URL",
                "suggested_key": { "default": "Alt+Shift+B" }
            }
        },
        "browser_specific_settings": {
            "gecko": {
                "id": "obsidian-borg@scottidler",
                "strict_min_version": "140.0",
                "data_collection_permissions": {
                    "required": ["none"],
                    "optional": []
                }
            }
        }
    })
}

#[cfg(test)]
mod tests;
