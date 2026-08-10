// Tauri v2 typed invoke wrapper

// Extend Window with Tauri globals to avoid TS2339
declare global {
  interface Window {
    __TAURI_INTERNALS__?: { invoke: (cmd: string, args?: unknown) => Promise<unknown> };
    __TAURI__?: {
      invoke?: (cmd: string, args?: unknown) => Promise<unknown>;
      core?: { invoke: (cmd: string, args?: unknown) => Promise<unknown> };
    };
  }
}

// isTauri: evaluated once at module load.
// Tauri injects __TAURI_INTERNALS__ synchronously before any scripts run.
export const isTauri =
  typeof window !== 'undefined' &&
  (!!window.__TAURI_INTERNALS__ || !!window.__TAURI__);

function createRendererInstanceId(): string {
  const cryptoApi = globalThis.crypto;
  if (typeof cryptoApi?.randomUUID === 'function') return cryptoApi.randomUUID();

  const bytes = new Uint8Array(16);
  if (typeof cryptoApi?.getRandomValues === 'function') {
    cryptoApi.getRandomValues(bytes);
  } else {
    for (let index = 0; index < bytes.length; index += 1) {
      bytes[index] = Math.floor(Math.random() * 256);
    }
  }
  bytes[6] = (bytes[6] & 0x0f) | 0x40;
  bytes[8] = (bytes[8] & 0x3f) | 0x80;
  const hex = Array.from(bytes, value => value.toString(16).padStart(2, '0')).join('');
  return `${hex.slice(0, 8)}-${hex.slice(8, 12)}-${hex.slice(12, 16)}-${hex.slice(16, 20)}-${hex.slice(20)}`;
}

/** One identity per WebView lifetime; never persisted or exposed to agents. */
export const rendererInstanceId = createRendererInstanceId();

// Resolve the invoke function across Tauri v1 / v2
function resolveInvoke(): ((cmd: string, args?: Record<string, unknown>) => Promise<unknown>) | null {
  const w = window as unknown as Record<string, unknown>;
  const internals = w.__TAURI_INTERNALS__ as Record<string, unknown> | undefined;
  if (internals && typeof internals.invoke === 'function') return internals.invoke as never;
  const tauri = w.__TAURI__ as Record<string, unknown> | undefined;
  if (tauri) {
    const core = tauri.core as Record<string, unknown> | undefined;
    if (core && typeof core.invoke === 'function') return core.invoke as never;
    if (typeof tauri.invoke === 'function') return tauri.invoke as never;
  }
  return null;
}

let _invoke = isTauri ? resolveInvoke() : null;

export function retryInvoke() {
  if (isTauri && !_invoke) _invoke = resolveInvoke();
  return _invoke;
}

// ── Git changes-panel types (mirror of src/git.rs serde output) ──
export interface GitFileEntry {
  /** Absolute, forward-slashed path (repo_root + "/" + rel). */
  path: string;
  /** Repo-relative path exactly as git reports it; diff/show specs use this. */
  rel: string;
  /** Single-letter status: M A D R C U or ? (untracked). */
  status: string;
  added: number;
  deleted: number;
}
/** A commit's metadata for the session-commits list (files fetched lazily via
 *  `gitCommitFiles` when the user expands a commit). */
export interface CommitMeta {
  /** Short hash (e.g. "abc1234"). */
  hash: string;
  /** Commit subject (first line). */
  message: string;
  author: string;
  /** Commit time, epoch seconds. */
  time: number;
}
export type GitChanges =
  | { state: 'no_git' }
  | { state: 'not_repo' }
  | {
      state: 'ok';
      repo_root: string;
      branch: string;
      /** Tracked files with uncommitted changes (staged OR unstaged, merged —
       *  numstat is HEAD↔worktree so the diff is "what changed since the last
       *  commit"). The staged/unstaged split was collapsed 2026-07-06. */
      uncommitted: GitFileEntry[];
      untracked: GitFileEntry[];
      /** Commits made since this Coffee CLI window opened (baseline..HEAD),
       *  metadata only — files fetched lazily via `gitCommitFiles`. Push-
       *  agnostic (push doesn't move HEAD). Reset on app close. */
      session_commits: CommitMeta[];
    };

export async function invoke<T>(cmd: string, args?: Record<string, unknown>): Promise<T> {
  if (!_invoke) throw new Error('Tauri IPC not available');
  return _invoke(cmd, args) as Promise<T>;
}

// ─── Type Definitions ────────────────────────────────────────────────────────

export interface SavedSession {
  id: string;
  name: string;
  tool: string;
  cwd: string;
  session_token: string | null;
  saved_at: string;
  file_path?: string;
  turn_count?: number;
}

/**
 * Topology-only checkpoint for a coordinated multi-agent workspace. Native
 * CLI resume tokens deliberately never cross this boundary: the backend keeps
 * them in its protected snapshot and validates the restore lease itself.
 */
export interface WorkspacePaneCheckpoint {
  pane_index: number;
  tool: string | null;
  sentinel_enabled: boolean;
  mcp_selection: McpProfileSelection;
  continuation?: 'fresh_by_user' | 'skipped' | null;
}

export interface WorkspaceCheckpoint {
  snapshot_id: string;
  workspace: string;
  pane_count: number;
  checkpoint_version: number;
  panes: WorkspacePaneCheckpoint[];
}

export interface ContinuationSummary {
  state: 'empty' | 'known' | 'needs_binding' | 'fresh_by_user' | 'skipped' | 'unsupported';
  reason: string | null;
  source: 'runtime_capture' | 'managed_claude_session' | 'manual_binding' | null;
  observed_at: number | null;
}

export interface WorkspacePaneSummary {
  pane_index: number;
  tool: string | null;
  sentinel_enabled: boolean;
  mcp_selection: McpProfileSelection;
  continuation: ContinuationSummary;
}

export interface WorkspaceSummary {
  snapshot_id: string;
  workspace: string;
  pane_count: number;
  checkpoint_version: number;
  revision: number;
  created_at: number;
  updated_at: number;
  panes: WorkspacePaneSummary[];
}

export type RestorePaneStatus =
  | 'empty'
  | 'skipped'
  | 'resumable'
  | 'fresh'
  | 'needs_binding'
  | 'cwd_missing'
  | 'tool_missing'
  | 'mcp_unavailable'
  | 'token_invalid';

export interface WorkspaceRestorePanePlan {
  pane_index: number;
  tool: string | null;
  sentinel_enabled: boolean;
  mcp_selection: McpProfileSelection;
  continuation: ContinuationSummary;
  status: RestorePaneStatus;
  reason: string | null;
}

export interface WorkspaceRestorePlan {
  snapshot_id: string;
  revision: number;
  workspace: string;
  pane_count: number;
  checkpoint_version: number;
  panes: WorkspaceRestorePanePlan[];
}

export interface BeginWorkspaceRestoreResult {
  attempt_id: string;
  plan: WorkspaceRestorePlan;
}

/** Display-only option for explicitly binding one recovered pane. */
export interface WorkspaceHistoryCandidate {
  selection_id: string;
  name: string;
  saved_at: string;
  turn_count?: number;
}

/** One use of a backend-owned restore lease, bound to one pane launch. */
export interface RestoreAttemptRef {
  snapshot_id: string;
  attempt_id: string;
  pane_index: number;
}

export interface DirEntryInfo {
  name: string;
  path: string;
  is_dir: boolean;
  size: number;
}

export interface FontInfo {
  family: string;
  monospace: boolean;
}

// ─── Typed Commands ──────────────────────────────────────────────────────────

export const commands = {
  pickFolder: () => invoke<string>('pick_folder'),

  // Window decorators
  windowMinimize: () => invoke<void>('window_minimize'),
  windowMaximize: () => invoke<void>('window_maximize'),
  windowClose: () => invoke<void>('window_close'),

  // Tier Terminal API
  tierTerminalStart: (sessionId: string, runId: string, tool: string | null, cols: number, rows: number, themeMode: string, locale?: string, toolData?: string, cwd?: string, resumeToken?: string, shell?: string, mcpSelection?: McpProfileSelection, workspaceContext?: WorkspaceCheckpoint, restoreAttempt?: RestoreAttemptRef) =>
    invoke<boolean>('tier_terminal_start', { sessionId, runId, tool, toolData: toolData ?? null, cols, rows, themeMode, locale: locale ?? null, cwd: cwd ?? null, resumeToken: resumeToken ?? null, shell: shell ?? null, mcpSelection: mcpSelection ?? { mode: 'auto' }, workspaceContext: workspaceContext ?? null, restoreAttempt: restoreAttempt ?? null, rendererInstanceId }),
  tierTerminalInput: (sessionId: string, runId: string, data: string) =>
    invoke<void>('tier_terminal_input', { sessionId, runId, data }),
  tierTerminalAgentStatus: (sessionId: string, runId: string, status: 'idle' | 'working' | 'wait_input') =>
    invoke<void>('tier_terminal_agent_status', { sessionId, runId, status }),
  tierTerminalFailTask: (sessionId: string, runId: string, reason: string) =>
    invoke<void>('tier_terminal_fail_task', { sessionId, runId, reason }),
  /** Raw write used for system-generated input (auto-skip prompts, etc.). */
  tierTerminalRawWrite: (sessionId: string, runId: string, data: string) =>
    invoke<void>('tier_terminal_raw_write', { sessionId, runId, data }),
  tierTerminalKill: (sessionId: string, runId: string) =>
    invoke<void>('tier_terminal_kill', { sessionId, runId }),
  tierTerminalResize: (sessionId: string, runId: string, cols: number, rows: number) =>
    invoke<void>('tier_terminal_resize', { sessionId, runId, cols, rows }),

  /** Notify the Rust backend that the window's visibility changed.
   *  When hidden=true, every per-session worker thread (ticker, emitter)
   *  widens its sleep / coalesce window so a backgrounded Coffee CLI
   *  drops to near-zero CPU instead of running its full foreground
   *  cadence. Apple Silicon laptops in particular need this to keep
   *  the chassis cool when users leave the app open all day. */
  setBackgroundMode: (hidden: boolean) =>
    invoke<void>('set_background_mode', { hidden }),

  /** Per-tab visibility signal — narrower than setBackgroundMode. Flip when
   *  this session's terminal element enters/leaves the viewport (a tab
   *  switch, not just the whole window hiding) so its backend emitter can
   *  widen its coalesce window while nobody is looking at that tab. */
  setSessionActive: (sessionId: string, runId: string, active: boolean) =>
    invoke<void>('set_session_active', { sessionId, runId, active }),

  // Session Resume
  getNativeHistory: () => invoke<SavedSession[]>('get_native_history'),
  registerMultiAgentRenderer: () =>
    invoke<void>('register_multi_agent_renderer', { rendererInstanceId }),
  listMultiAgentWorkspaces: () =>
    invoke<WorkspaceSummary[]>('list_multi_agent_workspaces'),
  checkpointMultiAgentWorkspace: (checkpoint: WorkspaceCheckpoint, allowCreate: boolean) =>
    invoke<WorkspaceSummary>('checkpoint_multi_agent_workspace', { checkpoint, allowCreate, rendererInstanceId }),
  discardMultiAgentWorkspace: (snapshotId: string) =>
    invoke<void>('discard_multi_agent_workspace', { snapshotId, rendererInstanceId }),
  listMultiAgentWorkspacePaneHistory: (snapshotId: string, paneIndex: number) =>
    invoke<WorkspaceHistoryCandidate[]>('list_multi_agent_workspace_pane_history', { snapshotId, paneIndex }),
  bindMultiAgentWorkspacePane: (snapshotId: string, paneIndex: number, selectionId: string) =>
    invoke<WorkspaceSummary>('bind_multi_agent_workspace_pane', { snapshotId, paneIndex, selectionId, rendererInstanceId }),
  setMultiAgentWorkspacePaneContinuation: (snapshotId: string, paneIndex: number, choice: 'fresh_by_user' | 'skipped') =>
    invoke<WorkspaceSummary>('set_multi_agent_workspace_pane_continuation', { snapshotId, paneIndex, choice, rendererInstanceId }),
  preflightMultiAgentWorkspace: (snapshotId: string) =>
    invoke<WorkspaceRestorePlan>('preflight_multi_agent_workspace', { snapshotId }),
  beginMultiAgentWorkspaceRestore: (snapshotId: string, expectedRevision: number) =>
    invoke<BeginWorkspaceRestoreResult>('begin_multi_agent_workspace_restore', { snapshotId, expectedRevision, rendererInstanceId }),
  releaseMultiAgentWorkspaceRestore: (attemptId: string) =>
    invoke<void>('release_multi_agent_workspace_restore', { attemptId }),
  cancelMultiAgentWorkspacePaneLaunch: (sessionId: string, restoreAttempt: RestoreAttemptRef) =>
    invoke<void>('cancel_multi_agent_workspace_pane_launch', { sessionId, restoreAttempt }),
  /** Per-session activity for the contribution heatmap.
   *  One entry per session file: { ts: epoch seconds, count: msg lines }.
   *  Frontend buckets ts into local-day boxes for the grid. */
  getMessageHeatmap: () =>
    invoke<{ ts: number; count: number }[]>('get_message_heatmap'),
  readNativeSession: (filePath: string) => invoke<string>('read_native_session', { filePath }),
  readOpencodeSession: (sessionId: string) =>
    invoke<string>('read_opencode_session', { sessionId }),
  // Hermes Agent sessions from the newer SQLite state.db (no per-session
  // file). Returns the same newline-delimited {message:{role,content}} shape
  // as readNativeSession so ChatReader's parser handles it unchanged.
  readHermesSession: (sessionToken: string) =>
    invoke<string>('read_hermes_session', { sessionToken }),
  // MiMo Code (OpenCode fork) — same SQLite schema, read from mimocode.db.
  readMimocodeSession: (sessionToken: string) =>
    invoke<string>('read_mimocode_session', { sessionToken }),
  checkNetworkPort: (host: string, port: number) => invoke<boolean>('check_network_port', { host, port }),

  // Tool availability detection
  checkToolsInstalled: () =>
    invoke<Record<string, boolean>>('check_tools_installed'),

  // External launch request (`launch --tool <id> [--cwd <dir>]`) handed to
  // the GUI at cold start. Drained exactly once by CenterPanel on mount —
  // warm-start requests arrive as 'launch-request' events instead.
  takePendingLaunch: () =>
    invoke<{ tool: string; cwd?: string } | null>('take_pending_launch'),

  /** Probed availability of optional shells (pwsh / Git Bash / wsl on
   *  Windows; zsh/bash/fish/sh on Unix). Fed to the SettingsModal shell
   *  picker so it only shows shells the user actually has. Inbox shells
   *  (powershell/cmd on Windows) are assumed present and NOT in this
   *  payload. Platform-specific fields are absent cross-OS — read with
   *  optional chaining. */
  detectShells: () => invoke<{
    pwsh_available?: boolean; git_bash_available?: boolean;
    zsh_available?: boolean; bash_available?: boolean;
    fish_available?: boolean; sh_available?: boolean;
    wsl_available: boolean;
  }>('detect_shells'),

  /** Static list of tools registered in the Rust src/tools/ registry —
   *  one entry per supported AI CLI with the canonical display name.
   *  Loaded once at app boot and cached; see `lib/tool-info.ts`. */
  listTools: () => invoke<{ id: string; displayName: string }[]>('list_tools'),

  /** Re-run legacy cleanup and non-hook presentation migration for one tool. */
  maintainToolIntegration: (tool: string) =>
    invoke<void>('maintain_tool_integration', { tool }),

  /** Gambit — save a clipboard-pasted image to a temp file and return its path.
   *  The returned absolute path is inserted into the textarea so the AI CLI agent
   *  (Claude Code, etc.) can read the image via the local filesystem. */
  saveClipboardImage: (dataBase64: string, extension: string) =>
    invoke<string>('save_clipboard_image', { dataBase64, extension }),

  /** Read an image from the OS clipboard and persist it as a PNG temp file,
   *  returning its absolute path (or null when the clipboard has no image).
   *  Uses the native backend so WebView2 never shows a permission prompt. */
  readClipboardImage: () => invoke<string | null>('read_clipboard_image'),

  listDirectory: (path: string) => invoke<DirEntryInfo[]>('list_directory', { path }),
  listSystemFonts: () => invoke<FontInfo[]>('list_system_fonts'),

  // ── Git-backed changes panel ──────────────────────────────────────
  // The right-side "修改记录" tab reads the active folder's git working
  // tree. `gitChanges` returns no_git / not_repo / ok (with uncommitted +
  // untracked groups, plus session_commits made this window); the
  // DiffPanel pulls each side's blob via `gitShowFile` and feeds the existing
  // jsdiff + Shiki pipeline.
  gitChanges: (folder: string) => invoke<GitChanges>('git_changes', { folder }),
  // Content of `<spec>` — e.g. "HEAD:src/a.ts" (committed) or ":src/a.ts"
  // (staged/index blob). null when the path doesn't exist at that revision;
  // the panel treats null as an empty side (file renders as all-additions).
  gitShowFile: (repoRoot: string, spec: string) =>
    invoke<string | null>('git_show_file', { repoRoot, spec }),
  // `git init` a folder — backs the not-a-repo state's "initialize" button.
  gitInit: (folder: string) => invoke<void>('git_init', { folder }),
  // Capture the session baseline (current HEAD) for a repo, idempotently.
  // Called at app launch + tab switch (not poll-gated — one rev-parse). Scopes
  // the "修改记录" session-commits list to commits made this window.
  gitCaptureBaseline: (folder: string) => invoke<void>('git_capture_baseline', { folder }),
  // Files changed in a single commit (lazy — on expand in the session-commits
  // list).
  gitCommitFiles: (repoRoot: string, hash: string) =>
    invoke<GitFileEntry[]>('git_commit_files', { repoRoot, hash }),
  // Current on-disk text of a file (lossy-UTF8 so GBK / latin-1 still
  // render; null = missing / binary). Used for the working-tree "new" side
  // and for untracked files, which have no git blob to `show`.
  readTextFile: (path: string) =>
    invoke<string | null>('read_text_file', { path }),

  // File system operations
  fsDelete: (path: string) => invoke<void>('fs_delete', { path }),
  fsRename: (path: string, newName: string) => invoke<void>('fs_rename', { path, newName }),
  fsPaste: (action: string, srcPath: string, targetDir: string) =>
    invoke<void>('fs_paste', { action, srcPath, targetDir }),
  showInFolder: (path: string) => invoke<void>('show_in_folder', { path }),

  // Task Board persistence (~/.coffee-cli/tasks.json)
  loadTasks: () => invoke<string>('load_tasks'),
  saveTasks: (data: string) => invoke<void>('save_tasks', { data }),

  // Credential store — passwords live in OS keychain, never in localStorage
  savePassword: (host: string, username: string, password: string) =>
    invoke<void>('save_password', { host, username, password }),
  loadPassword: (host: string, username: string) =>
    invoke<string | null>('load_password', { host, username }),
  deletePassword: (host: string, username: string) =>
    invoke<void>('delete_password', { host, username }),
  openUrl: (url: string) =>
    invoke<void>('open_url', { url }),

  // In-app self-update (Windows): use the published Mechoy version marker to
  // derive the matching installer, stream it with progress, then launch it.
  // Emits `self-update-progress` while it runs (see onSelfUpdateProgress).
  // Rejects on non-Windows / download failure — caller falls back to openUrl.
  downloadAndInstallUpdate: (version: string) =>
    invoke<void>('download_and_install_update', { version }),

  // Live fs watcher — subscribes to OS-native events under `path` and
  // emits `fs-refresh` Tauri events that Explorer already listens for.
  // Calling start with a new path implicitly replaces the previous watcher.
  startFsWatcher: (path: string) =>
    invoke<void>('start_fs_watcher', { path }),
  stopFsWatcher: () =>
    invoke<void>('stop_fs_watcher'),

  enableMultiAgentMode: (workspace: string, tools: string[]) =>
    invoke<{ ok: boolean; warnings: string[] }>('enable_multi_agent_mode', { workspace, tools }),
  disableMultiAgentMode: (workspace: string) =>
    invoke<{ ok: boolean; warnings: string[] }>('disable_multi_agent_mode', { workspace }),

  // ─── Per-tool launch overrides (~/.coffee-cli/tools.json) ───────────
  getToolConfig: (tool: string) =>
    invoke<ToolConfigEntry>('get_tool_config', { tool }),
  getAllToolConfigs: () =>
    invoke<Record<string, ToolConfigEntry>>('get_all_tool_configs'),
  setToolConfig: (tool: string, entry: ToolConfigEntry) =>
    invoke<void>('set_tool_config', { tool, entry }),

  // ─── Coffee-managed MCP profiles (~/.coffee-cli/mcp.json) ──────────
  getMcpConfig: () => invoke<McpConfig>('get_mcp_config'),
  getMcpConfigRecoveryToken: () => invoke<string>('get_mcp_config_recovery_token'),
  resetInvalidMcpConfig: (expectedToken: string) =>
    invoke<McpConfig>('reset_invalid_mcp_config', { expectedToken }),
  getMcpMultiAgentBinding: (workspace: string, pane: number) =>
    invoke<string | null>('get_mcp_multi_agent_binding', { workspace, pane }),
  saveMcpConfig: (config: McpConfig) => invoke<McpConfig>('save_mcp_config', { config }),
  setMcpMultiAgentBinding: (workspace: string, pane: number, mutation: McpMultiAgentBindingMutation) =>
    invoke<McpConfig>('set_mcp_multi_agent_binding', { workspace, pane, mutation }),
};

// In-app self-update progress, emitted by the Mechoy release downloader.
export interface SelfUpdateProgress {
  status: 'speed_test' | 'downloading' | 'launching' | 'error';
  percent: number;
}

// Subscribe to self-update progress while downloadAndInstallUpdate runs.
// Returns an unlisten fn. Dynamic-imports the event API (matches how the
// rest of the app subscribes to Tauri events).
export async function onSelfUpdateProgress(
  cb: (p: SelfUpdateProgress) => void,
): Promise<() => void> {
  const { listen } = await import('@tauri-apps/api/event');
  return listen<SelfUpdateProgress>('self-update-progress', (e) => cb(e.payload));
}

/**
 * One entry in `~/.coffee-cli/tools.json`. All fields are optional —
 * empty strings / empty arrays fall through to Coffee CLI's built-in
 * defaults for that tool. Lets users say things like "always launch claude
 * with --dangerously-skip-permissions" or "run codex through
 * `docker exec mybox`" without us having to auto-detect every
 * conceivable install path.
 */
export interface ToolConfigEntry {
  /** Full launch command. Whitespace-split — first token is the binary,
   *  the rest are prepended to args. Empty falls through to default. */
  command: string;
  /** Args appended AFTER the built-in args (so tool-managed flags like
   *  --mcp-config / --append-system-prompt still come first). */
  extra_args: string[];
  /** Pre-fills the cwd selector when starting a new tab. Empty falls
   *  through to the launchpad's last-used cwd. */
  default_cwd: string;
  /** Custom directory to scan for this tool's session history files.
   *  Empty falls through to the built-in scan path. */
  history_path: string;
}

export type McpProfileSelection =
  | { mode: 'auto' }
  | { mode: 'none' }
  | { mode: 'profile'; profile_id: string };

/** A persisted workspace+pane change. This is intentionally separate from
 * `McpProfileSelection`, where Auto and None are per-run choices. */
export type McpMultiAgentBindingMutation =
  | { mode: 'set'; profile_id: string }
  | { mode: 'clear' };

export interface McpEnvRef {
  from_env: string;
}

export type McpTransport =
  | { type: 'stdio'; command: string; args: string[]; env: Record<string, McpEnvRef> }
  | { type: 'http'; url: string; headers: Record<string, McpEnvRef> };

export interface McpServerDefinition {
  name: string;
  transport: McpTransport;
  enabled: boolean;
}

export interface McpProfile {
  name: string;
  servers: string[];
}

export interface McpConfig {
  version: number;
  revision: number;
  servers: Record<string, McpServerDefinition>;
  profiles: Record<string, McpProfile>;
  defaults: {
    global: string | null;
    agents: Record<string, string | null>;
  };
  workspace_bindings: Array<{ workspace: string; profile: string }>;
  multi_agent_bindings: Array<{ workspace: string; panes: Record<string, string> }>;
}
