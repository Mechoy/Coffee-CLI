//! Per-pane MCP wiring for multi-agent mode.
//!
//! Each multi-agent pane gets:
//!   - a private temp dir at `<temp>/coffee-cli/panes/<sanitized-pane-id>/`
//!     holding the per-pane CLI artifacts (Claude mcp.json / Codex
//!     instructions.md)
//!   - a per-pane MCP HTTP server (with `self_pane_id` baked in at spawn
//!     time), independently of CLI kind. So `whoami()`, `list_panes()`'s
//!     `is_self`, and `[From <id>]` auto-prefixing in `send_to_pane()` are
//!     deterministic across all CLIs — no LLM guessing of pane identity
//!     even when 4 panes run the same CLI type.
//!
//! Per-CLI handoff (consumed by `server::tier_terminal_start_blocking`):
//!
//! | CLI      | Coffee CLI passes via …                                    | Pane reads from …                                         |
//! |----------|------------------------------------------------------------|-----------------------------------------------------------|
//! | Claude   | `--mcp-config <pane-temp>/claude-mcp.json`                 | that JSON file                                            |
//! | Codex    | `-c mcp_servers.coffee-cli.url='<url>'`                    | command-line override (no file)                           |
//! |          | `-c model_instructions_file='<pane-temp>/inst.md'`         | per-pane temp file (no workspace touch)                   |
//! | OpenCode | `OPENCODE_CONFIG_CONTENT=<merged-json>` env var            | highest-priority inline config with MCP and instructions  |
//! | All      | `COFFEE_AGENT_RESULTS_DIR=<pane-temp>/results` env var      | temporary complete-result handoff                         |
//!
//! Antigravity CLI (replaced Gemini CLI 2026-05-19) is NOT in the table:
//! its plugin model is `agy plugin install <name>` (persistent registry)
//! rather than a per-invocation `--extensions <name>` flag, so the
//! per-pane stub-dir trick we used for Gemini's hard-coded
//! `~/.gemini/extensions/` loader doesn't map. Antigravity panes spawn
//! without coffee-cli MCP wiring until a plugin-install lifecycle is
//! designed.
//!
//! Workspace pollution: zero. No `.md`, no `settings.json`, no
//! `mcp_servers` block ever lands in the user's project directory.
//!
//! Global pollution: zero. Purely command-line overrides + OS temp.
//! Auth safety: we never set `CODEX_HOME`, so Codex's
//! `~/.codex/auth.json` remains reachable. Codex `-c` overrides merge
//! onto the user's `~/.codex/config.toml` rather than replacing it.
//! User customisation and credentials are preserved.
//!
//! Lifecycle: `prune_pane_artifacts()` clears leftovers at app start and
//! shutdown; `remove_pane_artifacts()` removes an individual pane's files
//! when it closes. New artifacts are created lazily in
//! `prepare_pane_config_dir()` on every PTY spawn — content is rewritten
//! idempotently each time, safe to call repeatedly for the same pane id.

use std::{fs, io, path::PathBuf};

use crate::mcp_server::McpEndpoint;

/// Key used for the Coffee CLI entry in every per-pane CLI config.
pub const MCP_KEY: &str = "coffee-cli";

/// Output of [`prepare_pane_config_dir`]. The caller picks the right
/// field based on CLI kind. Default-empty when `cli_kind` doesn't
/// match a multi-agent CLI.
#[derive(Debug, Clone, Default)]
pub struct PaneConfigPaths {
    /// `cli_kind == "claude"` only. Pass via `--mcp-config <path>`.
    pub claude_mcp_config_path: Option<PathBuf>,
    /// `cli_kind == "codex"` only. Caller appends these straight onto
    /// the codex argv (already in `-c key=value` pairs, ready to spawn).
    pub codex_extra_args: Vec<String>,
    /// `cli_kind == "opencode"` only. Pass via
    /// `OPENCODE_CONFIG_CONTENT=<json>`. OpenCode loads inline content
    /// last, so project config cannot replace this pane's endpoint. Any
    /// inherited inline config is structurally preserved.
    pub opencode_config_content: Option<String>,
    /// Temporary directory for complete cross-pane results that are too long
    /// for the bounded `read_pane` terminal view. Passed to every supported
    /// pane as `COFFEE_AGENT_RESULTS_DIR` and removed with pane artifacts.
    pub result_dir: Option<PathBuf>,
}

/// Build per-pane CLI artifacts for `pane_id` running `cli_kind`,
/// pointed at `endpoint`. `protocol_text` is written into the CLI's
/// instructions file (Codex `instructions.md`). Claude takes its
/// protocol text via `--append-system-prompt` and doesn't read a
/// file here — caller passes the same `protocol_text` through that
/// flag separately.
///
/// Idempotent: re-invoking with the same args overwrites in place.
/// Unknown `cli_kind` returns the default empty `PaneConfigPaths`.
pub fn prepare_pane_config_dir(
    pane_id: &str,
    cli_kind: &str,
    endpoint: &McpEndpoint,
    protocol_text: &str,
) -> std::io::Result<PaneConfigPaths> {
    let dir = panes_root().join(sanitize_pane_id(pane_id));
    fs::create_dir_all(&dir)?;

    let mut out = PaneConfigPaths::default();
    if matches!(cli_kind, "claude" | "codex" | "opencode") {
        let result_dir = dir.join("results");
        fs::create_dir_all(&result_dir)?;
        out.result_dir = Some(result_dir);
    }
    match cli_kind {
        "claude" => {
            let p = dir.join("claude-mcp.json");
            fs::write(&p, claude_mcp_json(endpoint))?;
            out.claude_mcp_config_path = Some(p);
        }
        "codex" => {
            // Per-pane protocol text. Referenced by `-c
            // model_instructions_file=<path>` so Codex bakes it into
            // the model's session context. No workspace touch.
            //
            // Note on the key name: Codex 0.x exposed this as
            // `experimental_instructions_file`, but starting with the
            // 2026-04 release the `experimental_` prefix is deprecated
            // and silently ignored — Codex prints
            //   `experimental_instructions_file is deprecated and ignored.
            //    Use model_instructions_file instead.`
            // and our protocol injection becomes a no-op (the multi-agent
            // CLI then has no idea how to call send_to_pane). Use the
            // new key. Older Codex versions just don't recognise it and
            // emit a soft warning, which is the strictly better failure
            // mode (warning + still-runnable shell vs silent no-op).
            let inst = dir.join("instructions.md");
            fs::write(&inst, protocol_text)?;
            // Codex `-c key=value` parses `value` as a TOML scalar. Use
            // TOML literal-strings ('...') so Windows backslashes in
            // the temp path don't accidentally trigger TOML escape
            // sequences (e.g. `\U` would otherwise look like a unicode
            // escape leadin in a basic-string).
            out.codex_extra_args = vec![
                "-c".to_string(),
                format!("mcp_servers.{key}.url='{url}'", key = MCP_KEY, url = endpoint.url),
                "-c".to_string(),
                format!(
                    "model_instructions_file='{path}'",
                    path = inst.display()
                ),
            ];
        }
        "opencode" => {
            // OpenCode loads OPENCODE_CONFIG_CONTENT after global, custom,
            // and project config. That final precedence is required here:
            // a project's own `mcp.coffee-cli` entry must not redirect a
            // pane to another tab. Preserve any inline JSON inherited from
            // the user's environment before adding the pane-owned fields.
            let inst = dir.join("instructions.md");
            fs::write(&inst, protocol_text)?;
            let inherited = std::env::var("OPENCODE_CONFIG_CONTENT").ok();
            out.opencode_config_content = Some(opencode_config_json(
                endpoint,
                &inst,
                inherited.as_deref(),
            )?);
        }
        _ => {}
    }
    Ok(out)
}

/// Wipe per-pane artifacts from any previous Coffee CLI run:
///   - `<temp>/coffee-cli/panes/`
///
/// Called once at app start (recover from crash residue), once at app
/// shutdown (tidy exit). Best-effort — missing dirs and permission
/// glitches are logged but never returned as errors. New artifacts get
/// recreated lazily by `prepare_pane_config_dir()` as panes spawn.
pub fn prune_pane_artifacts() {
    let root = panes_root();
    if root.exists() {
        if let Err(e) = fs::remove_dir_all(&root) {
            log::warn!(
                "[mcp-inject] prune {} failed: {} (will recreate per-pane dirs lazily)",
                root.display(),
                e
            );
        }
    }
}

/// Remove one pane's temporary CLI configuration when its terminal closes.
pub fn remove_pane_artifacts(pane_id: &str) {
    let dir = panes_root().join(sanitize_pane_id(pane_id));
    if let Err(e) = fs::remove_dir_all(&dir) {
        if e.kind() != std::io::ErrorKind::NotFound {
            log::warn!("[mcp-inject] remove {} failed: {}", dir.display(), e);
        }
    }
}

fn panes_root() -> PathBuf {
    std::env::temp_dir().join("coffee-cli").join("panes")
}

/// Pane ids contain `::` and `/` which are unfriendly for filenames
/// on Windows. Replace anything outside `[A-Za-z0-9_-]` with `_`.
fn sanitize_pane_id(pane_id: &str) -> String {
    pane_id
        .chars()
        .map(|c| match c {
            'a'..='z' | 'A'..='Z' | '0'..='9' | '-' | '_' => c,
            _ => '_',
        })
        .collect()
}

fn claude_mcp_json(endpoint: &McpEndpoint) -> String {
    let body = serde_json::json!({
        "mcpServers": {
            MCP_KEY: {
                "type": "http",
                "url": endpoint.url,
            }
        }
    });
    serde_json::to_string_pretty(&body).unwrap_or_default()
}

fn opencode_config_json(
    endpoint: &McpEndpoint,
    instructions_path: &std::path::Path,
    inherited: Option<&str>,
) -> io::Result<String> {
    // Three things this per-pane config must do:
    //
    // 1. MCP server: `type: "remote"` for HTTP endpoints, `url` is the
    //    base URL. Matches Claude's `type: "http"` semantically but uses
    //    OpenCode's own naming (see opencode.ai/docs/mcp-servers).
    //
    // 2. `instructions`: OpenCode officially accepts absolute instruction
    //    file paths. This gives it the same dispatch/DONE protocol that
    //    Claude and Codex receive.
    //
    // 3. `permission: "allow"`: OpenCode's TUI has no
    //    `--dangerously-skip-permissions` CLI flag (only `opencode run`
    //    does); the only hands-free path is the config field. The single-
    //    string form blanket-approves every permission category — read,
    //    edit, bash, webfetch, external_directory, etc. — which is the
    //    correct level of trust for multi-agent mode where another
    //    pane's LLM is dispatching work and there's no human at the
    //    keyboard to click "Allow". Per-pane config means the user's
    //    own standalone-OpenCode runs (in other terminals, with a
    //    human watching) keep their normal interactive permissions.
    let mut body = match inherited.filter(|value| !value.trim().is_empty()) {
        Some(value) => serde_json::from_str::<serde_json::Value>(value).map_err(|e| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                format!("invalid inherited OPENCODE_CONFIG_CONTENT: {e}"),
            )
        })?,
        None => serde_json::json!({}),
    };
    let root = body.as_object_mut().ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            "inherited OPENCODE_CONFIG_CONTENT must be a JSON object",
        )
    })?;

    root.entry("$schema".to_string())
        .or_insert_with(|| serde_json::json!("https://opencode.ai/config.json"));

    let instruction = instructions_path.display().to_string();
    let instructions = root
        .entry("instructions".to_string())
        .or_insert_with(|| serde_json::json!([]))
        .as_array_mut()
        .ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                "OPENCODE_CONFIG_CONTENT.instructions must be an array",
            )
        })?;
    if !instructions.iter().any(|value| value.as_str() == Some(&instruction)) {
        instructions.push(serde_json::Value::String(instruction));
    }

    // Multi-agent panes are unattended while peer work runs, so the
    // pane-local inline override intentionally enables all permissions.
    root.insert("permission".to_string(), serde_json::json!("allow"));

    let mcp = root
        .entry("mcp".to_string())
        .or_insert_with(|| serde_json::json!({}))
        .as_object_mut()
        .ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                "OPENCODE_CONFIG_CONTENT.mcp must be an object",
            )
        })?;
    mcp.insert(
        MCP_KEY.to_string(),
        serde_json::json!({
            "type": "remote",
            "url": endpoint.url,
        }),
    );

    serde_json::to_string_pretty(&body).map_err(io::Error::other)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ep() -> McpEndpoint {
        McpEndpoint {
            url: "http://127.0.0.1:50000/mcp".into(),
            abort_handle: None,
        }
    }

    fn unique_pane(label: &str) -> String {
        format!("test::pane-{}-{}", label, std::process::id())
    }

    #[test]
    fn claude_writes_mcp_json_with_url() {
        let pid = unique_pane("claude");
        let out = prepare_pane_config_dir(&pid, "claude", &ep(), "PROMPT").unwrap();
        let p = out.claude_mcp_config_path.expect("claude returns path");
        let body = fs::read_to_string(&p).unwrap();
        assert!(body.contains("coffee-cli"));
        assert!(body.contains("http://127.0.0.1:50000/mcp"));
        assert!(out.result_dir.as_ref().is_some_and(|dir| dir.is_dir()));
        let _ = fs::remove_dir_all(panes_root().join(sanitize_pane_id(&pid)));
    }

    #[test]
    fn codex_returns_minus_c_args_only() {
        let pid = unique_pane("codex");
        let out = prepare_pane_config_dir(&pid, "codex", &ep(), "PROTOCOL BODY").unwrap();
        assert!(out.claude_mcp_config_path.is_none());
        assert_eq!(out.codex_extra_args.len(), 4);
        assert_eq!(out.codex_extra_args[0], "-c");
        assert!(out.codex_extra_args[1].contains("mcp_servers.coffee-cli.url"));
        assert!(out.codex_extra_args[1].contains("http://127.0.0.1:50000/mcp"));
        assert_eq!(out.codex_extra_args[2], "-c");
        assert!(out.codex_extra_args[3].contains("model_instructions_file"));
        assert!(out.result_dir.as_ref().is_some_and(|dir| dir.is_dir()));
        // Protocol text actually got written.
        let inst_path = panes_root()
            .join(sanitize_pane_id(&pid))
            .join("instructions.md");
        let body = fs::read_to_string(&inst_path).unwrap();
        assert_eq!(body, "PROTOCOL BODY");
        let _ = fs::remove_dir_all(panes_root().join(sanitize_pane_id(&pid)));
    }

    #[test]
    fn opencode_returns_inline_config_with_url_and_allow_permission() {
        let pid = unique_pane("opencode");
        let out = prepare_pane_config_dir(&pid, "opencode", &ep(), "IGNORED").unwrap();
        let body = out
            .opencode_config_content
            .expect("opencode returns inline content");
        assert!(out.claude_mcp_config_path.is_none());
        assert!(out.codex_extra_args.is_empty());
        let value: serde_json::Value = serde_json::from_str(&body).unwrap();
        assert_eq!(value["mcp"]["coffee-cli"]["type"], "remote");
        assert_eq!(
            value["mcp"]["coffee-cli"]["url"],
            "http://127.0.0.1:50000/mcp"
        );
        let inst_path = panes_root()
            .join(sanitize_pane_id(&pid))
            .join("instructions.md");
        assert_eq!(value["instructions"][0], inst_path.display().to_string());
        assert_eq!(fs::read_to_string(&inst_path).unwrap(), "IGNORED");
        // permission: "allow" is the only hands-free path for OpenCode TUI
        // (no --dangerously-skip-permissions equivalent). Without it,
        // multi-agent dispatch into an OpenCode pane wedges on the first
        // permission prompt with no human present to approve.
        assert_eq!(value["permission"], "allow");
        assert!(!panes_root()
            .join(sanitize_pane_id(&pid))
            .join("opencode.json")
            .exists());
        let _ = fs::remove_dir_all(panes_root().join(sanitize_pane_id(&pid)));
    }

    #[test]
    fn opencode_preserves_inherited_inline_config() {
        let inherited = serde_json::json!({
            "theme": "coffee",
            "instructions": ["/existing/instructions.md"],
            "mcp": {
                "existing": { "type": "remote", "url": "http://127.0.0.1:1/mcp" },
                "coffee-cli": { "type": "remote", "url": "http://stale.invalid/mcp" }
            }
        })
        .to_string();
        let value: serde_json::Value = serde_json::from_str(
            &opencode_config_json(
                &ep(),
                std::path::Path::new("/coffee/instructions.md"),
                Some(&inherited),
            )
            .unwrap(),
        )
        .unwrap();

        assert_eq!(value["theme"], "coffee");
        assert_eq!(value["instructions"][0], "/existing/instructions.md");
        assert_eq!(value["instructions"][1], "/coffee/instructions.md");
        assert_eq!(value["mcp"]["existing"]["url"], "http://127.0.0.1:1/mcp");
        assert_eq!(
            value["mcp"]["coffee-cli"]["url"],
            "http://127.0.0.1:50000/mcp"
        );
    }

    #[test]
    fn unknown_cli_kind_is_a_noop() {
        let pid = unique_pane("unknown");
        let out = prepare_pane_config_dir(&pid, "qwen", &ep(), "ignored").unwrap();
        assert!(out.claude_mcp_config_path.is_none());
        assert!(out.codex_extra_args.is_empty());
        assert!(out.opencode_config_content.is_none());
        assert!(out.result_dir.is_none());
        let _ = fs::remove_dir_all(panes_root().join(sanitize_pane_id(&pid)));
    }
}
