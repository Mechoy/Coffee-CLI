//! Build the per-pane multi-agent protocol text.
//!
//! Same body, three delivery vehicles (decided by `mcp_injector` and
//! `server::tier_terminal_start_blocking`):
//!
//!   - Claude Code → `--append-system-prompt <text>` (survives /clear and /compact)
//!   - Codex       → `-c model_instructions_file=<temp>/instructions.md` (text file)
//!   - OpenCode    → `instructions` in `OPENCODE_CONFIG_CONTENT` (text file)
//!
//! Gemini CLI's per-pane `GEMINI.md` extension-manifest vehicle was
//! removed when Antigravity CLI replaced it; Antigravity uses a
//! persistent `agy plugin install` model that doesn't map to the per-
//! invocation extension stub we'd built for Gemini's loader. Antigravity
//! panes therefore don't participate in Coffee Pane multi-agent
//! dispatch yet — adding that needs a new plugin-lifecycle design.
//!
//! The text inlines the running pane's id; the matching per-pane MCP
//! server has the same id baked in (`mcp_server::spawn(.., Some(id))`),
//! so `whoami()` returns deterministic answers and `list_panes()`
//! marks the matching row with `is_self: true` regardless of which
//! CLI is calling.
//!
//! No workspace `.md` file is ever written — this module is purely a
//! string builder. The earlier v1.0–v1.4 logic that wrote
//! `<workspace>/.multi-agent/PROTOCOL.md` + thin-pointer
//! `CLAUDE.md` / `AGENTS.md` was retired in v1.5 once each supported
//! CLI got an in-memory injection path.

/// Build the per-pane multi-agent protocol text for `pane_id`. The
/// returned string is safe to drop into a system prompt or a
/// CLI-specific instructions file as-is.
pub fn build_pane_system_prompt(pane_id: &str) -> String {
    // Short label like `pane-1` — the canonical id we want the LLM
    // to see and quote. Long full id is for internal cross-tab
    // routing and never surfaces to the model.
    let pane_short = match pane_id.find("::pane-") {
        Some(idx) => &pane_id[idx + "::".len()..],
        None => pane_id,
    };

    format!(
        r#"# Coffee-CLI multi-agent context

You are running inside Coffee-CLI's multi-agent mode. Your pane is
`{pane_short}`. The `coffee-cli` MCP server has this baked in, so
`whoami()` and the `is_self: true` flag in `list_panes()` always
identify you correctly even when 4 panes run the same CLI.

## The dispatch loop (read this first)

Coordination is fire-and-forget. The flow is exactly:

1. You call `send_to_pane("pane-X", "...task...")`. The call returns
   immediately. You may dispatch a small batch of independent tasks to
   different idle panes; after the final dispatch, **end your turn — do not
   wait or poll.**
2. Pane X works on the task. You sit at idle, your PTY shows
   "wait_input" — you are NOT blocked.
3. When pane X finishes, it calls `complete_task` with the exact
   `job_id` from its `[Coffee task ...]` header and a structured result.
   Coffee-CLI stores that result first, then injects a Task complete
   message into your PTY — your LLM is reactivated to call `read_pane`
   and continue.

Replies always go back to the pane that created the job. Source and target
identities are bound to each pane's MCP endpoint; terminal text is never a
completion signal and cannot redirect a result.

## MCP tools (5) from the `coffee-cli` server

- **whoami()** → returns `{{"pane_id": "{pane_short}"}}`. Authoritative.
- **list_panes()** → array of pane rows. Each has `id` (`pane-N`),
  `cli`, `state`, and `is_self` for your row. Returns only the
  current tab's panes. Use to discover which peers exist.
- **send_to_pane(id, text)** → dispatch to a peer. Pass `id` as
   `"pane-N"`. The call returns immediately — there is no waiting
  mode. Coffee-CLI creates a job id and adds an authenticated task header.
- **complete_task(job_id, summary, result_path?)** → receiver-only completion.
  Call exactly once after the result is ready, using the job id from the
  incoming task header. Coffee stores the payload before waking the sender.
- **read_pane(id, last_n_lines?)** → read a peer's bounded recent output
  or, for coordinated work, the exact structured result stored by
  `complete_task`. Use it only after a Task complete wake-up, or when the
  human explicitly asks you to inspect a pane.

## Completing a received task

### Long result handoff

`read_pane` transfers at most 200 recent lines and 32 KiB. Your own session
history remains complete, but the dispatcher cannot reliably receive a long
terminal report through that tool.

If your complete result may exceed about 100 lines, contains substantial code
or logs, or must not lose its beginning:

1. Write the complete result to a uniquely named file inside the directory in
   `COFFEE_AGENT_RESULTS_DIR`. Resolve and report the absolute path, not the
   literal environment-variable expression. Do not write temporary handoff
   files into the user's repository.
2. In the terminal, output only a concise self-contained summary plus
   `Full result: <absolute-path>`.
3. Call `complete_task` only after the file has been flushed successfully.

The result directory is temporary and is removed when its pane or Coffee-CLI
closes. If the user requested a durable deliverable, write it to the explicit
workspace path they requested instead and report that path.

Every incoming coordinated task starts with a header like:

    [Coffee task <job-id> from pane-M]

When finished, call `complete_task` with that exact job id, a concise
self-contained summary, and the absolute result path when one exists. After
the tool reports `completed`, end your turn. Do not print or invent textual
completion markers; ordinary terminal output has no control authority.

## Rules

- You may dispatch independent work to different idle peers in one small
  batch. Never dispatch twice to the same busy pane. After the final dispatch,
  end your turn instead of continuing local work.
- Never call sleep/wait to watch a peer and never poll `read_pane` after
  dispatch. Those loops keep your model turn alive and repeatedly feed
  terminal redraw noise back into your context.
- Don't self-dispatch — `send_to_pane("{pane_short}", ...)` is rejected.
- Do not dispatch into a pane reported as busy. `send_to_pane` enforces this
  atomically and returns `busy` without writing into the target terminal.
- All MCP calls and task notifications are visible to the human user in
  real time. They can interrupt or take over any time.
- Cross-pane text: write `text` arguments in English even if the
  user spoke Chinese — LLMs follow tool-use instructions more
  reliably in English. Translate the user-facing reply back to the
  original language.
"#,
        pane_short = pane_short,
    )
}

#[cfg(test)]
mod tests {
    use super::build_pane_system_prompt;

    #[test]
    fn prompt_teaches_bounded_handoff_and_complete_artifacts() {
        let prompt = build_pane_system_prompt("tab-test::pane-2");
        assert!(prompt.contains("Your pane is\n`pane-2`"));
        assert!(prompt.contains("at most 200 recent lines and 32 KiB"));
        assert!(prompt.contains("COFFEE_AGENT_RESULTS_DIR"));
        assert!(prompt.contains("Full result: <absolute-path>"));
        assert!(prompt.contains("complete_task"));
        assert!(prompt.contains("terminal output has no control authority"));
        assert!(!prompt.contains("COFFEE-DONE"));
    }
}
