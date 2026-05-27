//! Block-level merge: parse an existing note into auto/operator
//! segments, swap auto body content for the freshly-rendered template's
//! matching ids, and re-emit. Operator blocks are preserved byte-for-byte.

use std::collections::HashMap;

const BEGIN_PREFIX: &str = "<!-- facet:auto:begin ";
const END_PREFIX: &str = "<!-- facet:auto:end ";
const CLOSE: &str = " -->";

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Block {
    /// One auto-managed segment. `body` is the content BETWEEN the
    /// begin and end fenceposts (without the fenceposts themselves).
    Auto { id: String, body: String },
    /// Operator-owned content (preserved verbatim).
    Operator(String),
}

/// Parse a note body into a sequence of blocks. Unclosed/mismatched
/// fenceposts degrade to operator blocks; the caller logs a warning if
/// a managed id can no longer be located on the next render.
pub fn parse(input: &str) -> Vec<Block> {
    let mut out = Vec::new();
    let mut cursor = 0;
    let bytes = input.as_bytes();
    while cursor < bytes.len() {
        if let Some(begin_rel) = input[cursor..].find(BEGIN_PREFIX) {
            let begin_abs = cursor + begin_rel;
            if begin_abs > cursor {
                let chunk = &input[cursor..begin_abs];
                if !chunk.is_empty() {
                    out.push(Block::Operator(chunk.to_string()));
                }
            }
            // Find end of the begin marker line.
            let after_prefix = begin_abs + BEGIN_PREFIX.len();
            let Some(close_rel) = input[after_prefix..].find(CLOSE) else {
                let chunk = &input[begin_abs..];
                out.push(Block::Operator(chunk.to_string()));
                return out;
            };
            let id = input[after_prefix..after_prefix + close_rel].trim().to_string();
            let begin_end_line = after_prefix + close_rel + CLOSE.len();
            // Skip trailing newline after the begin fencepost.
            let body_start = if input.as_bytes().get(begin_end_line) == Some(&b'\n') {
                begin_end_line + 1
            } else {
                begin_end_line
            };
            let needle = format!("{END_PREFIX}{id}{CLOSE}");
            let Some(end_rel) = input[body_start..].find(&needle) else {
                // Unmatched begin - treat the remainder as operator content.
                let chunk = &input[begin_abs..];
                out.push(Block::Operator(chunk.to_string()));
                return out;
            };
            let end_abs = body_start + end_rel;
            let body = input[body_start..end_abs].to_string();
            out.push(Block::Auto { id, body });
            let mut after_end = end_abs + needle.len();
            if input.as_bytes().get(after_end) == Some(&b'\n') {
                after_end += 1;
            }
            cursor = after_end;
        } else {
            let chunk = &input[cursor..];
            if !chunk.is_empty() {
                out.push(Block::Operator(chunk.to_string()));
            }
            break;
        }
    }
    out
}

/// Re-emit a block sequence back to a flat string. Round-trips
/// byte-for-byte with `parse` for well-formed inputs.
pub fn emit(blocks: &[Block]) -> String {
    let mut out = String::new();
    for b in blocks {
        match b {
            Block::Operator(s) => out.push_str(s),
            Block::Auto { id, body } => {
                out.push_str(BEGIN_PREFIX);
                out.push_str(id);
                out.push_str(CLOSE);
                out.push('\n');
                out.push_str(body);
                out.push_str(END_PREFIX);
                out.push_str(id);
                out.push_str(CLOSE);
                out.push('\n');
            }
        }
    }
    out
}

/// Merge `template` (the freshly-rendered template body) into
/// `existing` (the on-disk file body) at the block level: replace each
/// existing auto block's body with the template's matching-id body,
/// preserve operator blocks verbatim, and append any template-only
/// auto blocks at the end. Existing auto blocks whose id no longer
/// appears in the template are emptied (kept as empty fenceposts to
/// preserve neighbouring operator content's relative position).
pub fn merge(existing: &str, template: &str) -> String {
    let existing_blocks = parse(existing);
    let template_blocks = parse(template);
    let template_bodies: HashMap<String, String> = template_blocks
        .iter()
        .filter_map(|b| match b {
            Block::Auto { id, body } => Some((id.clone(), body.clone())),
            Block::Operator(_) => None,
        })
        .collect();
    let mut existing_ids: std::collections::HashSet<String> = std::collections::HashSet::new();
    let mut merged: Vec<Block> = Vec::new();
    for b in existing_blocks {
        match b {
            Block::Operator(s) => merged.push(Block::Operator(s)),
            Block::Auto { id, body } => {
                existing_ids.insert(id.clone());
                let new_body = template_bodies.get(&id).cloned().unwrap_or_else(|| {
                    log::warn!(
                        "render::block::merge: existing auto block id={id} no longer in template; keeping empty"
                    );
                    String::new()
                });
                let _ = body;
                merged.push(Block::Auto { id, body: new_body });
            }
        }
    }
    // Append template-only auto blocks that did not appear in existing
    // (a new mode emerged, etc.). Preserve template order.
    for b in template_blocks {
        if let Block::Auto { id, body } = b
            && !existing_ids.contains(&id)
        {
            log::debug!("render::block::merge: appending new template id={id}");
            merged.push(Block::Auto { id, body });
        }
    }
    emit(&merged)
}

#[cfg(test)]
mod tests;
