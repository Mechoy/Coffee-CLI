//! Coffee-CLI multi-agent MCP server.
//!
//! Exposes structured coordination tools over HTTP Streamable MCP transport:
//! - `whoami()` — identify the caller's pane.
//! - `list_panes()` — enumerate panes in the current multi-agent Tab with
//!   their CLI type, state (empty / idle / busy / terminated), and titles.
//! - `send_to_pane(id, text)` — inject a task into another pane's PTY and
//!   return immediately.
//! - `complete_task(job_id, summary, result_path)` — atomically publish a
//!   task result and notify the dispatcher.
//! - `read_pane(id, last_n_lines)` — read a peer's structured task result,
//!   falling back to bounded terminal output for explicit inspection.
//!
//! HTTP transport (not stdio) because Coffee-CLI is a resident Tauri
//! process and can't be spawned as a subprocess by each CLI. Every pane gets
//! its own `127.0.0.1:<random>` listener and temporary CLI configuration;
//! user and workspace configuration files are left untouched.
//!
//! Task state never comes from terminal text. Each dispatch creates a job
//! bound to its source and target panes. Only the target's identity-bound MCP
//! endpoint can complete that job, and the result is stored before a typed
//! Tauri event wakes the dispatcher. Terminal output is presentation only.
//!
//! History: MCP was retired in 2026-04-24 in a misread of the user's
//! product intent ("sentinel is on-top-of MCP, not replacement-for") and
//! restored 2026-04-25. See docs/MULTI-AGENT-ARCHITECTURE.md §九 decision
//! log for the embarrassing details.

use std::{
    collections::HashMap,
    io::Write,
    sync::{Arc, Mutex},
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use crate::terminal::SharedSession;

const READ_PANE_DEFAULT_LINES: usize = 80;
const READ_PANE_MAX_LINES: usize = 200;
const READ_PANE_MAX_BYTES: usize = 32 * 1024;

use rmcp::{
    ErrorData as McpError, ServerHandler,
    handler::server::{
        router::tool::ToolRouter,
        wrapper::Parameters,
    },
    model::*,
    schemars::JsonSchema,
    tool, tool_handler, tool_router,
    transport::streamable_http_server::{
        StreamableHttpServerConfig, StreamableHttpService,
        session::local::LocalSessionManager,
    },
};
use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Emitter};

const COMPLETION_SUMMARY_MAX_BYTES: usize = 16 * 1024;
const COMPLETION_PATH_MAX_BYTES: usize = 4 * 1024;
const MAX_TASK_RECORDS: usize = 256;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PaneTaskStatus {
    Running,
    Completed,
    Failed,
}

#[derive(Clone, Debug)]
struct PaneTaskRecord {
    job_id: String,
    source_id: String,
    target_id: String,
    status: PaneTaskStatus,
    summary: Option<String>,
    result_path: Option<String>,
    error: Option<String>,
    created_at: u64,
}

#[derive(Clone, Debug, Serialize)]
pub struct PaneTaskEvent {
    pub job_id: String,
    pub source_id: String,
    pub target_id: String,
    pub status: PaneTaskStatus,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

#[derive(Default)]
struct TaskCoordinatorState {
    records: HashMap<String, PaneTaskRecord>,
    current_by_target: HashMap<String, String>,
    latest_by_route: HashMap<(String, String), String>,
}

#[derive(Default)]
pub struct TaskCoordinator {
    state: Mutex<TaskCoordinatorState>,
}

impl TaskCoordinator {
    pub fn new() -> Self {
        Self::default()
    }

    fn start(&self, source_id: &str, target_id: &str) -> Result<String, String> {
        let mut state = self
            .state
            .lock()
            .map_err(|_| "task coordinator poisoned".to_string())?;
        if let Some(job_id) = state.current_by_target.get(target_id) {
            if let Some(record) = state.records.get(job_id) {
                if record.status == PaneTaskStatus::Running {
                    return Err(format!(
                        "target pane is busy with job {} from {}",
                        record.job_id,
                        pane_short(&record.source_id)
                    ));
                }
            }
        }

        prune_task_records(&mut state);
        let job_id = uuid::Uuid::new_v4().to_string();
        let record = PaneTaskRecord {
            job_id: job_id.clone(),
            source_id: source_id.to_string(),
            target_id: target_id.to_string(),
            status: PaneTaskStatus::Running,
            summary: None,
            result_path: None,
            error: None,
            created_at: epoch_seconds(),
        };
        state
            .current_by_target
            .insert(target_id.to_string(), job_id.clone());
        state.records.insert(job_id.clone(), record);
        Ok(job_id)
    }

    fn abort_start(&self, job_id: &str) {
        let Ok(mut state) = self.state.lock() else {
            return;
        };
        let Some(record) = state.records.remove(job_id) else {
            return;
        };
        if state.current_by_target.get(&record.target_id).map(String::as_str) == Some(job_id) {
            state.current_by_target.remove(&record.target_id);
        }
    }

    fn complete(
        &self,
        target_id: &str,
        job_id: &str,
        summary: String,
        result_path: Option<String>,
    ) -> Result<(PaneTaskRecord, bool), String> {
        let mut state = self
            .state
            .lock()
            .map_err(|_| "task coordinator poisoned".to_string())?;
        let current = state.current_by_target.get(target_id).map(String::as_str);
        if current != Some(job_id) {
            return Err("job is stale or does not belong to this pane".to_string());
        }
        let (source_id, target_id_owned, record_clone) = {
            let record = state
                .records
                .get_mut(job_id)
                .ok_or_else(|| "job not found".to_string())?;
            if record.target_id != target_id {
                return Err("job target identity mismatch".to_string());
            }
            if record.status == PaneTaskStatus::Completed {
                return Ok((record.clone(), false));
            }
            if record.status != PaneTaskStatus::Running {
                return Err(format!("job is already {:?}", record.status));
            }
            record.status = PaneTaskStatus::Completed;
            record.summary = Some(summary);
            record.result_path = result_path;
            (
                record.source_id.clone(),
                record.target_id.clone(),
                record.clone(),
            )
        };
        state.latest_by_route.insert(
            (source_id, target_id_owned),
            job_id.to_string(),
        );
        Ok((record_clone, true))
    }

    pub fn fail_target(&self, target_id: &str, error: String) -> Option<PaneTaskEvent> {
        let mut state = self.state.lock().ok()?;
        let job_id = state.current_by_target.get(target_id)?.clone();
        let (source_id, target_id_owned) = {
            let record = state.records.get_mut(&job_id)?;
            if record.status != PaneTaskStatus::Running {
                return None;
            }
            record.status = PaneTaskStatus::Failed;
            record.error = Some(error.clone());
            (record.source_id.clone(), record.target_id.clone())
        };
        state.current_by_target.remove(target_id);
        state.latest_by_route.insert(
            (source_id.clone(), target_id_owned.clone()),
            job_id.clone(),
        );
        Some(PaneTaskEvent {
            job_id,
            source_id: target_id_owned,
            target_id: source_id,
            status: PaneTaskStatus::Failed,
            error: Some(error),
        })
    }

    fn running_for_target(&self, target_id: &str) -> Option<PaneTaskRecord> {
        let state = self.state.lock().ok()?;
        let job_id = state.current_by_target.get(target_id)?;
        let record = state.records.get(job_id)?;
        (record.status == PaneTaskStatus::Running).then(|| record.clone())
    }

    fn latest_for_route(&self, source_id: &str, target_id: &str) -> Option<PaneTaskRecord> {
        let state = self.state.lock().ok()?;
        if let Some(current_id) = state.current_by_target.get(target_id) {
            if let Some(record) = state.records.get(current_id) {
                if record.source_id == source_id && record.status == PaneTaskStatus::Running {
                    return Some(record.clone());
                }
            }
        }
        let job_id = state
            .latest_by_route
            .get(&(source_id.to_string(), target_id.to_string()))?;
        state.records.get(job_id).cloned()
    }
}

fn prune_task_records(state: &mut TaskCoordinatorState) {
    if state.records.len() < MAX_TASK_RECORDS {
        return;
    }
    let mut terminal: Vec<(u64, String)> = state
        .records
        .values()
        .filter(|record| record.status != PaneTaskStatus::Running)
        .map(|record| (record.created_at, record.job_id.clone()))
        .collect();
    terminal.sort_by_key(|(created_at, _)| *created_at);
    let remove_count = state.records.len().saturating_sub(MAX_TASK_RECORDS - 1);
    for (_, job_id) in terminal.into_iter().take(remove_count) {
        state.records.remove(&job_id);
        state.latest_by_route.retain(|_, value| value != &job_id);
        state.current_by_target.retain(|_, value| value != &job_id);
    }
}

// ---------- Pane abstraction (in-memory mock for v1.0 day 1-2) ----------

/// State of a single pane as visible to the primary CLI.
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
#[schemars(crate = "rmcp::schemars")]
pub enum PaneState {
    /// Pane has no CLI running yet; user hasn't selected one.
    Empty,
    /// PTY is alive and the CLI is accepting input.
    Idle,
    /// CLI is producing output or awaiting long task completion.
    Busy,
    /// PTY exited.
    Terminated,
}

/// Snapshot of a pane returned by `list_panes`.
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
#[schemars(crate = "rmcp::schemars")]
pub struct PaneInfo {
    /// Short pane label like `pane-1`. Pass straight to `send_to_pane`
    /// / `read_pane`. (`list_panes` is already scoped to the caller's
    /// own tab, so a short tab-relative label is unambiguous; the
    /// long `tab-<uuid>::pane-N` form is also accepted on input but
    /// no longer returned here — it just blew up tool-call rendering
    /// inside narrow grid panes for no benefit.)
    pub id: String,
    /// Same as `id`. Kept for callers that read `title` to label rows.
    pub title: String,
    /// Raw full pane id (`tab-<uuid>::pane-N`). Used internally for
    /// tab-scope filtering and self-detection — `#[serde(skip)]` so
    /// it's never sent to the LLM (the whole point of this rewrite
    /// was to keep long UUIDs out of the model's context).
    #[serde(skip, default)]
    pub full_id: String,
    /// CLI running in this pane (claude / codex / antigravity / opencode / shell / ...).
    pub cli: String,
    pub state: PaneState,
    /// Epoch seconds of last output from this pane.
    pub last_activity_at: u64,
    /// `true` only for the row representing the caller's own pane.
    /// Set by the MCP server based on its baked-in `self_pane_id`,
    /// so a CLI receiving this list knows unambiguously which entry
    /// is itself — even when 4 panes run the same CLI type.
    /// Omitted (None / not serialized) if the server doesn't know
    /// the caller's identity.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub is_self: Option<bool>,
}

/// Live pane store bridging the MCP layer to `terminal::SharedSession`.
///
/// Each Coffee-CLI terminal session (one per Tab pane) is visible here as
/// a "pane". The primary pane's CLI (Claude Code / Codex / OpenCode)
/// calls MCP tools; we translate those calls into direct operations on
/// the other panes' PTYs.
pub struct PaneStore {
    session: SharedSession,
    tasks: Arc<TaskCoordinator>,
    app: AppHandle,
    /// ANSI escape sequence matcher, reused across reads.
    /// Same pattern as terminal.rs emitter thread (line ~738).
    ansi_re: regex::Regex,
}

impl PaneStore {
    pub fn new(session: SharedSession, tasks: Arc<TaskCoordinator>, app: AppHandle) -> Self {
        Self {
            session,
            tasks,
            app,
            ansi_re: regex::Regex::new(
                r"\x1b\[[0-9;?]*[ -/]*[@-~]|\x1b\].*?(?:\x07|\x1b\\)|\x1b.",
            )
            .expect("ANSI regex compiles"),
        }
    }

    /// Snapshot every session in the shared map as a PaneInfo row.
    ///
    /// This internal snapshot returns every session in the process. The MCP
    /// handler filters it to the caller's tab before returning list_panes.
    async fn list(&self) -> Vec<PaneInfo> {
        // Extract everything we need under a brief lock, then drop it.
        let raw = tokio::task::spawn_blocking({
            let session = self.session.clone();
            move || {
                let guard = session.lock().ok()?;
                let rows: Vec<(String, Option<String>, String, Instant)> = guard
                    .iter()
                    .map(|(id, sess)| {
                        let (status, last_at) = match sess.activity.lock() {
                            Ok(act) => {
                                let stale = act.last_output_at.elapsed() > Duration::from_secs(15);
                                let status = act.native_status.clone().unwrap_or_else(|| {
                                    if act.last_status == "working" && stale {
                                        "wait_input".to_string()
                                    } else {
                                        act.last_status.clone()
                                    }
                                });
                                (status, act.last_output_at)
                            }
                            Err(_) => ("unknown".to_string(), Instant::now()),
                        };
                        (id.clone(), sess.tool_name.clone(), status, last_at)
                    })
                    .collect();
                Some(rows)
            }
        })
        .await
        .unwrap_or(None)
        .unwrap_or_default();

        let now_instant = Instant::now();
        let now_epoch = epoch_seconds();

        let mut list: Vec<PaneInfo> = raw
            .into_iter()
            .map(|(id, tool_name, status, last_at)| {
                let elapsed = now_instant.saturating_duration_since(last_at).as_secs();
                let last_activity_at = now_epoch.saturating_sub(elapsed);
                let pane_label = pane_short(&id);
                let task_running = self.tasks.running_for_target(&id).is_some();
                PaneInfo {
                    title: pane_label.clone(),
                    cli: tool_name.unwrap_or_else(|| "shell".to_string()),
                    state: if task_running {
                        PaneState::Busy
                    } else {
                        status_to_pane_state(&status)
                    },
                    last_activity_at,
                    id: pane_label,
                    full_id: id,
                    is_self: None, // filled in by CoffeeMcp::list_panes if known
                }
            })
            .collect();
        list.sort_by(|a, b| a.id.cmp(&b.id));
        list
    }

    /// Inject text into the target pane's PTY stdin and, when `wait=true`,
    /// block until the pane's CLI returns to its prompt (or `timeout_sec`
    /// elapses), then return the ANSI-stripped output that arrived since
    /// the write.
    ///
    /// `submit=true` (default) auto-appends `\r` if the text isn't already
    /// newline-terminated, so the target CLI actually executes the command
    /// instead of leaving it in the input box. The carriage return also
    /// also marks the pane as `"working"`; the wait loop changes it back to
    /// `"wait_input"` after the response settles.
    ///
    /// Output capture is task-scoped: the MCP-only buffer is cleared before
    /// the write, then read after idle detection. Terminal rendering and the
    /// CLI's own session history are independent and remain untouched.
    async fn dispatch(
        &self,
        id: &str,
        text: &str,
        submit: bool,
        wait: bool,
        timeout_sec: u64,
    ) -> Result<DispatchResult, String> {
        // Strip any caller-provided trailing newline; we always append our
        // own in a SECOND write so Ink/React-based REPLs (Claude Code,
        // historically Gemini CLI before its sunset) treat the body and
        // the Enter as two separate stdin events — not one pasted chunk
        // where the final \r gets swallowed as part of the text. Observed
        // live on Gemini CLI: a combined "body\r" write showed up in the
        // input box but never submitted; splitting
        // body + short sleep + "\r" reliably submits.
        let body = text.trim_end_matches(['\r', '\n']).to_string();
        let should_submit = submit;
        let bytes_written = body.len() + if should_submit { 1 } else { 0 };

        // Phase 1a: start a task-scoped capture + write BODY (no Enter yet).
        // read_pane is a result handoff, so retaining older peer tasks only
        // creates ambiguity and makes truncation metadata stale.
        {
            let id2 = id.to_string();
            let body2 = body.clone();
            let session = self.session.clone();
            tokio::task::spawn_blocking(move || -> Result<(), String> {
                let (writer_arc, buffer_arc) = {
                    let guard = session
                        .lock()
                        .map_err(|_| "session map poisoned".to_string())?;
                    let sess = guard
                        .get(&id2)
                        .ok_or_else(|| format!("pane not found: {}", id2))?;
                    (sess.writer_lock.clone(), sess.output_buffer.clone())
                };

                {
                    let mut ring = buffer_arc
                        .lock()
                        .map_err(|_| "output buffer poisoned".to_string())?;
                    ring.clear();
                }

                {
                    let mut writer = writer_arc
                        .lock()
                        .map_err(|_| "pane writer poisoned".to_string())?;
                    if !body2.is_empty() {
                        writer
                            .write_all(body2.as_bytes())
                            .map_err(|e| format!("pty write failed: {}", e))?;
                        writer
                            .flush()
                            .map_err(|e| format!("pty flush failed: {}", e))?;
                    }
                }

                Ok(())
            })
            .await
            .map_err(|e| format!("blocking task join failed: {}", e))??
        };

        // Phase 1b: pause so the target REPL processes the body
        // characters into its input field, THEN send the Enter as a
        // separate keystroke. Observed live on 2026-04-23 (against the
        // pre-sunset Gemini CLI): a flat 120ms was enough for short
        // < 100-char prompts but failed for a 300-char multi-line
        // Claude→peer dispatch — the peer's Ink reconciler was still
        // painting the last lines when `\r` arrived, so the CR got
        // absorbed into the text instead of submitting. Body-size
        // proportional delay fixes the whole range: 250ms base
        // (covers the fixed render cost) + 1ms per body character
        // (scales with paint work), clamped to 1.5s so we never sit
        // on a huge paste for ages. Still fires
        // and mark the pane busy for list_panes/read_pane.
        if should_submit {
            let body_len = body.chars().count() as u64;
            let delay_ms = (250 + body_len).clamp(250, 1500);
            tokio::time::sleep(Duration::from_millis(delay_ms)).await;
            let id3 = id.to_string();
            let session = self.session.clone();
            tokio::task::spawn_blocking(move || -> Result<(), String> {
                let (writer_arc, activity_arc) = {
                    let guard = session
                        .lock()
                        .map_err(|_| "session map poisoned".to_string())?;
                    let sess = guard
                        .get(&id3)
                        .ok_or_else(|| format!("pane not found: {}", id3))?;
                    (sess.writer_lock.clone(), sess.activity.clone())
                };
                {
                    let mut writer = writer_arc
                        .lock()
                        .map_err(|_| "pane writer poisoned".to_string())?;
                    writer
                        .write_all(b"\r")
                        .map_err(|e| format!("pty write failed: {}", e))?;
                    writer
                        .flush()
                        .map_err(|e| format!("pty flush failed: {}", e))?;
                }
                if let Ok(mut act) = activity_arc.lock() {
                    act.mark_working();
                }
                Ok(())
            })
            .await
            .map_err(|e| format!("blocking task join failed: {}", e))??;

            // Phase 1d: verify the CR actually submitted. This is the
            // single most critical correctness gate of the dispatch flow:
            // if the body delivered but the CR was absorbed (Ink/React
            // reconciler still painting when \r arrived, bracketed-paste
            // mode swallowing the trailing newline, etc.), the target
            // pane sits silently with the message stuck in its input
            // box and the entire orchestration hangs — exact symptom
            // user reported as "成语接龙 pane 2 不动".
            //
            // Detection: after a 1.5s grace, if no PTY output has
            // arrived since we wrote the CR, it almost certainly never
            // reached the REPL's reducer.
            // (Real LLM CLIs paint *something* — Thinking…/spinner/
            // input-box clear — within 1.5s of a successful submit.)
            //
            // Recovery: send a single retry CR. Cost of a false positive
            // (CR did land but the LLM was unusually slow) is one empty
            // Enter at the prompt, which all three target CLIs (Claude
            // Code / Codex) treat as a no-op. We deliberately
            // do not retry more than once: two retries means the agent
            // is genuinely stuck (network, OOM, model crash) and adding
            // more CRs won't help — let the wait loop time out and
            // surface that to the caller.
            let cr_send_time = Instant::now();
            tokio::time::sleep(Duration::from_millis(1500)).await;

            let cr_lost = {
                let id_check = id.to_string();
                let session_check = self.session.clone();
                tokio::task::spawn_blocking(move || -> bool {
                    let Ok(guard) = session_check.lock() else { return false; };
                    let Some(sess) = guard.get(&id_check) else { return false; };
                    let Ok(act) = sess.activity.lock() else { return false; };
                    act.last_output_at < cr_send_time
                })
                .await
                .unwrap_or(false)
            };

            if cr_lost {
                log::warn!(
                    "coffee-cli mcp dispatch: CR appears absorbed by {}, retrying once",
                    id
                );
                let id_retry = id.to_string();
                let session_retry = self.session.clone();
                let _ = tokio::task::spawn_blocking(move || -> Result<(), String> {
                    let writer_arc = {
                        let guard = session_retry
                            .lock()
                            .map_err(|_| "session map poisoned".to_string())?;
                        let sess = guard
                            .get(&id_retry)
                            .ok_or_else(|| format!("pane not found: {}", id_retry))?;
                        sess.writer_lock.clone()
                    };
                    let mut writer = writer_arc
                        .lock()
                        .map_err(|_| "pane writer poisoned".to_string())?;
                    writer
                        .write_all(b"\r")
                        .map_err(|e| format!("pty write failed: {}", e))?;
                    writer
                        .flush()
                        .map_err(|e| format!("pty flush failed: {}", e))?;
                    Ok(())
                })
                .await;
            }
        }

        if !wait {
            return Ok(DispatchResult {
                bytes_written,
                waited: false,
                timed_out: false,
                captured_output: None,
            });
        }

        // Phase 2: poll for idle. Two independent paths — either one means
        // the pane is done.
        //
        //   A) marker-based: ticker flipped status back to "wait_input"
        //      (shell prompt marker seen) AND output either arrived since
        //      send or has been quiet 2s+. Primary path when terminal.rs's
        //      prompt_markers match the target CLI's actual prompt.
        //
        //   B) settle-based: we saw output come in after send time AND then
        //      it has been quiet for 2.5s+. Independent of prompt markers.
        //      Load-bearing when a CLI's prompt isn't in the marker list
        //      (observed live on the pre-sunset Gemini CLI: its "* "
        //      input prompt didn't match the `✦` preset, so path A never
        //      fired and the controller pane would hang forever waiting
        //      on a response that already arrived).
        //
        // The settle_silence threshold is slightly longer than long_silence
        // so we don't declare idle in the gap BETWEEN our write hitting
        // the PTY and Gemini starting to render its answer.
        let send_time = Instant::now();
        let deadline = send_time + Duration::from_secs(timeout_sec);

        // Initial grace so the target CLI has a chance to start producing
        // output before we begin checking for a settled response.
        tokio::time::sleep(Duration::from_millis(400)).await;

        let mut timed_out = true;
        loop {
            if Instant::now() > deadline {
                break;
            }

            let idle = {
                let id2 = id.to_string();
                let session = self.session.clone();
                tokio::task::spawn_blocking(move || -> Result<bool, String> {
                    let guard = session
                        .lock()
                        .map_err(|_| "session map poisoned".to_string())?;
                    let sess = guard
                        .get(&id2)
                        .ok_or_else(|| format!("pane not found: {}", id2))?;
                    let mut act = sess
                        .activity
                        .lock()
                        .map_err(|_| "activity poisoned".to_string())?;
                    let at_prompt = act.is_done();
                    let now = Instant::now();
                    let produced_since_send = act.last_output_at >= send_time;
                    let silence = now.duration_since(act.last_output_at);
                    // Observed 2026-04-23 against the pre-sunset Gemini
                    // CLI: LLM-driven CLIs (Claude/Codex/Gemini) paused
                    // 3-8s between planning phases while the model
                    // thinks; the old 2s/2.5s thresholds treated these
                    // as "task done" and returned Claude a half-finished
                    // result. Bump to 8s/15s — the longest observed
                    // mid-task think gap was ~10s, so 15s for
                    // settle_silence is conservative without stretching
                    // too long. Real idle after a genuinely completed
                    // task (prompt returns, ✨ summary renders) hits
                    // marker_path in <2s and early-returns regardless,
                    // so this doesn't slow the happy path.
                    let long_silence = silence > Duration::from_millis(8000);
                    let settle_silence = silence > Duration::from_millis(15000);

                    let marker_path = at_prompt && (produced_since_send || long_silence);
                    let settle_path = produced_since_send && settle_silence;

                    let idle = marker_path || settle_path;
                    if idle {
                        act.mark_done();
                    }
                    Ok(idle)
                })
                .await
                .map_err(|e| format!("blocking task join failed: {}", e))??
            };

            if idle {
                timed_out = false;
                break;
            }

            tokio::time::sleep(Duration::from_millis(500)).await;
        }

        // Phase 3: snapshot the task-scoped buffer after idle.
        let buf_after = {
            let id2 = id.to_string();
            let session = self.session.clone();
            tokio::task::spawn_blocking(move || -> Result<String, String> {
                let guard = session
                    .lock()
                    .map_err(|_| "session map poisoned".to_string())?;
                let sess = guard
                    .get(&id2)
                    .ok_or_else(|| format!("pane not found: {}", id2))?;
                let ring = sess
                    .output_buffer
                    .lock()
                    .map_err(|_| "output buffer poisoned".to_string())?;
                Ok(ring.joined())
            })
            .await
            .map_err(|e| format!("blocking task join failed: {}", e))??
        };

        let stripped = self.ansi_re.replace_all(&buf_after, "").to_string();

        // Keep the dormant legacy wait-mode path safe if it is ever exposed
        // again: carriage-return redraws require a byte cap, not just lines.
        let trimmed = bounded_output_tail(&stripped, 200, READ_PANE_MAX_BYTES).text;

        Ok(DispatchResult {
            bytes_written,
            waited: true,
            timed_out,
            captured_output: Some(trimmed),
        })
    }

    /// Return the last `last_n` lines of the pane's ANSI-stripped output,
    /// plus an `is_idle` flag derived from native CLI status when available.
    async fn read(&self, id: &str, last_n: usize) -> Result<(BoundedOutput, bool), String> {
        let id = id.to_string();
        let session = self.session.clone();
        let ansi_re = self.ansi_re.clone();

        tokio::task::spawn_blocking(move || -> Result<(BoundedOutput, bool), String> {
            let guard = session
                .lock()
                .map_err(|_| "session map poisoned".to_string())?;
            let sess = guard
                .get(&id)
                .ok_or_else(|| format!("pane not found: {}", id))?;

            // Pull the raw output tail under its own lock, dropped immediately.
            let (joined, buffer_truncated) = {
                let ring = sess
                    .output_buffer
                    .lock()
                    .map_err(|_| "output buffer poisoned".to_string())?;
                (ring.joined(), ring.prefix_truncated())
            };

            // Native OSC-title status is authoritative. Older/unsupported
            // CLIs fall back to the legacy activity state; its silence escape
            // hatch is intentionally limited to explicit terminal inspection,
            // never coordinated job completion.
            let is_idle = sess
                .activity
                .lock()
                .map(|activity| match activity.native_status.as_deref() {
                    Some("idle") => true,
                    Some(_) => false,
                    None => {
                        activity.is_done()
                            || (activity.last_status == "working"
                                && activity.last_output_at.elapsed() > Duration::from_secs(15))
                    }
                })
                .unwrap_or(false);

            drop(guard);

            // Strip ANSI, then enforce both line and byte limits. Terminal
            // spinners frequently repaint with bare carriage returns, which
            // can produce one enormous logical line and bypass a line-only cap.
            let stripped = ansi_re.replace_all(&joined, "").to_string();
            let mut output = bounded_output_tail(&stripped, last_n, READ_PANE_MAX_BYTES);
            output.truncated |= buffer_truncated;
            Ok((output, is_idle))
        })
        .await
        .map_err(|e| format!("blocking task join failed: {}", e))?
    }

    async fn mark_terminal_done(&self, id: &str) {
        let id = id.to_string();
        let session = self.session.clone();
        let _ = tokio::task::spawn_blocking(move || {
            let Ok(guard) = session.lock() else {
                return;
            };
            let Some(sess) = guard.get(&id) else {
                return;
            };
            if let Ok(mut activity) = sess.activity.lock() {
                activity.mark_done();
            };
        })
        .await;
    }

    async fn ensure_dispatch_ready(&self, id: &str) -> Result<(), String> {
        let id = id.to_string();
        let session = self.session.clone();
        tokio::task::spawn_blocking(move || -> Result<(), String> {
            let guard = session
                .lock()
                .map_err(|_| "session map poisoned".to_string())?;
            let sess = guard
                .get(&id)
                .ok_or_else(|| format!("pane not found: {id}"))?;
            let activity = sess
                .activity
                .lock()
                .map_err(|_| "activity poisoned".to_string())?;
            match activity.native_status.as_deref() {
                Some("idle") => Ok(()),
                Some("working") | Some("wait_input") => {
                    Err("target pane's native CLI status is busy".to_string())
                }
                Some(status) => Err(format!(
                    "target pane reported unknown native status: {status}"
                )),
                None if activity.last_status == "working"
                    && activity.last_output_at.elapsed() <= Duration::from_secs(15) =>
                {
                    Err("target pane recently received a submitted prompt".to_string())
                }
                None => Ok(()),
            }
        })
        .await
        .map_err(|error| format!("native status check failed: {error}"))?
    }

    fn emit_task_event(&self, record: &PaneTaskRecord) -> Result<(), String> {
        self.app
            .emit(
                "multi-agent-task-complete",
                PaneTaskEvent {
                    job_id: record.job_id.clone(),
                    source_id: record.target_id.clone(),
                    target_id: record.source_id.clone(),
                    status: record.status.clone(),
                    error: record.error.clone(),
                },
            )
            .map_err(|error| format!("completion event emit failed: {error}"))
    }
}

/// Outcome of `PaneStore::dispatch` — conveyed back to the MCP caller so
/// legacy wait-mode users can distinguish completion from timeout.
#[derive(Debug)]
pub struct DispatchResult {
    pub bytes_written: usize,
    /// Whether the caller requested wait=true (vs fire-and-forget).
    pub waited: bool,
    /// True only when waited=true AND the deadline hit without the pane
    /// flipping back to wait_input. `captured_output` still holds whatever
    /// arrived in that window.
    pub timed_out: bool,
    /// ANSI-stripped output that arrived between the write and idle.
    /// Some(..) iff waited=true; None iff fire-and-forget.
    pub captured_output: Option<String>,
}

fn status_to_pane_state(status: &str) -> PaneState {
    match status {
        "wait_input" => PaneState::Idle,
        "working" => PaneState::Busy,
        "" | "unknown" => PaneState::Empty,
        _ => PaneState::Idle,
    }
}

fn epoch_seconds() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

// ---------- MCP tool arguments ----------

#[derive(Debug, Deserialize, JsonSchema)]
#[schemars(crate = "rmcp::schemars")]
pub struct SendToPaneArgs {
    /// Target pane. **Use the short form** (`pane-1`, `pane-2`, …) —
    /// it's what the `pane` field of `list_panes()` returns and keeps
    /// the rendered tool call short enough to display cleanly inside
    /// a 25%-width grid pane. The full id (`<tab_id>::pane-2`) and a
    /// bare digit (`2`) are also accepted for back-compat. Must not
    /// resolve to the caller's own pane.
    pub id: String,
    /// Text to inject into the target pane's stdin.
    pub text: String,
    /// If true (default), auto-append `\r` unless `text` already ends with
    /// a newline. Set false when you need to type without submitting (e.g.
    /// inserting template text for the user to finish editing).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub submit: Option<bool>,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[schemars(crate = "rmcp::schemars")]
pub struct ReadPaneArgs {
    /// Target pane. Same conventions as `send_to_pane`: short form
    /// (`pane-1`) preferred, full id and bare digit also accepted.
    pub id: String,
    /// Max recent lines to return. Default 80, max 200. Results also have a
    /// strict 32 KiB UTF-8-safe limit so terminal redraws cannot flood context.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_n_lines: Option<usize>,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[schemars(crate = "rmcp::schemars")]
pub struct CompleteTaskArgs {
    /// Exact job id included in the `[Coffee task ...]` dispatch header.
    pub job_id: String,
    /// Concise, self-contained result for the dispatching pane. Keep large
    /// reports in `COFFEE_AGENT_RESULTS_DIR` and provide `result_path`.
    pub summary: String,
    /// Optional absolute path to a complete result artifact. The file must
    /// already exist and be non-empty before completion is accepted.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub result_path: Option<String>,
}


/// Extract the tab id portion of a pane id (`${tab_id}::pane-${idx}`).
/// Used to scope `list_panes` and `send_to_pane` to the caller's own
/// multi-agent Tab so simultaneous tabs don't see / dispatch to each
/// other. If the input doesn't match the expected format, returns the
/// whole string — that's a safe fallback (single-tab in legacy mode).
fn tab_prefix(pane_id: &str) -> &str {
    match pane_id.find("::pane-") {
        Some(idx) => &pane_id[..idx],
        None => pane_id,
    }
}

/// Extract the short pane label (e.g. `pane-1`) from a full pane id
/// like `tab-fb3f2173-...::pane-1`. Returned as a String the LLM can
/// quote inline in tool calls without dragging the 44-char tab UUID
/// along — keeps `send_to_pane(...)` arg lists short enough to render
/// cleanly inside a 25%-width grid pane. Falls back to the whole id
/// if no `::pane-` separator is found (legacy / split-pane sessions).
fn pane_short(pane_id: &str) -> String {
    match pane_id.find("::pane-") {
        Some(idx) => pane_id[idx + "::".len()..].to_string(),
        None => pane_id.to_string(),
    }
}

/// Resolve the `id` argument of `send_to_pane` / `read_pane` against
/// the caller's tab context. Accepts:
///   - full id: `tab-X::pane-N`             → returned unchanged
///   - short label: `pane-N`                → expanded with `self_tab`
///   - bare digit / number: `1` / `2` / …   → expanded as `<self_tab>::pane-N`
///
/// Short forms are the recommended way for an LLM to call these tools
/// in a 4-pane grid because the Claude/Codex TUIs render long
/// tool-call arg lists badly when wrapped in narrow panes (the long
/// UUID + a multi-byte text payload trips emoji-width-aware folding).
/// Full ids stay accepted forever — pre-v1.5.1 callers (and the LLMs
/// they teach) keep working.
fn resolve_pane_id(arg_id: &str, self_pane_id: Option<&str>) -> String {
    if arg_id.contains("::pane-") {
        return arg_id.to_string();
    }
    let Some(self_id) = self_pane_id else {
        return arg_id.to_string();
    };
    let tab = tab_prefix(self_id);
    if let Some(stripped) = arg_id.strip_prefix("pane-") {
        if stripped.chars().all(|c| c.is_ascii_digit()) {
            return format!("{tab}::pane-{stripped}");
        }
    }
    if arg_id.chars().all(|c| c.is_ascii_digit()) && !arg_id.is_empty() {
        return format!("{tab}::pane-{arg_id}");
    }
    arg_id.to_string()
}

fn same_tab(left: &str, right: &str) -> bool {
    tab_prefix(left) == tab_prefix(right)
}

fn validate_completion_payload(
    summary: String,
    result_path: Option<String>,
) -> Result<(String, Option<String>), String> {
    let summary = summary.trim().to_string();
    if summary.is_empty() {
        return Err("summary must not be empty".to_string());
    }
    if summary.len() > COMPLETION_SUMMARY_MAX_BYTES {
        return Err(format!(
            "summary exceeds {} bytes; write the full result to COFFEE_AGENT_RESULTS_DIR and provide result_path",
            COMPLETION_SUMMARY_MAX_BYTES
        ));
    }

    let result_path = match result_path {
        Some(path) => {
            let path = path.trim().to_string();
            if path.is_empty() || path.len() > COMPLETION_PATH_MAX_BYTES {
                return Err("result_path is empty or too long".to_string());
            }
            if path.contains(['\r', '\n']) {
                return Err("result_path must be a single line".to_string());
            }
            let path_buf = std::path::PathBuf::from(&path);
            if !path_buf.is_absolute() {
                return Err("result_path must be absolute".to_string());
            }
            let metadata = std::fs::metadata(&path_buf)
                .map_err(|error| format!("result_path is not readable: {error}"))?;
            if !metadata.is_file() || metadata.len() == 0 {
                return Err("result_path must reference a non-empty file".to_string());
            }
            Some(path)
        }
        None => None,
    };

    Ok((summary, result_path))
}

#[derive(Debug, PartialEq, Eq)]
struct BoundedOutput {
    text: String,
    truncated: bool,
    buffered_bytes: usize,
}

fn bounded_output_tail(input: &str, max_lines: usize, max_bytes: usize) -> BoundedOutput {
    let buffered_bytes = input.len();
    if max_bytes == 0 {
        return BoundedOutput {
            text: String::new(),
            truncated: !input.is_empty(),
            buffered_bytes,
        };
    }

    let mut lines: Vec<&str> = input.lines().collect();
    let lines_truncated = lines.len() > max_lines;
    if lines.len() > max_lines {
        lines = lines.split_off(lines.len() - max_lines);
    }
    let by_lines = lines.join("\n");
    if by_lines.len() <= max_bytes {
        return BoundedOutput {
            text: by_lines,
            truncated: lines_truncated,
            buffered_bytes,
        };
    }

    const MARKER: &str = "[... earlier terminal output truncated ...]\n";
    if max_bytes <= MARKER.len() {
        return BoundedOutput {
            text: MARKER[..max_bytes].to_string(),
            truncated: true,
            buffered_bytes,
        };
    }
    let tail_budget = max_bytes.saturating_sub(MARKER.len());
    let mut start = by_lines.len().saturating_sub(tail_budget);
    while start < by_lines.len() && !by_lines.is_char_boundary(start) {
        start += 1;
    }
    BoundedOutput {
        text: format!("{MARKER}{}", &by_lines[start..]),
        truncated: true,
        buffered_bytes,
    }
}

#[cfg(test)]
mod tests {
    use super::{
        PaneTaskStatus, TaskCoordinator, bounded_output_tail, resolve_pane_id,
        same_tab, tab_prefix, validate_completion_payload,
    };

    #[test]
    fn tab_prefix_extracts_tab_portion() {
        assert_eq!(tab_prefix("tab-abc::pane-1"), "tab-abc");
        assert_eq!(tab_prefix("tab-abc::pane-4"), "tab-abc");
        assert_eq!(
            tab_prefix("tab-uuid-with-dashes::pane-2"),
            "tab-uuid-with-dashes"
        );
    }

    #[test]
    fn tab_prefix_falls_back_for_unmatched_format() {
        // Legacy / split-pane / shell sessions don't use the
        // ::pane- format. Returning the whole id is the safe
        // default — these never collide cross-Tab anyway.
        assert_eq!(tab_prefix("legacy-session-id"), "legacy-session-id");
        assert_eq!(tab_prefix("tab-X::split-1"), "tab-X::split-1");
    }

    #[test]
    fn tab_prefix_distinguishes_concurrent_tabs() {
        // The whole point of tab_prefix: panes in tab-A and tab-B
        // must produce DIFFERENT prefixes so list_panes can filter
        // them apart even when both Tabs run 4 Claude panes.
        let a1 = tab_prefix("tab-A::pane-1");
        let b1 = tab_prefix("tab-B::pane-1");
        assert_ne!(a1, b1, "concurrent multi-agent tabs must be isolatable");
    }

    #[test]
    fn short_pane_two_resolves_inside_callers_tab() {
        let caller = "tab-A::pane-1";
        assert_eq!(resolve_pane_id("pane-2", Some(caller)), "tab-A::pane-2");
        assert_eq!(resolve_pane_id("2", Some(caller)), "tab-A::pane-2");
    }

    #[test]
    fn same_tab_accepts_siblings_and_rejects_other_tabs() {
        assert!(same_tab("tab-A::pane-1", "tab-A::pane-2"));
        assert!(!same_tab("tab-A::pane-1", "tab-B::pane-2"));
    }

    #[test]
    fn output_tail_keeps_only_requested_lines() {
        let output = bounded_output_tail("one\ntwo\nthree", 2, 1024);
        assert_eq!(output.text, "two\nthree");
        assert!(output.truncated);
        assert_eq!(output.buffered_bytes, 13);
    }

    #[test]
    fn output_tail_reports_complete_small_output() {
        let output = bounded_output_tail("one\ntwo", 80, 1024);
        assert_eq!(output.text, "one\ntwo");
        assert!(!output.truncated);
        assert_eq!(output.buffered_bytes, 7);
    }

    #[test]
    fn output_tail_caps_single_long_line_without_splitting_utf8() {
        let input = format!("{}咖啡", "x".repeat(4096));
        let output = bounded_output_tail(&input, 200, 128);
        assert!(output.text.len() <= 128);
        assert!(output.text.starts_with("[... earlier terminal output truncated ...]"));
        assert!(output.text.ends_with("咖啡"));
        assert!(output.truncated);
        assert_eq!(output.buffered_bytes, input.len());
    }

    #[test]
    fn task_lifecycle_is_structured_and_exactly_once() {
        let tasks = TaskCoordinator::new();
        let source = "tab-A::pane-1";
        let target = "tab-A::pane-3";
        let job_id = tasks.start(source, target).unwrap();

        let running = tasks.latest_for_route(source, target).unwrap();
        assert_eq!(running.status, PaneTaskStatus::Running);
        assert!(tasks.start(source, target).unwrap_err().contains("busy"));

        let (completed, is_new) = tasks
            .complete(target, &job_id, "review complete".to_string(), None)
            .unwrap();
        assert!(is_new);
        assert_eq!(completed.status, PaneTaskStatus::Completed);

        let (_, duplicate_is_new) = tasks
            .complete(target, &job_id, "ignored duplicate".to_string(), None)
            .unwrap();
        assert!(!duplicate_is_new);
        let stored = tasks.latest_for_route(source, target).unwrap();
        assert_eq!(stored.summary.as_deref(), Some("review complete"));
    }

    #[test]
    fn concurrent_dispatch_reserves_a_target_exactly_once() {
        let tasks = std::sync::Arc::new(TaskCoordinator::new());
        let barrier = std::sync::Arc::new(std::sync::Barrier::new(8));
        let handles: Vec<_> = (0..8)
            .map(|index| {
                let tasks = tasks.clone();
                let barrier = barrier.clone();
                std::thread::spawn(move || {
                    barrier.wait();
                    tasks.start(
                        &format!("tab-A::pane-{}", index + 1),
                        "tab-A::pane-9",
                    )
                })
            })
            .collect();

        let outcomes: Vec<_> = handles
            .into_iter()
            .map(|handle| handle.join().unwrap())
            .collect();
        assert_eq!(outcomes.iter().filter(|outcome| outcome.is_ok()).count(), 1);
        assert_eq!(outcomes.iter().filter(|outcome| outcome.is_err()).count(), 7);
    }

    #[test]
    fn stale_or_wrong_pane_cannot_complete_a_job() {
        let tasks = TaskCoordinator::new();
        let source = "tab-A::pane-1";
        let target = "tab-A::pane-2";
        let first = tasks.start(source, target).unwrap();
        tasks
            .complete(target, &first, "first".to_string(), None)
            .unwrap();
        let second = tasks.start(source, target).unwrap();

        assert!(tasks
            .complete(target, &first, "late".to_string(), None)
            .unwrap_err()
            .contains("stale"));
        assert!(tasks
            .complete("tab-A::pane-4", &second, "wrong".to_string(), None)
            .unwrap_err()
            .contains("stale"));
        assert_eq!(
            tasks.latest_for_route(source, target).unwrap().job_id,
            second
        );
    }

    #[test]
    fn task_results_are_visible_only_to_the_dispatching_route() {
        let tasks = TaskCoordinator::new();
        let source = "tab-A::pane-1";
        let target = "tab-A::pane-2";
        let job_id = tasks.start(source, target).unwrap();
        tasks
            .complete(target, &job_id, "private result".to_string(), None)
            .unwrap();

        assert!(tasks.latest_for_route(source, target).is_some());
        assert!(tasks
            .latest_for_route("tab-A::pane-3", target)
            .is_none());
        assert!(tasks
            .latest_for_route("tab-B::pane-1", target)
            .is_none());
    }

    #[test]
    fn target_failure_is_stored_and_routes_back_to_dispatcher() {
        let tasks = TaskCoordinator::new();
        let source = "tab-A::pane-1";
        let target = "tab-A::pane-3";
        let job_id = tasks.start(source, target).unwrap();

        let event = tasks
            .fail_target(target, "process exited".to_string())
            .unwrap();
        assert_eq!(event.job_id, job_id);
        assert_eq!(event.source_id, target);
        assert_eq!(event.target_id, source);
        assert_eq!(event.status, PaneTaskStatus::Failed);
        assert!(tasks.fail_target(target, "duplicate".to_string()).is_none());
        assert_eq!(
            tasks.latest_for_route(source, target).unwrap().status,
            PaneTaskStatus::Failed
        );
    }

    #[test]
    fn coordinator_prunes_old_terminal_jobs() {
        let tasks = TaskCoordinator::new();
        let source = "tab-A::pane-1";
        let target = "tab-A::pane-2";
        for index in 0..300 {
            let job_id = tasks.start(source, target).unwrap();
            tasks
                .complete(target, &job_id, format!("result {index}"), None)
                .unwrap();
        }
        assert!(tasks.state.lock().unwrap().records.len() <= 256);
    }

    #[test]
    fn completion_payload_requires_real_non_empty_artifact() {
        let dir = std::env::temp_dir().join(format!(
            "coffee-cli-completion-test-{}",
            std::process::id()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("result.md");
        std::fs::write(&path, "complete result").unwrap();

        let (_, stored_path) = validate_completion_payload(
            "summary".to_string(),
            Some(path.to_string_lossy().into_owned()),
        )
        .unwrap();
        assert_eq!(stored_path.as_deref(), path.to_str());
        assert!(validate_completion_payload("  ".to_string(), None).is_err());
        assert!(validate_completion_payload(
            "summary".to_string(),
            Some(dir.join("missing.md").to_string_lossy().into_owned())
        )
        .is_err());

        let _ = std::fs::remove_dir_all(dir);
    }
}

// ---------- MCP server handler ----------

#[derive(Clone)]
pub struct CoffeeMcp {
    tool_router: ToolRouter<CoffeeMcp>,
    panes: Arc<PaneStore>,
    /// The pane this MCP server instance is dedicated to — i.e. "the
    /// caller's identity, baked in at spawn time". Each multi-agent
    /// pane spawns its own MCP server bound to its own port, with
    /// its own `self_pane_id` set. `None` means the server is
    /// anonymous (legacy / non-multi-agent mode); in that case
    /// `whoami` returns an error and `list_panes` doesn't mark
    /// `is_self`.
    self_pane_id: Option<String>,
}

#[tool_router]
impl CoffeeMcp {
    pub fn new(panes: Arc<PaneStore>, self_pane_id: Option<String>) -> Self {
        Self {
            tool_router: Self::tool_router(),
            panes,
            self_pane_id,
        }
    }

    #[tool(
        description = "Return the caller's coordinated Coffee-CLI pane identity \
as `{ pane_id: \"pane-N\" }`. Coffee uses this bound identity to authorize \
dispatch, completion, and reads."
    )]
    async fn whoami(&self) -> Result<CallToolResult, McpError> {
        match &self.self_pane_id {
            Some(id) => {
                // Return the short label (e.g. `pane-1`) so the LLM
                // sees a value short enough to drop straight into
                // tool calls without bloating the rendered arg list.
                let payload = serde_json::json!({ "pane_id": pane_short(id) });
                Ok(CallToolResult::success(vec![Content::text(
                    serde_json::to_string_pretty(&payload).unwrap_or_default(),
                )]))
            }
            None => Ok(CallToolResult::success(vec![Content::text(
                serde_json::json!({ "error": "this MCP endpoint is not bound to a pane" })
                    .to_string(),
            )])),
        }
    }

    #[tool(
        description = "List panes in the caller's own multi-agent tab. Cross-tab \
panes are filtered out. Each row has id, title, cli, state (empty/idle/busy/\
terminated), and is_self. Use this before calling send_to_pane."
    )]
    async fn list_panes(&self) -> Result<CallToolResult, McpError> {
        let mut panes = self.panes.list().await;
        if let Some(self_id) = &self.self_pane_id {
            // Tab-scope filter: only show panes whose tab matches the
            // caller's. This is what makes simultaneous multi-agent
            // tabs safe — a pane in Tab A can't accidentally dispatch
            // to a pane in Tab B because it never sees Tab B's panes
            // in the first place. We filter on the internal `full_id`
            // (the long `tab-<uuid>::pane-N` form), since the public
            // `id` field is now a short tab-relative `pane-N`.
            let self_tab = tab_prefix(self_id);
            panes.retain(|p| tab_prefix(&p.full_id) == self_tab);
            for p in &mut panes {
                if &p.full_id == self_id {
                    p.is_self = Some(true);
                }
            }
        }
        let payload = serde_json::to_string_pretty(&panes).unwrap_or_default();
        Ok(CallToolResult::success(vec![Content::text(payload)]))
    }

    #[tool(
        description = "Dispatch a task to a peer pane. This is fire-and-forget — \
there is no waiting mode. You may dispatch a small batch of independent tasks \
to different idle panes; after the final call, end your turn and sit at idle \
until a target calls \
`complete_task`, which reactivates your LLM with a structured result. \
\
Coffee-CLI writes an identity-bound task header and completion contract together \
with the requested text, then appends a carriage return. Self-dispatch, \
cross-tab dispatch, and dispatch to a pane with a running job are rejected."
    )]
    async fn send_to_pane(
        &self,
        Parameters(args): Parameters<SendToPaneArgs>,
    ) -> Result<CallToolResult, McpError> {
        // Resolve the `id` arg: accept short forms (`pane-2`, `2`) and
        // expand them against the caller's tab. Keeps tool calls short
        // enough to render cleanly inside narrow grid panes.
        let target_id = resolve_pane_id(&args.id, self.self_pane_id.as_deref());

        // Reject self-dispatch up front — this MCP instance knows
        // exactly which pane it represents.
        if let Some(self_id) = &self.self_pane_id {
            if self_id == &target_id {
                return Ok(CallToolResult::success(vec![Content::text(
                    serde_json::json!({
                        "status": "failed",
                        "error": "cannot send_to_pane to self",
                        "self_pane_id": self_id,
                    })
                    .to_string(),
                )]));
            }
            // Cross-Tab guard: refuse to dispatch into a pane that
            // belongs to a different multi-agent Tab. Without this,
            // a 4-pane Tab A could accidentally pipe work into a
            // 4-pane Tab B because both tabs share the same global
            // SharedSession map. Mirrors the filtering done in
            // `list_panes`.
            let self_tab = tab_prefix(self_id);
            let target_tab = tab_prefix(&target_id);
            if !same_tab(self_id, &target_id) {
                return Ok(CallToolResult::success(vec![Content::text(
                    serde_json::json!({
                        "status": "failed",
                        "error": "target pane belongs to a different Tab; cross-Tab dispatch is not supported",
                        "self_tab": self_tab,
                        "target_tab": target_tab,
                    })
                    .to_string(),
                )]));
            }
        }
        let Some(source_id) = self.self_pane_id.as_deref() else {
            return Ok(CallToolResult::success(vec![Content::text(
                serde_json::json!({
                    "status": "failed",
                    "error": "send_to_pane requires an identity-bound pane endpoint"
                })
                .to_string(),
            )]));
        };

        if let Err(error) = self.panes.ensure_dispatch_ready(&target_id).await {
            return Ok(CallToolResult::success(vec![Content::text(
                serde_json::json!({
                    "status": "busy",
                    "pane_id": pane_short(&target_id),
                    "error": error,
                    "instruction": "Do not write to or poll this pane. Choose another idle pane or wait until the human-visible task finishes."
                })
                .to_string(),
            )]));
        }

        // Multi-agent dispatch is one-shot: reserve an identity-bound job
        // before touching the PTY. This makes concurrent/busy dispatches fail
        // atomically instead of being merged into a running agent turn.
        let job_id = match self.panes.tasks.start(source_id, &target_id) {
            Ok(job_id) => job_id,
            Err(error) => {
                return Ok(CallToolResult::success(vec![Content::text(
                    serde_json::json!({
                        "status": "busy",
                        "pane_id": pane_short(&target_id),
                        "error": error,
                        "instruction": "Do not write to or poll this pane. Choose another idle pane or wait for the existing task's completion notification."
                    })
                    .to_string(),
                )]));
            }
        };

        let wait = false;
        let submit = args.submit.unwrap_or(true);
        if !submit {
            self.panes.tasks.abort_start(&job_id);
            return Ok(CallToolResult::success(vec![Content::text(
                serde_json::json!({
                    "status": "failed",
                    "error": "submit=false is not supported for coordinated tasks"
                })
                .to_string(),
            )]));
        }
        let timeout_sec = 0u64;

        // Prefix the dispatched text with `[From <self_pane>]` so the
        // receiving CLI's LLM sees who sent the work — without this
        // the target gets a bare command and has to guess the source.
        // Use the short `pane-N` form (not the long full id) so the
        // resulting prefix doesn't blow up the receiver's terminal
        // width either.
        let dispatch_text = format!(
            "[Coffee task {job_id} from {}]\n{}\n\nCompletion contract: after the result is ready, call coffee-cli.complete_task with job_id \"{job_id}\", a concise self-contained summary, and an absolute result_path when the full result is stored in a file. Do not print a textual completion marker.",
            pane_short(source_id),
            args.text,
        );

        match self
            .panes
            .dispatch(&target_id, &dispatch_text, submit, wait, timeout_sec)
            .await
        {
            Ok(result) => {
                let status = if !result.waited {
                    "submitted"
                } else if result.timed_out {
                    "timeout"
                } else {
                    "completed"
                };
                let mut payload = serde_json::json!({
                    "status": status,
                    "pane_id": pane_short(&target_id),
                    "bytes_written": result.bytes_written,
                    "job_id": job_id,
                });
                if let Some(output) = result.captured_output {
                    payload["output"] = serde_json::json!(output);
                }
                Ok(CallToolResult::success(vec![Content::text(
                    serde_json::to_string_pretty(&payload).unwrap_or_default(),
                )]))
            }
            Err(e) => {
                self.panes.tasks.abort_start(&job_id);
                Ok(CallToolResult::success(vec![Content::text(
                    serde_json::json!({ "status": "failed", "error": e }).to_string(),
                )]))
            }
        }
    }

    #[tool(
        description = "Complete the coordinated task currently assigned to this pane. \
Call this exactly once, only after the result is ready. `job_id` must match the \
Coffee task header. The summary is stored before the dispatching pane is woken, \
so terminal output and repainting cannot forge or race completion. For long \
results, first write a non-empty file under COFFEE_AGENT_RESULTS_DIR and pass its \
absolute path as result_path. After success, end your turn."
    )]
    async fn complete_task(
        &self,
        Parameters(args): Parameters<CompleteTaskArgs>,
    ) -> Result<CallToolResult, McpError> {
        let Some(target_id) = self.self_pane_id.as_deref() else {
            return Ok(CallToolResult::success(vec![Content::text(
                serde_json::json!({
                    "status": "failed",
                    "error": "complete_task requires an identity-bound pane endpoint"
                })
                .to_string(),
            )]));
        };
        let (summary, result_path) = match validate_completion_payload(args.summary, args.result_path) {
            Ok(payload) => payload,
            Err(error) => {
                return Ok(CallToolResult::success(vec![Content::text(
                    serde_json::json!({ "status": "failed", "error": error }).to_string(),
                )]));
            }
        };

        match self
            .panes
            .tasks
            .complete(target_id, &args.job_id, summary, result_path)
        {
            Ok((record, is_new)) => {
                self.panes.mark_terminal_done(target_id).await;
                if is_new {
                    if let Err(error) = self.panes.emit_task_event(&record) {
                        return Ok(CallToolResult::success(vec![Content::text(
                            serde_json::json!({
                                "status": "completed_with_notification_error",
                                "job_id": record.job_id,
                                "error": error,
                                "instruction": "The result is stored. End your turn; the human can recover it with read_pane."
                            })
                            .to_string(),
                        )]));
                    }
                }
                Ok(CallToolResult::success(vec![Content::text(
                    serde_json::json!({
                        "status": if is_new { "completed" } else { "already_completed" },
                        "job_id": record.job_id,
                        "dispatching_pane": pane_short(&record.source_id),
                        "instruction": "Completion is recorded. End your turn now."
                    })
                    .to_string(),
                )]))
            }
            Err(error) => Ok(CallToolResult::success(vec![Content::text(
                serde_json::json!({ "status": "failed", "error": error }).to_string(),
            )])),
        }
    }

    #[tool(
        description = "Read the most recent output lines from another pane. \
Use after Coffee-CLI wakes you with a `Task complete` message. Coordinated jobs \
return the structured summary/result_path stored by complete_task; terminal output \
is used only as a fallback when the human explicitly asks you to inspect a pane. \
Never poll progress, sleep, wait, or repeatedly call read_pane after send_to_pane."
    )]
    async fn read_pane(
        &self,
        Parameters(args): Parameters<ReadPaneArgs>,
    ) -> Result<CallToolResult, McpError> {
        // Accept short forms (`pane-2`, `2`) — same convenience as
        // send_to_pane.
        let target_id = resolve_pane_id(&args.id, self.self_pane_id.as_deref());
        if let Some(self_id) = &self.self_pane_id {
            if !same_tab(self_id, &target_id) {
                return Ok(CallToolResult::success(vec![Content::text(
                    serde_json::json!({
                        "status": "failed",
                        "error": "target pane belongs to a different Tab; cross-Tab reads are not supported",
                        "self_tab": tab_prefix(self_id),
                        "target_tab": tab_prefix(&target_id),
                    })
                    .to_string(),
                )]));
            }
        }
        let last_n = args
            .last_n_lines
            .unwrap_or(READ_PANE_DEFAULT_LINES)
            .clamp(1, READ_PANE_MAX_LINES);

        if let Some(source_id) = self.self_pane_id.as_deref() {
            if let Some(record) = self.panes.tasks.latest_for_route(source_id, &target_id) {
                let payload = match record.status {
                    PaneTaskStatus::Running => serde_json::json!({
                        "status": "working",
                        "job_id": record.job_id,
                        "pane_id": pane_short(&target_id),
                        "output": "",
                        "is_idle": false,
                        "instruction": "End your turn now. Do not sleep, wait, or poll read_pane. Coffee-CLI will reactivate you when complete_task stores the result."
                    }),
                    PaneTaskStatus::Completed => {
                        let mut output = record.summary.clone().unwrap_or_default();
                        if let Some(path) = &record.result_path {
                            output.push_str("\n\nFull result: ");
                            output.push_str(path);
                        }
                        serde_json::json!({
                            "status": "completed",
                            "job_id": record.job_id,
                            "pane_id": pane_short(&target_id),
                            "summary": record.summary,
                            "result_path": record.result_path,
                            "output": output,
                            "is_idle": true,
                            "truncated": false,
                        })
                    }
                    PaneTaskStatus::Failed => serde_json::json!({
                        "status": record.status,
                        "job_id": record.job_id,
                        "pane_id": pane_short(&target_id),
                        "error": record.error,
                        "output": "",
                        "is_idle": true,
                    }),
                };
                return Ok(CallToolResult::success(vec![Content::text(
                    serde_json::to_string_pretty(&payload).unwrap_or_default(),
                )]));
            }
        }

        match self.panes.read(&target_id, last_n).await {
            Ok((output, is_idle)) => {
                let payload = if is_idle {
                    let returned_bytes = output.text.len();
                    let mut payload = serde_json::json!({
                        "status": "idle",
                        "output": output.text,
                        "is_idle": true,
                        "truncated": output.truncated,
                        "buffered_bytes": output.buffered_bytes,
                        "returned_bytes": returned_bytes,
                    });
                    if output.truncated {
                        payload["instruction"] = serde_json::json!(
                            "This terminal view is incomplete. Use the `Full result: <absolute-path>` shown near the end to read the peer's complete artifact. If no path is present, ask the peer to save and report the complete result."
                        );
                    }
                    payload
                } else {
                    // A busy pane can contain thousands of spinner redraws.
                    // Returning none of them is also a runtime guard against
                    // models that ignore the fire-and-forget protocol.
                    serde_json::json!({
                        "status": "working",
                        "output": "",
                        "is_idle": false,
                        "instruction": "End your turn now. Do not sleep, wait, or poll read_pane."
                    })
                };
                Ok(CallToolResult::success(vec![Content::text(
                    serde_json::to_string_pretty(&payload).unwrap_or_default(),
                )]))
            }
            Err(e) => Ok(CallToolResult::success(vec![Content::text(
                serde_json::json!({ "status": "failed", "error": e }).to_string(),
            )])),
        }
    }

}

#[tool_handler]
impl ServerHandler for CoffeeMcp {
    fn get_info(&self) -> ServerInfo {
        ServerInfo {
            protocol_version: ProtocolVersion::V_2024_11_05,
            capabilities: ServerCapabilities::builder().enable_tools().build(),
            server_info: Implementation::from_build_env(),
            instructions: Some(
                "Coffee-CLI multi-agent MCP server. \
Tools: whoami, list_panes, send_to_pane, complete_task, read_pane. \
Use these to coordinate ACROSS different CLIs (Claude/Codex/OpenCode). \
You may send a small batch to different idle panes; after the final send_to_pane, \
end the turn immediately. Never poll, sleep, or wait for a peer; \
the structured completion notification will reactivate the caller. A receiving pane must call \
complete_task exactly once after its result is ready. Read peer output only after notification. \
For intra-CLI parallelism, prefer your native subagent SDK (Agent Teams / app-server / TaskTool). \
The caller's system prompt contains the full coordination protocol."
                    .to_string(),
            ),
        }
    }
}

// ---------- Entry point: spawn HTTP server on a dynamic port ----------

/// A per-pane endpoint kept in memory for the lifetime of its terminal.
#[derive(Clone, Debug)]
pub struct McpEndpoint {
    pub url: String,
    pub(crate) abort_handle: Option<tokio::task::AbortHandle>,
}

impl McpEndpoint {
    pub fn shutdown(&self) {
        if let Some(handle) = &self.abort_handle {
            handle.abort();
        }
    }
}

/// Axum middleware that (a) logs every incoming request for debugging
/// and (b) works around rmcp 0.8.5's strict Accept-header check.
///
/// rmcp 0.8.5 StreamableHttpService returns **HTTP 406 Not Acceptable**
/// unless the request's `Accept` header contains BOTH `application/json`
/// AND `text/event-stream`. Some MCP clients (observed with Claude Code
/// v2.1.114) only send one of the two and get rejected before they can
/// call any tool.
///
/// We rewrite the Accept header to the canonical combination so rmcp
/// always proceeds. rmcp then decides response shape (JSON vs SSE) based
/// on the request; both shapes are MCP-spec compliant.
async fn mcp_request_middleware(
    mut req: axum::extract::Request,
    next: axum::middleware::Next,
) -> axum::response::Response {
    use axum::http::{header, HeaderValue};
    let method = req.method().clone();
    let path = req.uri().path().to_string();
    let accept_in = req
        .headers()
        .get(header::ACCEPT)
        .and_then(|v| v.to_str().ok())
        .map(|s| s.to_string())
        .unwrap_or_default();

    // Always present both media types to rmcp; that's the only combo it accepts.
    req.headers_mut().insert(
        header::ACCEPT,
        HeaderValue::from_static("application/json, text/event-stream"),
    );

    log::debug!(
        "[mcp] {} {} accept-in=\"{}\" → \"application/json, text/event-stream\"",
        method,
        path,
        accept_in
    );

    next.run(req).await
}

/// Spawn the MCP server bound to `127.0.0.1:0` (OS-assigned port).
/// Returns the full endpoint info once bound. Server runs in a detached
/// tokio task; caller can drop the returned value (server keeps running
/// for the lifetime of the tokio runtime).
///
/// `self_pane_id` bakes a specific pane identity into THIS server
/// instance: every tool call to it is treated as coming from that pane.
/// Pass `None` for an "anonymous" server (legacy / non-multi-agent),
/// or `Some(pane_id)` to make `whoami()`, `is_self` in `list_panes`,
/// and identity-bound `send_to_pane` jobs all work without the
/// LLM needing to guess.
pub async fn spawn(
    panes: Arc<PaneStore>,
    self_pane_id: Option<String>,
) -> anyhow::Result<McpEndpoint> {
    spawn_with_port(panes, self_pane_id, 0).await
}

/// Like `spawn`, but lets the caller request a specific port.
///
/// `preferred_port = 0` falls back to OS-assigned (the per-pane coordination
/// servers want this — they're transient and ephemeral by design).
///
/// If the preferred port is busy we silently fall back to OS-assigned
/// rather than fail; the caller persists whatever port we got.
pub async fn spawn_with_port(
    panes: Arc<PaneStore>,
    self_pane_id: Option<String>,
    preferred_port: u16,
) -> anyhow::Result<McpEndpoint> {
    let service = StreamableHttpService::new(
        {
            let panes = panes.clone();
            let pane_id = self_pane_id.clone();
            move || Ok(CoffeeMcp::new(panes.clone(), pane_id.clone()))
        },
        LocalSessionManager::default().into(),
        StreamableHttpServerConfig::default(),
    );

    let router = axum::Router::new()
        .nest_service("/mcp", service)
        .layer(axum::middleware::from_fn(mcp_request_middleware));
    let listener = match preferred_port {
        0 => tokio::net::TcpListener::bind("127.0.0.1:0").await?,
        p => match tokio::net::TcpListener::bind(("127.0.0.1", p)).await {
            Ok(l) => l,
            Err(e) => {
                log::warn!(
                    "[mcp] preferred port {p} unavailable ({e}); falling back to OS-assigned"
                );
                tokio::net::TcpListener::bind("127.0.0.1:0").await?
            }
        },
    };
    let addr = listener.local_addr()?;

    let mut endpoint = McpEndpoint {
        url: format!("http://{}/mcp", addr),
        abort_handle: None,
    };

    log::info!("coffee-cli mcp server listening at {}", endpoint.url);

    // Keep an abort handle so closing a pane also releases its listener.
    let task = tokio::spawn(async move {
        if let Err(e) = axum::serve(listener, router).await {
            log::error!("coffee-cli mcp server exited with error: {}", e);
        }
    });
    endpoint.abort_handle = Some(task.abort_handle());

    Ok(endpoint)
}
