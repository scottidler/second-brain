use super::*;

pub(crate) fn detect_text_pattern(text: &str) -> TextPattern {
    let trimmed = text.trim();

    // Check for define: pattern
    if let Some(word) = trimmed
        .strip_prefix("define:")
        .or_else(|| trimmed.strip_prefix("Define:"))
        .map(|w| w.trim().to_string())
        && !word.is_empty()
    {
        return TextPattern::Define { word };
    }

    // Check for clarify: <word> vs <word> pattern
    if let Some(rest) = trimmed
        .strip_prefix("clarify:")
        .or_else(|| trimmed.strip_prefix("Clarify:"))
        .map(|w| w.trim())
        && let Some((a, b)) = rest.split_once(" vs ")
    {
        let word_a = a.trim().to_string();
        let word_b = b.trim().to_string();
        if !word_a.is_empty() && !word_b.is_empty() {
            return TextPattern::Clarify { word_a, word_b };
        }
    }

    // `idea:` prefix forces an Idea note even when the text carries a URL - the
    // explicit escape hatch that replaces the old `<10 chars` prose heuristic
    // (Phase 8 resolved decision). It short-circuits the URL redirect below.
    let is_idea = trimmed
        .strip_prefix("idea:")
        .or_else(|| trimmed.strip_prefix("Idea:"))
        .is_some();

    // Phase 8: prose+URL ALWAYS becomes an annotated URL ingest (the source is
    // fetched). The surrounding prose becomes the capture note. A bare URL
    // yields `note = None`. Additional URLs stay in the note text as plain
    // links; the first URL is the capture target.
    if !is_idea && let Some(url) = router::extract_url_from_text(trimmed) {
        let note = router::extract_capture_note(trimmed, &url);
        return TextPattern::ContainsUrl { url, note };
    }

    TextPattern::General
}

pub(crate) async fn process_text(
    text: &str,
    tags: Vec<String>,
    method: IngestMethod,
    force: bool,
    config: &Config,
    trace_id: &str,
) -> IngestResult {
    let start = Instant::now();
    match process_text_inner(text, tags, method, force, config, trace_id).await {
        Ok(mut result) => {
            let elapsed = start.elapsed();
            log::info!("[{trace_id}] Text pipeline completed in {elapsed:.2?}");
            result.elapsed_secs = Some(elapsed.as_secs_f64());
            result
        }
        Err(e) => {
            let elapsed = start.elapsed();
            log::error!("[{trace_id}] Text pipeline failed in {elapsed:.2?}: {e:?}");
            IngestResult {
                status: IngestStatus::Failed {
                    reason: format!("{:#}", e),
                },
                method: Some(method),
                elapsed_secs: Some(elapsed.as_secs_f64()),
                // Non-URL handler: text content is in hand, so a terminal
                // error is a publish failure, not a fetch.
                failure_stage: Some(vault::receipts::FailureStage::PublishFailed),
                ..Default::default()
            }
        }
    }
}

pub(crate) async fn process_text_inner(
    text: &str,
    tags: Vec<String>,
    method: IngestMethod,
    force: bool,
    config: &Config,
    trace_id: &str,
) -> Result<IngestResult> {
    let pattern = detect_text_pattern(text);
    log::debug!("Text pattern detected: {pattern:?}");

    match pattern {
        TextPattern::ContainsUrl { url, note } => {
            // Redirect to URL pipeline, carrying the capture note (Phase 8).
            return Ok(process_url(&url, note, tags, method, force, config, trace_id).await);
        }
        TextPattern::Define { .. } | TextPattern::Clarify { .. } => {
            return process_vocab(text, &pattern, tags, method, force, config, trace_id).await;
        }
        TextPattern::General => {}
    }

    // Code snippet detection (after pattern matching / URL redirect, before general LLM classification)
    if let Some(language) = looks_like_code(text) {
        return process_code_snippet(text, &language, tags, method, force, config, trace_id).await;
    }

    // General text: classify via LLM, then create a note
    let use_fabric = fabric::is_available(&config.fabric);

    // Generate title from text (first line or LLM-generated)
    let title = generate_text_title(text, use_fabric, config).await;

    // Phase 9c-hotfix cutover: route the general text branch through the
    // Idea distiller so the published note carries `distilled: true` plus the
    // structured `## Summary` / `## Claims` / `## Links` / `## Transcript`
    // body sections. IdeaDistiller is synthesis-only (no Fabric call); the
    // full input lands verbatim in `distilled.transcript`.
    let distilled =
        crate::stages::distill::distill_for_publish_idea(&config.fabric, &config.staging, trace_id, text, Some(&title))
            .await;

    let mut all_tags: Vec<String> = tags.iter().map(|t| hygiene::sanitize_tag(t)).collect();
    all_tags.extend(distilled.tags.iter().map(|t| hygiene::sanitize_tag(t)));

    // Generate tags via Fabric (driven by the distilled summary so we send
    // less text over the wire than the raw input would).
    if use_fabric {
        match fabric::generate_tags(&distilled.summary, &config.fabric).await {
            Ok(fabric_tags) => all_tags.extend(fabric_tags.into_iter().map(|t| hygiene::sanitize_tag(&t))),
            Err(e) => log::warn!("fabric generate_tags failed, continuing without fabric tags: {e}"),
        }
    }
    finalize_tags(&mut all_tags, config).await;

    // Text/idea is a verbatim-preservation kind: the full input is the note's
    // only persistent source, so it keeps its in-note `## Transcript`.
    let rendered_distilled = distillers::render(
        &distilled,
        distillers::RenderOptions {
            include_transcript: true,
        },
    );
    let note = NoteContent {
        title: title.clone(),
        source_url: None,
        asset_path: None,
        tags: all_tags.clone(),
        // Keep `summary` populated for downstream IngestResult callers and
        // ledger entries that surface a one-line preview; the structured
        // body comes from `distilled_body`.
        summary: distilled.summary.clone(),
        description: None,
        capture_note: None,
        content_type: ContentType::Note,
        embed_code: None,
        method: Some(method),
        trace_id: Some(trace_id.to_string()),
        slides: Vec::new(),
        distilled_body: Some(rendered_distilled.body_markdown),
        frontmatter_additions: rendered_distilled.frontmatter_additions,
        origin: None,
        status: None,
    };

    let rendered = markdown::render_note(&note, &config.frontmatter);
    let filename = format!("{}.md", hygiene::note_filename(&title, trace_id));

    let dest_path = config.inbox_dir()?;
    std::fs::create_dir_all(&dest_path).context("Failed to create destination directory")?;

    let note_path = super::atomic::resolve_publish_path(&dest_path.join(&filename), force);
    vault::note::write_atomic(&note_path, rendered.as_bytes()).context("Failed to write note to vault")?;

    log::info!("[{trace_id}] Wrote text note: {}", note_path.display());

    publish_note(
        config,
        &note_path,
        method,
        format!("[text: {}]", vault::text::truncate_with_ellipsis(text, 50)),
        title,
        all_tags,
        trace_id,
        distilled.meta.validation.is_degraded(),
    )
}

pub(crate) async fn process_vocab(
    text: &str,
    pattern: &TextPattern,
    tags: Vec<String>,
    method: IngestMethod,
    force: bool,
    config: &Config,
    trace_id: &str,
) -> Result<IngestResult> {
    let use_fabric = fabric::is_available(&config.fabric);

    let (title, content_type, body) = match pattern {
        TextPattern::Define { word } => {
            // Generate definition via LLM
            let body = if use_fabric {
                let prompt = format!(
                    "Define the word \"{word}\". Determine what language it is. \
                     Provide: 1) The definition 2) Example sentences. \
                     Format as markdown with ## Examples section."
                );
                fabric::run_pattern("summarize", &prompt, &config.fabric)
                    .await
                    .unwrap_or_else(|_| format!("definition:: [define: {word}]"))
            } else {
                format!("definition:: [define: {word}]")
            };

            // Detect language (simple heuristic: ask LLM or check common patterns)
            let language = detect_language(word, use_fabric, config).await;

            (
                word.clone(),
                ContentType::VocabDefine {
                    word: word.clone(),
                    language: language.clone(),
                },
                body,
            )
        }
        TextPattern::Clarify { word_a, word_b } => {
            let title = format!("{word_a} vs {word_b}");
            let body = if use_fabric {
                let prompt = format!(
                    "Compare and clarify the difference between \"{word_a}\" and \"{word_b}\". \
                     Determine what language they are. \
                     Provide: definitions, usage contexts, examples, and common confusions. \
                     Format as markdown."
                );
                fabric::run_pattern("summarize", &prompt, &config.fabric)
                    .await
                    .unwrap_or_else(|_| format!("[clarify: {word_a} vs {word_b}]"))
            } else {
                format!("[clarify: {word_a} vs {word_b}]")
            };

            let language = detect_language(word_a, use_fabric, config).await;

            (
                title,
                ContentType::VocabClarify {
                    word_a: word_a.clone(),
                    word_b: word_b.clone(),
                    language: language.clone(),
                },
                body,
            )
        }
        _ => unreachable!("process_vocab called with non-vocab pattern"),
    };

    // Phase 9c-hotfix cutover: route the vocab body through the distiller
    // dispatcher (which maps Vocabulary to IdeaDistiller). The vocab
    // definition prose is preserved verbatim in `distilled.transcript`.
    let ingest_kind = match &content_type {
        ContentType::VocabDefine { language, .. } | ContentType::VocabClarify { language, .. } => {
            if language == "es" {
                crate::types::IngestKind::VocabularyEs
            } else {
                crate::types::IngestKind::VocabularyEn
            }
        }
        _ => crate::types::IngestKind::VocabularyEn,
    };
    let distilled = crate::stages::distill::distill_for_publish_vocab(
        &config.fabric,
        &config.staging,
        trace_id,
        ingest_kind,
        &body,
        Some(&title),
    )
    .await;

    let mut all_tags: Vec<String> = tags.iter().map(|t| hygiene::sanitize_tag(t)).collect();
    all_tags.extend(distilled.tags.iter().map(|t| hygiene::sanitize_tag(t)));
    let vocab_tag = match &content_type {
        ContentType::VocabDefine { language, .. } | ContentType::VocabClarify { language, .. } => {
            format!("{language}-vocab")
        }
        _ => "vocab".to_string(),
    };
    all_tags.push(hygiene::sanitize_tag(&vocab_tag));
    finalize_tags(&mut all_tags, config).await;

    // Vocabulary is a verbatim-preservation kind: keeps its in-note transcript.
    let rendered_distilled = distillers::render(
        &distilled,
        distillers::RenderOptions {
            include_transcript: true,
        },
    );
    let note = NoteContent {
        title: title.clone(),
        source_url: None,
        asset_path: None,
        tags: all_tags.clone(),
        summary: distilled.summary.clone(),
        description: None,
        capture_note: None,
        content_type,
        embed_code: None,
        method: Some(method),
        trace_id: Some(trace_id.to_string()),
        slides: Vec::new(),
        distilled_body: Some(rendered_distilled.body_markdown),
        frontmatter_additions: rendered_distilled.frontmatter_additions,
        origin: None,
        status: None,
    };

    let rendered = markdown::render_note(&note, &config.frontmatter);
    let filename = format!("{}.md", hygiene::note_filename(&title, trace_id));

    let dest_path = config.inbox_dir()?;
    std::fs::create_dir_all(&dest_path).context("Failed to create destination directory")?;

    let note_path = super::atomic::resolve_publish_path(&dest_path.join(&filename), force);
    vault::note::write_atomic(&note_path, rendered.as_bytes()).context("Failed to write note to vault")?;

    log::info!("[{trace_id}] Wrote vocab note: {}", note_path.display());

    publish_note(
        config,
        &note_path,
        method,
        format!("[{}]", text.trim()),
        title,
        all_tags,
        trace_id,
        distilled.meta.validation.is_degraded(),
    )
}

/// Generate a title from text input.
pub(crate) async fn generate_text_title(text: &str, use_fabric: bool, config: &Config) -> String {
    // Use first line as title if it's short enough
    let first_line = text.lines().next().unwrap_or(text).trim();
    if !first_line.is_empty() && first_line.len() <= 80 {
        return first_line.to_string();
    }

    // Try LLM to generate a title
    if use_fabric
        && let Ok(title) = fabric::run_pattern(
            "summarize",
            &format!("Generate a very short (3-8 word) title for this text:\n\n{text}"),
            &config.fabric,
        )
        .await
    {
        let title = title.lines().next().unwrap_or(&title).trim().to_string();
        if !title.is_empty() && title.len() <= 100 {
            return title;
        }
    }

    // Fallback: truncate first line
    if first_line.chars().count() > 80 {
        vault::text::truncate_with_ellipsis(first_line, 77)
    } else {
        "Quick Note".to_string()
    }
}

/// Detect whether text looks like a code snippet and return the detected language.
///
/// Uses a high threshold to avoid false positives on plain text. Requires at least
/// 3 lines and 2+ code indicators to trigger.
pub(crate) fn looks_like_code(text: &str) -> Option<String> {
    let lines: Vec<&str> = text.lines().collect();

    // Must have at least 3 lines
    if lines.len() < 3 {
        return None;
    }

    // Check for shebang line first - strong signal
    if let Some(first) = lines.first() {
        let first = first.trim();
        if first.starts_with("#!") {
            if first.contains("python") {
                return Some("python".to_string());
            } else if first.contains("bash") || first.contains("/sh") || first.contains("zsh") {
                return Some("bash".to_string());
            } else if first.contains("node") {
                return Some("javascript".to_string());
            } else if first.contains("ruby") {
                return Some("ruby".to_string());
            } else if first.contains("perl") {
                return Some("perl".to_string());
            }
            // Shebang but unknown interpreter - still code
            return Some(String::new());
        }
    }

    // Count code indicators
    let mut indicators = 0u32;

    // Language-specific keyword markers (counted per unique marker type found)
    let rust_markers = ["fn ", "pub fn", "async fn", "impl ", "use ", "let mut ", "mod "];
    let python_markers = ["def ", "import ", "from ", "class ", "elif ", "except "];
    let js_markers = ["function ", "const ", "===", "!==", "=> {", "require(", "export "];
    let go_markers = ["func ", "package ", "import (", "go func", "defer "];
    let c_markers = ["#include", "int main", "void ", "printf(", "malloc("];
    let general_markers = ["return ", "if (", "for (", "while (", "switch ("];

    let mut rust_score = 0u32;
    let mut python_score = 0u32;
    let mut js_score = 0u32;
    let mut go_score = 0u32;
    let mut c_score = 0u32;

    for marker in &rust_markers {
        if text.contains(marker) {
            rust_score += 1;
            indicators += 1;
        }
    }
    for marker in &python_markers {
        if text.contains(marker) {
            python_score += 1;
            indicators += 1;
        }
    }
    for marker in &js_markers {
        if text.contains(marker) {
            js_score += 1;
            indicators += 1;
        }
    }
    for marker in &go_markers {
        if text.contains(marker) {
            go_score += 1;
            indicators += 1;
        }
    }
    for marker in &c_markers {
        if text.contains(marker) {
            c_score += 1;
            indicators += 1;
        }
    }
    for marker in &general_markers {
        if text.contains(marker) {
            indicators += 1;
        }
    }

    // Structural indicators
    // Lines with consistent indentation (2+ spaces or tabs)
    let indented_lines = lines
        .iter()
        .filter(|l| !l.is_empty() && (l.starts_with("  ") || l.starts_with('\t')))
        .count();
    if indented_lines >= 2 {
        indicators += 1;
    }

    // Bracket/brace patterns typical of code
    let has_braces = text.contains('{') && text.contains('}');
    let has_arrow = text.contains("->") || text.contains("=>");
    let has_scope_op = text.contains("::");
    let has_logical_ops = text.contains("||") || text.contains("&&");
    let has_semicolons = text.matches(';').count() >= 2;

    if has_braces {
        indicators += 1;
    }
    if has_arrow {
        indicators += 1;
    }
    if has_scope_op {
        indicators += 1;
    }
    if has_logical_ops {
        indicators += 1;
    }
    if has_semicolons {
        indicators += 1;
    }

    // Count structural indicators separately
    let structural_count = has_braces as u32
        + has_arrow as u32
        + has_scope_op as u32
        + has_logical_ops as u32
        + has_semicolons as u32
        + (indented_lines >= 2) as u32;

    // Require at least 2 code indicators AND at least 1 structural indicator.
    // This prevents plain English with words like "import", "class", "function" from triggering.
    if indicators < 2 || structural_count == 0 {
        return None;
    }

    // Determine language by highest score
    let max_score = rust_score.max(python_score).max(js_score).max(go_score).max(c_score);
    if max_score == 0 {
        // Indicators came from structural patterns only - not confident enough
        // unless there are many structural indicators (4+)
        if indicators >= 4 {
            return Some(String::new());
        }
        return None;
    }

    let language = if rust_score == max_score && rust_score >= 2 {
        "rust"
    } else if python_score == max_score && python_score >= 2 {
        "python"
    } else if js_score == max_score && js_score >= 2 {
        "javascript"
    } else if go_score == max_score && go_score >= 2 {
        "go"
    } else if c_score == max_score && c_score >= 2 {
        "c"
    } else {
        // Some language indicators but not enough to be confident about which
        ""
    };

    Some(language.to_string())
}

/// Generate a title for a code snippet.
///
/// Tries to extract a meaningful name from:
/// 1. First comment line
/// 2. First function/class definition
/// 3. LLM-generated title
/// 4. Fallback: "Code Snippet"
pub(crate) async fn generate_code_title(text: &str, language: &str, use_fabric: bool, config: &Config) -> String {
    // Try first comment line
    for line in text.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        // Skip shebang
        if trimmed.starts_with("#!") {
            continue;
        }
        // Single-line comments
        let comment = if trimmed.starts_with("//") {
            Some(trimmed.trim_start_matches('/').trim())
        } else if trimmed.starts_with('#') && !trimmed.starts_with("#!") && !trimmed.starts_with("#include") {
            Some(trimmed.trim_start_matches('#').trim())
        } else if trimmed.starts_with("/*") || trimmed.starts_with("/**") {
            Some(
                trimmed
                    .trim_start_matches('/')
                    .trim_start_matches('*')
                    .trim_end_matches('*')
                    .trim_end_matches('/')
                    .trim(),
            )
        } else {
            None
        };
        if let Some(c) = comment
            && !c.is_empty()
            && c.len() <= 80
        {
            return c.to_string();
        }
        break;
    }

    // Try first function/class name
    for line in text.lines() {
        let trimmed = line.trim();
        // Rust: fn name, pub fn name
        if let Some(rest) = trimmed.strip_prefix("fn ").or_else(|| {
            trimmed
                .strip_prefix("pub fn ")
                .or_else(|| trimmed.strip_prefix("async fn "))
                .or_else(|| trimmed.strip_prefix("pub async fn "))
        }) && let Some(name) = rest.split('(').next()
        {
            let name = name.trim();
            if !name.is_empty() {
                return format!("{language} - {name}").trim_start_matches(" - ").to_string();
            }
        }
        // Python: def name
        if let Some(rest) = trimmed.strip_prefix("def ")
            && let Some(name) = rest.split('(').next()
        {
            let name = name.trim();
            if !name.is_empty() {
                return format!("{language} - {name}").trim_start_matches(" - ").to_string();
            }
        }
        // Go/JS: func name / function name
        if let Some(rest) = trimmed
            .strip_prefix("func ")
            .or_else(|| trimmed.strip_prefix("function "))
            && let Some(name) = rest.split('(').next()
        {
            let name = name.trim();
            if !name.is_empty() {
                return format!("{language} - {name}").trim_start_matches(" - ").to_string();
            }
        }
        // class
        if let Some(rest) = trimmed.strip_prefix("class ")
            && let Some(name) = rest.split(['(', ':', '{', ' ']).next()
        {
            let name = name.trim();
            if !name.is_empty() {
                return format!("{language} - {name}").trim_start_matches(" - ").to_string();
            }
        }
    }

    // Try LLM
    if use_fabric
        && let Ok(title) = fabric::run_pattern(
            "summarize",
            &format!("Generate a very short (3-8 word) title for this code snippet:\n\n{text}"),
            &config.fabric,
        )
        .await
    {
        let title = title.lines().next().unwrap_or(&title).trim().to_string();
        if !title.is_empty() && title.len() <= 100 {
            return title;
        }
    }

    "Code Snippet".to_string()
}

/// Process a code snippet: create a note with fenced code block.
pub(crate) async fn process_code_snippet(
    text: &str,
    language: &str,
    tags: Vec<String>,
    method: IngestMethod,
    force: bool,
    config: &Config,
    trace_id: &str,
) -> Result<IngestResult> {
    let use_fabric = fabric::is_available(&config.fabric);

    let title = generate_code_title(text, language, use_fabric, config).await;

    let mut all_tags: Vec<String> = tags.iter().map(|t| hygiene::sanitize_tag(t)).collect();
    all_tags.push("code-snippet".to_string());
    if !language.is_empty() {
        all_tags.push(hygiene::sanitize_tag(language));
    }

    // Generate additional tags via Fabric
    if use_fabric {
        match fabric::generate_tags(text, &config.fabric).await {
            Ok(fabric_tags) => all_tags.extend(fabric_tags.into_iter().map(|t| hygiene::sanitize_tag(&t))),
            Err(e) => log::warn!("fabric generate_tags failed, continuing without fabric tags: {e}"),
        }
    }
    finalize_tags(&mut all_tags, config).await;

    // Build fenced code block as the summary
    let summary = format!("```{language}\n{text}\n```");

    let note = NoteContent {
        title: title.clone(),
        source_url: None,
        asset_path: None,
        tags: all_tags.clone(),
        summary,
        description: None,
        content_type: ContentType::Code {
            language: language.to_string(),
        },
        embed_code: None,
        method: Some(method),
        trace_id: Some(trace_id.to_string()),
        slides: Vec::new(),
        ..NoteContent::default()
    };

    let rendered = markdown::render_note(&note, &config.frontmatter);
    let filename = format!("{}.md", hygiene::note_filename(&title, trace_id));

    let dest_path = config.inbox_dir()?;
    std::fs::create_dir_all(&dest_path).context("Failed to create destination directory")?;

    let note_path = super::atomic::resolve_publish_path(&dest_path.join(&filename), force);
    vault::note::write_atomic(&note_path, rendered.as_bytes()).context("Failed to write code note to vault")?;

    log::info!(
        "[{trace_id}] Wrote code snippet note: {} (language: {})",
        note_path.display(),
        language
    );

    publish_note(
        config,
        &note_path,
        method,
        format!("[code: {}]", if language.is_empty() { "unknown" } else { language }),
        title,
        all_tags,
        trace_id,
        false,
    )
}

/// Detect language of a word (simple heuristic, can be enhanced with LLM).
pub(crate) async fn detect_language(word: &str, use_fabric: bool, config: &Config) -> String {
    if use_fabric
        && let Ok(result) = fabric::run_pattern(
            "summarize",
            &format!(
                "What language is the word \"{word}\"? Reply with just the language name \
                 in lowercase (e.g., \"english\", \"spanish\", \"french\"). Nothing else."
            ),
            &config.fabric,
        )
        .await
    {
        let lang = result.trim().to_lowercase();
        // Accept reasonable language names
        if !lang.is_empty() && lang.len() < 20 && !lang.contains(' ') {
            return lang;
        }
    }

    // Fallback: assume English
    "english".to_string()
}
