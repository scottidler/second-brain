# Design Document: Oracle Call Subcommand

**Author:** Scott Idler
**Date:** 2026-03-29
**Status:** Implemented
**Review Passes Completed:** 5/5

## Summary

Add an `oracle call` subcommand for direct tool invocation without MCP transport. Developers and scripts can call any of oracle's 18 tools from the command line, get JSON on stdout, and skip the MCP handshake entirely.

## Problem Statement

### Background

Oracle serves 18 tools via MCP over stdio using rmcp 1.3.0 (NDJSON framing). The only way to invoke a tool today is through a full MCP session: initialize handshake, send `notifications/initialized`, send a tool call, parse NDJSON response. This workflow is handled automatically by MCP clients like Claude Code, but there is no way to call a tool directly.

### Problem

Invoking oracle tools outside of an MCP client requires hand-crafting NDJSON protocol messages. The [transport test report](../2026-03-29-oracle-transport-test-report.md) demonstrated this pain - verifying `schema_info` output required constructing a three-message MCP handshake sequence by hand. This makes oracle tools inaccessible for:

- **Developer testing:** verifying tool output during development
- **Shell scripting:** piping oracle results into jq or other tools
- **Debugging:** isolating tool logic bugs from transport issues

### Goals

- Invoke any oracle tool from the command line with a single command
- Get JSON output on stdout, suitable for piping to jq
- List available tools for discoverability
- No MCP transport, no handshake, no session lifecycle

### Non-Goals

- Interactive REPL or multi-tool sessions
- Replacing MCP transport for client integrations
- Adding new tool handlers (this is purely a new invocation path for existing tools)
- Supporting structured output formats beyond JSON (e.g., table, CSV)

## Proposed Solution

### Overview

Add a `Call` variant to the CLI that instantiates `OracleMcpServer`, dispatches directly to the matching tool handler method, and prints the result as JSON to stdout. No MCP session, no rmcp transport involved.

### CLI Design

```
oracle call <tool>                                # call with default {} args
oracle call <tool> --json '{"query": "rust"}'     # call with explicit args
oracle call --list                                # list all 18 tools
```

Add to `Commands` enum in `cli.rs`:

```rust
/// Call a tool directly (no MCP transport)
Call {
    /// Tool name (use --list to see available tools)
    #[arg(required_unless_present = "list")]
    tool: Option<String>,
    /// JSON arguments (default: {})
    #[arg(long)]
    json: Option<String>,
    /// List available tool names
    #[arg(long)]
    list: bool,
},
```

Clap enforces: either `tool` must be provided, or `--list` must be set.

### Dispatch Method

Add `dispatch()` to `OracleMcpServer` with a plain match on tool name. Each arm deserializes JSON into the tool's request type and calls the handler. 18 arms is manageable without a macro.

```rust
impl OracleMcpServer {
    pub async fn dispatch(
        &self,
        name: &str,
        args: serde_json::Value,
    ) -> Result<CallToolResult, McpError> {
        match name {
            "knowledge_search" => {
                let req: KnowledgeSearchRequest =
                    serde_json::from_value(args).map_err(|e| Self::deser_err(name, &e))?;
                self.knowledge_search(Parameters(req)).await
            }
            "note_read" => {
                let req: NoteReadRequest =
                    serde_json::from_value(args).map_err(|e| Self::deser_err(name, &e))?;
                self.note_read(Parameters(req)).await
            }
            // ... remaining 16 tools follow the same pattern
            _ => Err(McpError::invalid_params(
                format!("unknown tool: {name} (use oracle call --list)"),
                None,
            )),
        }
    }

    fn deser_err(tool: &str, e: &serde_json::Error) -> McpError {
        McpError::invalid_params(format!("{tool}: {e}"), None)
    }
}
```

Each arm is mechanical: deserialize `Value` into `RequestType`, wrap in `Parameters()`, call handler. The `deser_err` helper produces clear messages like `knowledge_search: missing field 'query'`.

### Entry Point

Add `run_call()` in `main.rs`:

```rust
async fn run_call(config: Config, tool: &str, args_json: Option<&str>) -> Result<()> {
    let db = SearchIndex::open(&config.db_path()).context("Failed to open database")?;
    db.index_vault(&config.vault_root()).context("Failed to index vault")?;

    let server = OracleMcpServer::new(config, db);

    let args: serde_json::Value = match args_json {
        Some(json) => serde_json::from_str(json).context("invalid JSON arguments")?,
        None => serde_json::json!({}),
    };

    let result = server.dispatch(tool, args).await
        .map_err(|e| eyre::eyre!("{}", e.message))?;

    if result.is_error == Some(true) {
        // Tool returned an error result (not a Rust error, but a tool-level error)
        for content in &result.content {
            if let Some(text) = content.as_text() {
                eprintln!("{}", text.text);
            }
        }
        std::process::exit(1);
    }

    for content in &result.content {
        if let Some(text) = content.as_text() {
            // Content::json() produces Text with JSON-as-string; try to pretty-print
            if let Ok(parsed) = serde_json::from_str::<serde_json::Value>(&text.text) {
                println!("{}", serde_json::to_string_pretty(&parsed)?);
            } else {
                println!("{}", text.text);
            }
        }
    }

    Ok(())
}
```

Key details:
- **Vault indexing:** Same index pass as `run_serve` startup - ensures fresh data.
- **Content extraction:** `Content` is `Annotated<RawContent>` which `Deref`s to `RawContent`. The `as_text()` method returns `Option<&RawTextContent>`. Since `Content::json()` serializes to a string and stores it as `RawContent::Text`, all oracle tool output is accessible via `as_text().text`.
- **JSON pretty-printing:** Tool output is JSON-as-string. Parse and re-serialize with indentation for human readability. Falls back to raw text for non-JSON content (e.g., "Note not found" messages).
- **Error results:** `CallToolResult.is_error` is `Some(true)` for tool-level errors. Print to stderr and exit 1.

### List Implementation

For `--list`, use the generated `tool_router()` class method - no database, config, or server instance needed:

```rust
fn run_list() {
    let router = OracleMcpServer::tool_router();
    for tool in router.list_all() {
        println!("{:<20} {}", tool.name, tool.description.as_deref().unwrap_or(""));
    }
}
```

`ToolRouter::list_all()` returns `Vec<Tool>` sorted by name. Each `Tool` has `name: Cow<'static, str>` and `description: Option<Cow<'static, str>>`.

### Implementation Plan

**Files changed:**

| File | Change |
|------|--------|
| `oracle/src/cli.rs` | Add `Call` variant to `Commands` enum |
| `oracle/src/server.rs` | Add `dispatch()` and `deser_err()` methods to `OracleMcpServer` |
| `oracle/src/main.rs` | Add `run_call()`, `run_list()`, wire into `main()` match |

**Order:**

1. Add `Call` variant to `Commands` in `cli.rs`
2. Add `dispatch()` method to `OracleMcpServer` in `server.rs` (18 match arms)
3. Add `run_call()` and `run_list()` in `main.rs`
4. Wire `Commands::Call` into the `main()` match
5. Verify with `oracle call --list` and `oracle call schema_info`

No new modules, no new dependencies, no changes to the serve path.

## Alternatives Considered

### Alternative 1: Reuse ToolRouter::call()
- **Description:** Use rmcp's built-in `ToolRouter::call()` for dispatch instead of a custom match
- **Pros:** No match table to maintain; automatic sync with `#[tool_router]` registration
- **Cons:** `ToolRouter::call()` requires a `RequestContext<RoleServer>` tied to a live MCP session. Constructing one without a real session means reaching into rmcp internals and faking session state.
- **Why not chosen:** The dispatch match is simple and explicit. Coupling to rmcp session internals would be fragile across rmcp version upgrades.

### Alternative 2: Dispatch Macro
- **Description:** Generate match arms with a `dispatch_tool!` macro to reduce repetition
- **Pros:** Less boilerplate per arm
- **Cons:** Adds indirection; macro debugging is harder; 18 arms is manageable without it
- **Why not chosen:** Plain match is more readable. Each arm is 3 lines. If oracle grows past 30+ tools, a macro could be reconsidered.

### Alternative 3: Shell Wrapper Script
- **Description:** A script that constructs MCP handshake messages, pipes to `oracle serve`, parses NDJSON output
- **Pros:** No code changes to oracle
- **Cons:** Fragile (must track protocol changes), slow (full MCP lifecycle per call), error handling is poor
- **Why not chosen:** A first-class subcommand is simpler, faster, and maintainable.

## Technical Considerations

### Dependencies

No new dependencies. All required APIs already available:
- `serde_json::from_value` / `from_str` - deserialization (serde_json workspace dep)
- `OracleMcpServer::new()`, `SearchIndex::open()` - existing constructors
- `rmcp::handler::server::wrapper::Parameters<T>` - existing newtype wrapper
- `rmcp::model::{CallToolResult, Content, Tool}` - existing types
- `ToolRouter::list_all()` - existing method on generated router

### Performance

`oracle call` bypasses MCP transport - no NDJSON parsing, no handshake, no session lifecycle. The only startup cost is vault indexing (same as `oracle serve`). Index is incremental - only changed files are processed.

### Security

`oracle call` is a local CLI command reading from a local SQLite database and local markdown files. No network exposure. JSON args are deserialized through serde with the same typed validation as MCP tool calls - invalid enum values, missing required fields, and type mismatches all produce clear errors.

### Testing Strategy

**CLI smoke tests:**
1. `oracle call schema_info` - valid JSON output, exit 0
2. `oracle call knowledge_search --json '{"query":"test"}'` - results array
3. `oracle call nonexistent` - error message mentioning `--list`, non-zero exit
4. `oracle call knowledge_search --json 'garbage'` - parse error, non-zero exit
5. `oracle call --list` - prints 18 tool names, one per line

**Regression:**
- `oracle serve` behavior unchanged. `dispatch()` is additive - no changes to the serve path or transport wiring.

### Rollout Plan

Ship via `cargo install --path oracle`. Oracle is an MCP server launched on demand - no systemd restart needed.

## Risks and Mitigations

| Risk | Likelihood | Impact | Mitigation |
|------|------------|--------|------------|
| New tool added to `#[tool_router]` but not to `dispatch()` | Med | Low | `oracle call` returns "unknown tool" for that name; add CI smoke test comparing `--list` count with dispatch arm count |
| serde ignores extra JSON fields silently | Low | Low | Matches MCP behavior - not a regression. Could add `#[serde(deny_unknown_fields)]` later if strictness is desired. |
| Vault index adds latency on every call | Low | Low | Index is incremental (ms for unchanged vault). For hot-loop scripting, user can pre-index with `oracle index`. |

## Open Questions

None. This is a straightforward CLI addition with no architectural decisions pending.

## References

- [Transport test report](../2026-03-29-oracle-transport-test-report.md) - demonstrated the need for direct tool invocation during testing
- rmcp 1.3.0 `Content` type: `Annotated<RawContent>` with `Deref` to `RawContent`; `Content::json()` produces `Text` variant
- rmcp 1.3.0 `ToolRouter::list_all()` returns `Vec<Tool>` with `name` and `description` fields
