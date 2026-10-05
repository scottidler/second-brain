use std::process::Command as ProcessCommand;

pub(super) fn get_tool_validation_help() -> String {
    #[allow(clippy::type_complexity)]
    // `notify-send` is a runtime-dependency proxy: borg does not shell out to
    // it (we use the in-process notify-rust crate), but its presence indicates
    // a working libnotify stack and gives the operator a diagnostic one-liner
    // (`notify-send foo`) when desktop toasts go missing.
    let tools: &[(&str, &str, &str, &[(&str, &str, &str)])] = &[
        ("yt-dlp", "--version", "2023.0.0", &[("ffmpeg", "-version", "")]),
        ("fabric", "--version", "1.0.0", &[]),
        ("markitdown", "--version", "", &[]),
        ("notify-send", "--version", "", &[]),
    ];

    struct ToolEntry {
        icon: String,
        name: String,
        version: String,
        prefix: String,
    }
    let mut entries: Vec<ToolEntry> = Vec::new();
    for (tool, version_arg, min_version, deps) in tools {
        let status = check_tool_version(tool, version_arg, min_version);
        entries.push(ToolEntry {
            icon: status.status_icon,
            name: tool.to_string(),
            version: status.version,
            prefix: "  ".to_string(),
        });
        for (i, (dep, dep_ver_arg, dep_min_ver)) in deps.iter().enumerate() {
            let dep_status = check_tool_version(dep, dep_ver_arg, dep_min_ver);
            let connector = if i == deps.len() - 1 { "└──" } else { "├──" };
            entries.push(ToolEntry {
                icon: dep_status.status_icon,
                name: dep.to_string(),
                version: dep_status.version,
                prefix: format!("  {connector} "),
            });
        }
    }

    let max_left_len = entries
        .iter()
        .map(|e| e.prefix.chars().count() + 2 + 1 + e.name.len())
        .max()
        .unwrap_or(0);
    let max_ver_len = entries.iter().map(|e| e.version.len()).max().unwrap_or(0);

    let mut help = String::from("REQUIRED TOOLS:\n");
    for entry in &entries {
        let left_len = entry.prefix.chars().count() + 2 + 1 + entry.name.len();
        let padding = max_left_len - left_len;
        help.push_str(&format!(
            "{}{} {}{}  {:>width$}\n",
            entry.prefix,
            entry.icon,
            entry.name,
            " ".repeat(padding),
            entry.version,
            width = max_ver_len,
        ));
    }

    help.push_str(&format!(
        "\nLogs are written to: {}",
        crate::logger::log_path("borg").display()
    ));
    help
}

struct ToolStatus {
    version: String,
    status_icon: String,
}

fn check_tool_version(tool: &str, version_arg: &str, min_version: &str) -> ToolStatus {
    match ProcessCommand::new(tool).arg(version_arg).output() {
        Ok(output) if output.status.success() => {
            let version_output = String::from_utf8_lossy(&output.stdout);
            let version = extract_version(&version_output);
            let meets_requirement = if min_version.is_empty() {
                true
            } else {
                version_compare(&version, min_version)
            };
            ToolStatus {
                version: if version.is_empty() || version == "unknown" {
                    "installed".to_string()
                } else {
                    version
                },
                status_icon: if meets_requirement { "\u{2705}" } else { "\u{26a0}\u{fe0f}" }.to_string(),
            }
        }
        _ => ToolStatus {
            version: "not found".to_string(),
            status_icon: "\u{274c}".to_string(),
        },
    }
}

fn extract_version(output: &str) -> String {
    if let Some(line) = output.lines().next() {
        for word in line.split_whitespace() {
            let trimmed = word.trim_start_matches('v');
            if trimmed.contains('.') && trimmed.chars().next().is_some_and(|c| c.is_ascii_digit()) {
                return trimmed.to_string();
            }
        }
        let trimmed = line.trim();
        if trimmed.contains('.') && trimmed.chars().next().is_some_and(|c| c.is_ascii_digit()) {
            return trimmed.to_string();
        }
    }
    "unknown".to_string()
}

fn version_compare(version: &str, min_version: &str) -> bool {
    let parse = |v: &str| -> Vec<u32> { v.split('.').map(|part| part.parse().unwrap_or(0)).collect() };
    let v1 = parse(version);
    let v2 = parse(min_version);
    for (a, b) in v1.iter().zip(v2.iter()) {
        if a > b {
            return true;
        }
        if a < b {
            return false;
        }
    }
    v1.len() >= v2.len()
}
