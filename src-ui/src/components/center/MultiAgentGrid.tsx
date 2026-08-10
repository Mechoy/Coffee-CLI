// MultiAgentGrid.tsx — independent "four pane" tab content.
//
// Design (user spec 2026-04-23):
//   Four paper slices. No borders, no card backgrounds, no paddings, no
//   header strips. Each pane renders identically to a single-terminal
//   tab. Only visual differentiation between panes is:
//     (a) a 1/2/3/4 number badge in the top-right, tinted by the theme
//         accent;
//     (b) when any pane has keyboard focus, the other three dim to 0.35
//         opacity so the user's eye follows the cursor.
//
//   All four panes are peers — no primary/worker distinction. Every
//   pane has its own PTY session `${tabId}::pane-${idx}` where idx is
//   1..4 matching the UI badge; the backend PaneStore / MCP tools see
//   the same id, so when the user says "pane 2" the CLI's MCP call
//   targets the exact same slot.
//
// Implementation notes:
//   - focused pane detection uses onFocus (capture, because the event
//     fires on the nested xterm textarea and we want to catch it on the
//     pane wrapper). Initial state is null → all panes full brightness
//     until the first click; this keeps the first-paint visually calm.
//   - `requiresCwd: false` was set in CenterPanel's Launchpad entry, so
//     tab.folderPath may be null. TierTerminal handles that by falling
//     back to the user's home directory inside terminal::spawn.

import { useEffect, useMemo, useRef, useState } from 'react';
import { useAppState, type TerminalSession, type ToolType, type MultiAgentPane } from '../../store/app-state';
import { TierTerminal } from './TierTerminal';
import { ErrorBoundary } from '../common/ErrorBoundary';
import { commands, type McpConfig, type McpProfileSelection, type ToolConfigEntry, type WorkspaceCheckpoint } from '../../tauri';
import { setFocusedPane } from '../../lib/pane-focus';
import { useT } from '../../i18n/useT';
import { getToolDisplayName } from '../../lib/tool-info';
import { getMcpLaunchWrapperWarning, McpLaunchWrapperWarning, type McpLaunchWrapperWarningInfo } from '../../features/mcp/McpLaunchWrapperWarning';
import './MultiAgentGrid.css';

interface Props {
  tab: TerminalSession;
  hasBg: boolean;
  bgUrl: string;
  bgType: 'image' | 'video' | 'none';
  paneCount?: 2 | 3 | 4;
}

// Multi-agent quadrant CLIs. Each pane runs one of these as a primary
// participant — the per-pane MCP server (built lazily in
// `tier_terminal_start`) is wired into each via the CLI-specific path
// documented in `mcp_injector.rs`. OpenCode joins via the
// `OPENCODE_CONFIG_CONTENT=<merged-json>` env var so its workspace
// stays untouched (same zero-pollution invariant as the other two).
//
const PANE_CLI_OPTIONS: Array<{ value: ToolType; label: string }> = (
  ['claude', 'codex', 'opencode'] as const
).map((value) => ({ value, label: getToolDisplayName(value) }));

function buildWorkspaceCheckpoint(
  tab: TerminalSession,
  panes: MultiAgentPane[],
  paneCount: number,
): WorkspaceCheckpoint | null {
  if (!tab.folderPath || !tab.multiAgentWorkspaceId) return null;
  const checkpointPanes = panes.map((pane) => {
    const tool = pane.tool ?? pane.restoreTool ?? null;
    return {
      pane_index: pane.paneIdx,
      tool,
      sentinel_enabled: pane.sentinelEnabled !== false,
      mcp_selection: pane.mcpSelection ?? { mode: 'auto' as const },
      continuation: tool ? pane.workspaceContinuation ?? null : null,
    };
  });
  // Opening a layout but never selecting a participant is not a recoverable
  // conversation. Once a record exists, however, an all-empty layout is an
  // intentional topology update: it must be saved so closed panes do not
  // reappear as old conversations on the next recovery.
  if (!checkpointPanes.some((pane) => pane.tool !== null) && !tab.multiAgentWorkspacePersisted) return null;
  return {
    snapshot_id: tab.multiAgentWorkspaceId,
    workspace: tab.folderPath,
    pane_count: paneCount,
    checkpoint_version: tab.multiAgentWorkspaceCheckpointVersion ?? 0,
    panes: checkpointPanes,
  };
}

export function MultiAgentGrid({ tab, hasBg, bgUrl, bgType, paneCount = 4 }: Props) {
  const { state, dispatch } = useAppState();
  const [focusedPaneIdx, setFocusedPaneIdx] = useState<number | null>(null);
  const [mcpConfig, setMcpConfig] = useState<McpConfig | null>(null);
  const [mcpConfigUnavailable, setMcpConfigUnavailable] = useState(false);
  const [savedPaneProfiles, setSavedPaneProfiles] = useState<Record<number, string | null>>({});
  const [toolConfigs, setToolConfigs] = useState<Record<string, ToolConfigEntry>>({});
  const [topologyMutationPending, setTopologyMutationPending] = useState(false);
  const topologyMutationRef = useRef(false);

  // Detect which of the 3 coordination-eligible CLIs are actually installed
  // so the picker greys out the ones the user doesn't have (same visual
  // language as the Desktop launchpad — see .launchpad-card-disabled).
  // Runs once on mount; missing keys default to `true` so we don't flash
  // a false "disabled" state before the IPC resolves.
  const [toolsInstalled, setToolsInstalled] = useState<Record<string, boolean>>({});
  useEffect(() => {
    commands.checkToolsInstalled()
      .then(result => setToolsInstalled(result))
      .catch(() => {});
    commands.getAllToolConfigs()
      .then(setToolConfigs)
      .catch(() => setToolConfigs({}));
  }, []);

  // MCP Profiles can be edited while this grid remains mounted behind the
  // settings dialog. Reload once Settings closes so a deleted profile cannot
  // remain as a stale explicit launch selection, and new profiles appear
  // without requiring the user to remount the whole grid.
  useEffect(() => {
    let disposed = false;
    commands.getMcpConfig()
      .then(config => {
        if (disposed) return;
        setMcpConfig(config);
        setMcpConfigUnavailable(false);
      })
      .catch(() => {
        if (disposed) return;
        setMcpConfig(null);
        setMcpConfigUnavailable(true);
      });
    return () => { disposed = true; };
  }, [state.settingsOpen]);

  // A Profile may have been deleted from Settings after the user selected it
  // for this pane. Auto is the safe replacement: deleting a profile also
  // clears its saved bindings server-side, so this cannot silently retain a
  // capability the user removed.
  useEffect(() => {
    if (!mcpConfig) return;
    // A recovered workspace has already passed an explicit preflight. Do not
    // silently replace its named profile with Auto while it is being restored.
    if (tab.multiAgentWorkspaceId) return;
    for (const pane of tab.multiAgent?.panes ?? []) {
      const selection = pane.mcpSelection;
      if (selection?.mode === 'profile' && !mcpConfig.profiles[selection.profile_id]) {
        dispatch({ type: 'SET_PANE_MCP_SELECTION', tabId: tab.id, paneIdx: pane.paneIdx, selection: { mode: 'auto' } });
      }
    }
  }, [dispatch, mcpConfig, tab.id, tab.multiAgent?.panes]);

  useEffect(() => {
    let disposed = false;
    if (!tab.folderPath || !mcpConfig) {
      setSavedPaneProfiles({});
      return () => { disposed = true; };
    }

    Promise.all(
      Array.from({ length: paneCount }, (_, index) => index + 1).map(async paneIdx => [
        paneIdx,
        await commands.getMcpMultiAgentBinding(tab.folderPath!, paneIdx),
      ] as const),
    )
      .then((bindings) => {
        if (!disposed) setSavedPaneProfiles(Object.fromEntries(bindings));
      })
      .catch(() => {
        if (!disposed) setSavedPaneProfiles({});
      });

    return () => { disposed = true; };
  }, [mcpConfig, paneCount, tab.folderPath]);

  // paneIdx is 1-indexed to match the user-visible badge numbering and
  // the MCP session id (`::pane-1` .. `::pane-4`). See the header comment.
  const panes: MultiAgentPane[] = (tab.multiAgent?.panes
    ?? Array.from({ length: paneCount }, (_, i) => ({
         paneIdx: i + 1,
         tool: null as ToolType,
       }))).slice(0, paneCount);
  // A restore lease pins the saved layout. Hold structural controls until all
  // first-launch panes either claim or reject their one-time permissions.
  const restoreLaunchPending = panes.some((pane) => pane.restoreLeasePending);

  const workspaceCheckpoint = useMemo(
    () => buildWorkspaceCheckpoint(tab, panes, paneCount),
    [paneCount, panes, tab],
  );
  const checkpointSignature = workspaceCheckpoint ? JSON.stringify(workspaceCheckpoint) : '';
  // The backend owns first-record creation inside the PTY launch fence. A
  // debounce here must never win that race: otherwise a failed first spawn can
  // leave a recovery card for a workspace that never had a running pane.
  const hasPersistedWorkspace = tab.multiAgentWorkspacePersisted === true;
  // Keep the latest compact topology available to unmount cleanup. The normal
  // checkpoint path remains debounced, but closing Coffee immediately after a
  // pane/layout change must not throw that change away with the cancelled
  // timer.
  const latestWorkspaceCheckpointRef = useRef<WorkspaceCheckpoint | null>(workspaceCheckpoint);
  const latestWorkspacePersistedRef = useRef(hasPersistedWorkspace);
  latestWorkspaceCheckpointRef.current = workspaceCheckpoint;
  latestWorkspacePersistedRef.current = hasPersistedWorkspace;

  // Persist only the compact coordinated topology. Terminal output, PTYs, MCP
  // artifacts, and native session tokens remain outside the WebView and never
  // enter this debounce path.
  useEffect(() => {
    if (!workspaceCheckpoint || !hasPersistedWorkspace) return;
    const timer = window.setTimeout(() => {
      commands
        .checkpointMultiAgentWorkspace(
          workspaceCheckpoint,
          false,
        )
        .then(() => dispatch({ type: 'MARK_MULTI_AGENT_WORKSPACE_PERSISTED', tabId: tab.id }))
        .catch((error) => {
          console.warn('[multi-agent] workspace checkpoint failed:', error);
        });
    }, 180);
    return () => window.clearTimeout(timer);
  }, [checkpointSignature, dispatch, hasPersistedWorkspace, tab.id]); // eslint-disable-line react-hooks/exhaustive-deps

  useEffect(() => () => {
    const checkpoint = latestWorkspaceCheckpointRef.current;
    if (!checkpoint || !latestWorkspacePersistedRef.current) return;
    commands
      .checkpointMultiAgentWorkspace(checkpoint, false)
      .catch((error) => {
        console.warn('[multi-agent] final workspace checkpoint failed:', error);
      });
  }, [tab.id]);

  /**
   * Structural changes must reach disk before their React state transition.
   * A debounced checkpoint is still useful for ordinary updates, but it cannot
   * protect a pane close followed immediately by an application exit. The
   * backend rejects stale renderer identities and conflicting versions, so the
   * visible layout remains unchanged when this durable write cannot be made.
   */
  const persistTopologyBeforeDispatch = async (
    updatePanes: (checkpointPanes: WorkspaceCheckpoint['panes']) => WorkspaceCheckpoint['panes'],
  ): Promise<boolean> => {
    if (!workspaceCheckpoint) return true;
    if (topologyMutationRef.current) return false;
    const nextCheckpoint: WorkspaceCheckpoint = {
      ...workspaceCheckpoint,
      checkpoint_version: workspaceCheckpoint.checkpoint_version + 1,
      panes: updatePanes(workspaceCheckpoint.panes),
    };
    topologyMutationRef.current = true;
    setTopologyMutationPending(true);
    try {
      await commands.checkpointMultiAgentWorkspace(nextCheckpoint, false);
      latestWorkspaceCheckpointRef.current = nextCheckpoint;
      latestWorkspacePersistedRef.current = true;
      dispatch({ type: 'MARK_MULTI_AGENT_WORKSPACE_PERSISTED', tabId: tab.id });
      return true;
    } catch (error) {
      // A pane can be closed before its first start reaches the backend. In
      // that case there is deliberately no record yet; TierTerminal's run
      // cancellation tombstone prevents that delayed start from creating one.
      if (!hasPersistedWorkspace) return true;
      console.warn('[multi-agent] immediate workspace checkpoint failed:', error);
      return false;
    } finally {
      topologyMutationRef.current = false;
      setTopologyMutationPending(false);
    }
  };

  const setPaneMcpSelection = async (paneIdx: number, selection: McpProfileSelection) => {
    if (restoreLaunchPending || topologyMutationPending) return;
    const committed = await persistTopologyBeforeDispatch((checkpointPanes) => checkpointPanes.map((pane) => (
      pane.pane_index === paneIdx ? { ...pane, mcp_selection: selection } : pane
    )));
    if (!committed) return;
    dispatch({ type: 'SET_PANE_MCP_SELECTION', tabId: tab.id, paneIdx, selection });
    // Auto follows the persisted workspace+pane binding and None is a
    // one-run override. Neither may alter mcp.json. A named profile is the
    // explicit action that saves a binding for this pane.
    if (!tab.folderPath || selection.mode !== 'profile') return;
    commands.setMcpMultiAgentBinding(tab.folderPath, paneIdx, {
      mode: 'set',
      profile_id: selection.profile_id,
    })
      .then(setMcpConfig)
      .catch((error) => {
        console.warn('[multi-agent] MCP binding save failed:', error);
      });
  };

  const clearPaneMcpBinding = (paneIdx: number) => {
    if (restoreLaunchPending || topologyMutationPending) return;
    if (!tab.folderPath) return;
    commands.setMcpMultiAgentBinding(tab.folderPath, paneIdx, { mode: 'clear' })
      .then(setMcpConfig)
      .catch((error) => {
        console.warn('[multi-agent] MCP binding clear failed:', error);
      });
  };

  // ─── Multi-agent mode handshake ─────────────────────────────────────
  //
  // Post-v1.5 the backend wires each pane's MCP server and CLI
  // artifacts lazily inside `tier_terminal_start` (per-pane temp dir
  // under `<temp>/coffee-cli/panes/`).
  // Workspaces stay pristine — no CLAUDE.md / AGENTS.md /
  // .multi-agent/ ever gets written, no global ~/.codex
  // mcp_servers entries get touched.
  //
  // We still call enable/disable here so the backend has a structured
  // place to surface preflight warnings, and so future cross-cutting
  // logic (telemetry, license gating, …) has the obvious hook.
  const installedSigRef = useRef<string>('');
  const activeTools: string[] = Array.from(
    new Set(panes.map(p => p.tool).filter((t): t is NonNullable<ToolType> => !!t))
  ).map(String).sort();
  const sig = `${tab.folderPath ?? ''}|${activeTools.join(',')}`;
  useEffect(() => {
    if (!tab.folderPath) return;
    if (activeTools.length === 0) return;
    if (installedSigRef.current === sig) return;
    installedSigRef.current = sig;

    commands
      .enableMultiAgentMode(tab.folderPath, activeTools)
      .then((r) => {
        if (r.warnings?.length) {
          console.warn('[multi-agent] enable warnings:', r.warnings);
        }
      })
      .catch((e) => {
        console.warn('[multi-agent] enable_multi_agent_mode failed (UI still usable):', e);
      });
  }, [sig]); // eslint-disable-line react-hooks/exhaustive-deps

  // Cleanup on unmount: each TierTerminal releases its own MCP listener
  // and temp artifacts; this workspace-level hook remains for future
  // cross-cutting teardown.
  useEffect(() => {
    const workspace = tab.folderPath;
    return () => {
      if (!workspace) return;
      if (!installedSigRef.current) return;
      commands
        .disableMultiAgentMode(workspace)
        .catch((e) => console.warn('[multi-agent] disable on unmount failed:', e));
    };
  }, [tab.folderPath]);

  const onSelectTool = (paneIdx: number, tool: ToolType) => {
    if (restoreLaunchPending || topologyMutationRef.current) return;
    const selection = panes.find((pane) => pane.paneIdx === paneIdx)?.mcpSelection;
    // Settings closes asynchronously. Guard the short interval before the
    // profile-refresh effect resets a deleted selection, otherwise a quick
    // launch can reach the backend with a profile that no longer exists.
    const mcpSelection = selection?.mode === 'profile'
      && mcpConfig
      && !mcpConfig.profiles[selection.profile_id]
      ? { mode: 'auto' as const }
      : undefined;
    dispatch({ type: 'SET_PANE_TOOL', tabId: tab.id, paneIdx, tool, mcpSelection });
  };

  // 2-pane and 3-pane coordination always render as side-by-side columns — the 2×2
  // grid mode is only meaningful for 4 panes. The user's columns/grid
  // toggle in multi-agent settings therefore only applies when paneCount === 4.
  const isColumns = paneCount !== 4 || state.multiAgentLayout === 'columns';
  const layoutMod = isColumns
    ? ` multi-agent-grid--columns multi-agent-grid--columns-${paneCount}`
    : ' multi-agent-grid--grid';

  return (
    <div className={`multi-agent-grid-standalone${layoutMod}${hasBg && bgUrl ? ' multi-agent-has-bg' : ''}`}>
      {/* Grid-level wallpaper. Sits behind all four panes so empty
          panes (CLI picker state) and any gaps show the user's bg
          just like single-terminal tabs do. Filled panes also get
          their TierTerminal's own .tier-terminal-bg layer — harmless
          redundancy, but guarantees xterm-transparent composition
          stays correct regardless of grid-level state. Mirrors the
          .launchpad-bg pattern in CenterPanel so the user-controlled
          image opacity (--wallpaper-opacity on :root) applies the
          same way. */}
      {hasBg && bgUrl && (
        <div className="multi-agent-bg">
          {bgType === 'video'
            ? <video src={bgUrl} autoPlay loop muted playsInline />
            : <img src={bgUrl} alt="" draggable={false} />}
        </div>
      )}
      {panes.map((pane) => {
        const paneSessionId = `${tab.id}::pane-${pane.paneIdx}`;
        const restoreAttempt = tab.multiAgentRestoreAttemptId && pane.restoreLeasePending && workspaceCheckpoint ? {
          snapshot_id: workspaceCheckpoint.snapshot_id,
          attempt_id: tab.multiAgentRestoreAttemptId,
          pane_index: pane.paneIdx,
        } : undefined;
        const settleWorkspaceLaunch = workspaceCheckpoint ? async (accepted: boolean): Promise<boolean> => {
          if (!accepted && restoreAttempt) {
            try {
              await commands.cancelMultiAgentWorkspacePaneLaunch(paneSessionId, restoreAttempt);
            } catch (error) {
              // Do not clear the UI lease before the backend has accepted its
              // revocation. Otherwise the grid can become editable while the
              // backend still considers this snapshot actively restoring.
              console.warn('[multi-agent] pane restore cancellation failed:', error);
              return false;
            }
          }
          dispatch({
            type: 'ACK_PANE_WORKSPACE_LAUNCH',
            tabId: tab.id,
            paneIdx: pane.paneIdx,
            accepted,
            restoreAttemptId: restoreAttempt?.attempt_id,
          });
          return true;
        } : undefined;
        const savedProfileId = savedPaneProfiles[pane.paneIdx] ?? null;
        const savedProfileName = savedProfileId
          ? (mcpConfig?.profiles[savedProfileId]?.name ?? savedProfileId)
          : null;
        // Coordinated panes need completion wake-ups for the dispatch loop to
        // finish. Undefined is the backward-compatible default-on state;
        // users can still explicitly turn the marker scanner off.
        const sentinelEnabled = pane.sentinelEnabled !== false;
        const isEmpty = pane.tool === null;
        const isDeferredRecovery = isEmpty && pane.restoreStatus !== undefined;
        const isFocused = focusedPaneIdx === pane.paneIdx;
        const isDimmed = focusedPaneIdx !== null && !isFocused;

        return (
          <div
            key={pane.paneIdx}
            className={`multi-agent-pane pane-slot-${pane.paneIdx}${isDimmed ? ' is-dimmed' : ''}`}
            // Capture-phase so we win the focus-intent announcement even
            // when the click lands on inert background (empty pane body,
            // padding around the CLI picker, gap between xterm canvas
            // and pane edges). onFocusCapture alone only fires when the
            // click actually hits a focusable element, which misses all
            // the "dead" pixels users expect to be clickable.
            onMouseDownCapture={() => {
              setFocusedPaneIdx(pane.paneIdx);
              // Mirror to a module-level registry so ActiveGambit (which
              // lives at App-level, outside this component) can route its
              // Send to the pane the user last clicked.
              setFocusedPane(tab.id, pane.paneIdx);
            }}
            onFocusCapture={() => {
              setFocusedPaneIdx(pane.paneIdx);
              setFocusedPane(tab.id, pane.paneIdx);
            }}
          >
            {/* Theme-tinted pane number badge.
                - Empty pane: plain numeric label (nothing to close here).
                - Active pane: button that shows the number by default and
                  swaps to × on hover. Clicking kills this pane's PTY and
                  resets its tool to null — the pane re-renders as the
                  3-button CLI picker without disturbing the other panes
                  or closing the whole Tab. */}
            {(() => {
              // Green dot after a structured MCP task completion event.
              // within the last 30 minutes. Past that we assume the pane has
              // started a new turn and the "done" signal is stale.
              const showDot = sentinelEnabled && pane.completionTs
                && Date.now() - pane.completionTs < 30 * 60 * 1000;
              return isEmpty ? (
                <div className="pane-number-badge">
                  {pane.paneIdx}
                  {showDot && <span className="pane-completion-dot" aria-hidden="true" />}
                </div>
              ) : (
                <button
                  type="button"
                  className="pane-number-badge pane-number-badge--closable"
                  aria-label={`Close pane ${pane.paneIdx}`}
                  disabled={restoreLaunchPending || topologyMutationPending}
                  onClick={async (e) => {
                    e.stopPropagation();
                    if (restoreLaunchPending || topologyMutationPending) return;
                    const committed = await persistTopologyBeforeDispatch((checkpointPanes) => checkpointPanes.map((checkpointPane) => (
                      checkpointPane.pane_index === pane.paneIdx
                        ? {
                            ...checkpointPane,
                            tool: null,
                            mcp_selection: { mode: 'auto' },
                            continuation: null,
                          }
                        : checkpointPane
                    )));
                    if (!committed) return;
                    // TierTerminal owns the runId and performs the guarded
                    // cleanup from its unmount effect after this dispatch.
                    if (focusedPaneIdx === pane.paneIdx) {
                      setFocusedPaneIdx(null);
                      setFocusedPane(tab.id, null);
                    }
                    dispatch({
                      type: 'SET_PANE_TOOL',
                      tabId: tab.id,
                      paneIdx: pane.paneIdx,
                      tool: null,
                    });
                  }}
                >
                  <span className="pane-badge-num">{pane.paneIdx}</span>
                  <span className="pane-badge-x" aria-hidden="true">×</span>
                  {showDot && <span className="pane-completion-dot" aria-hidden="true" />}
                </button>
              );
            })()}

            <div className="multi-agent-pane-body">
              {isDeferredRecovery ? (
                <DeferredRecoveryPane
                  tool={pane.restoreTool}
                  status={pane.restoreStatus!}
                  reason={pane.restoreReason}
                  disabled={restoreLaunchPending || toolsInstalled[String(pane.restoreTool)] === false}
                  onStartFresh={() => {
                    if (pane.restoreTool) onSelectTool(pane.paneIdx, pane.restoreTool);
                  }}
                />
              ) : isEmpty ? (
                <EmptyPanePicker
                  onSelect={(tool) => onSelectTool(pane.paneIdx, tool)}
                  profiles={mcpConfig?.profiles ?? {}}
                  config={mcpConfig}
                  toolConfigs={toolConfigs}
                  mcpConfigUnavailable={mcpConfigUnavailable}
                  mcpSelection={pane.mcpSelection ?? { mode: 'auto' }}
                  onMcpSelection={(selection) => setPaneMcpSelection(pane.paneIdx, selection)}
                  savedProfileName={savedProfileName}
                  onClearSavedBinding={savedProfileId ? () => clearPaneMcpBinding(pane.paneIdx) : undefined}
                  sentinelEnabled={sentinelEnabled}
                  onToggleSentinel={() => {
                    if (restoreLaunchPending || topologyMutationPending) return;
                    void (async () => {
                      const committed = await persistTopologyBeforeDispatch((checkpointPanes) => checkpointPanes.map((checkpointPane) => (
                        checkpointPane.pane_index === pane.paneIdx
                          ? { ...checkpointPane, sentinel_enabled: !sentinelEnabled }
                          : checkpointPane
                      )));
                      if (!committed) return;
                      dispatch({
                        type: 'SET_PANE_SENTINEL',
                        tabId: tab.id,
                        paneIdx: pane.paneIdx,
                        enabled: !sentinelEnabled,
                      });
                    })();
                  }}
                  toolsInstalled={toolsInstalled}
                  disabled={restoreLaunchPending || topologyMutationPending}
                />
              ) : (
                <ErrorBoundary
                  fallbackLabel="Tier Terminal Error"
                  onError={() => { void settleWorkspaceLaunch?.(false); }}
                >
                  {/* Pass hasBg through so xterm stays transparent when
                      the user has a wallpaper set — this lets the single
                      grid-level .multi-agent-bg show through all panes.
                      bgUrl is intentionally empty so TierTerminal never
                      renders its own per-pane .tier-terminal-bg layer;
                      the shared grid wallpaper handles that instead. */}
                  <TierTerminal
                    key={`${paneSessionId}:${tab.restartKey ?? 0}`}
                    sessionId={paneSessionId}
                    tool={pane.tool}
                    toolName={undefined}
                    theme={state.currentTheme}
                    lang={state.currentLang}
                    isActive={isFocused}
                    toolData={pane.toolData}
                    folderPath={tab.folderPath}
                    mcpSelection={pane.mcpSelection}
                    workspaceContext={workspaceCheckpoint ?? undefined}
                    restoreAttempt={restoreAttempt}
                    lockWorkspaceCwd={workspaceCheckpoint !== null}
                    onWorkspaceLaunchSettled={settleWorkspaceLaunch}
                    hasBg={hasBg}
                    bgUrl=""
                    bgType="none"
                    termColorScheme={state.termColorScheme}
                  />
                </ErrorBoundary>
              )}
            </div>
          </div>
        );
      })}
    </div>
  );
}

interface DeferredRecoveryPaneProps {
  tool?: Exclude<ToolType, null>;
  status: NonNullable<MultiAgentPane['restoreStatus']>;
  reason?: string;
  disabled: boolean;
  onStartFresh: () => void;
}

function DeferredRecoveryPane({ tool, status, reason, disabled, onStartFresh }: DeferredRecoveryPaneProps) {
  const { state } = useAppState();
  const chinese = state.currentLang.startsWith('zh');
  const labels: Record<DeferredRecoveryPaneProps['status'], string> = chinese
    ? {
        needs_binding: '需要绑定历史对话',
        cwd_missing: '工作目录不可用',
        tool_missing: '工具不可用',
        mcp_unavailable: 'MCP 配置不可用',
        token_invalid: '原生会话不可继续',
        launch_failed: '恢复窗格启动失败',
      }
    : {
        needs_binding: 'History binding required',
        cwd_missing: 'Workspace unavailable',
        tool_missing: 'Tool unavailable',
        mcp_unavailable: 'MCP configuration unavailable',
        token_invalid: 'Native session unavailable',
        launch_failed: 'Recovered pane did not start',
      };
  const canStartFresh = Boolean(tool) && (status === 'needs_binding' || status === 'token_invalid' || status === 'launch_failed');

  return (
    <div className="deferred-recovery-pane" role="status">
      <strong>{labels[status]}</strong>
      {reason && <span>{reason}</span>}
      {canStartFresh && (
        <button
          type="button"
          className="deferred-recovery-pane-action"
          disabled={disabled}
          onClick={(event) => {
            event.stopPropagation();
            if (!disabled) onStartFresh();
          }}
        >
          {chinese ? `新建 ${getToolDisplayName(tool!)} 对话` : `Start new ${getToolDisplayName(tool!)}`}
        </button>
      )}
    </div>
  );
}

interface EmptyPanePickerProps {
  onSelect: (tool: ToolType) => void;
  profiles: McpConfig['profiles'];
  config: McpConfig | null;
  toolConfigs: Record<string, ToolConfigEntry>;
  mcpConfigUnavailable: boolean;
  mcpSelection: McpProfileSelection;
  onMcpSelection: (selection: McpProfileSelection) => void;
  savedProfileName: string | null;
  onClearSavedBinding?: () => void;
  sentinelEnabled: boolean;
  onToggleSentinel: () => void;
  toolsInstalled: Record<string, boolean>;
  disabled: boolean;
}

// Per-CLI setup hints removed per user request: the paper-slice
// aesthetic calls for a completely clean empty pane — just the
// CLI buttons, nothing else. Auth friction (Codex login, OpenCode auth)
// surfaces naturally once the user clicks; no need to pre-announce it.
// The skip-permissions auto-accept still lives in server.rs for Claude,
// so users don't see a speed bump there.
function EmptyPanePicker({ onSelect, profiles, config, toolConfigs, mcpConfigUnavailable, mcpSelection, onMcpSelection, savedProfileName, onClearSavedBinding, sentinelEnabled, onToggleSentinel, toolsInstalled, disabled }: EmptyPanePickerProps) {
  const t = useT();
  const wrapperWarnings = PANE_CLI_OPTIONS
    .map((option) => getMcpLaunchWrapperWarning(
      mcpSelection,
      config,
      String(option.value),
      toolConfigs,
      { allowKnownAutoDefault: false, toolLabel: option.label },
    ))
    .filter((warning): warning is McpLaunchWrapperWarningInfo => warning !== null);
  return (
    <div className="empty-pane-picker">
      <div className="empty-pane-options">
        {PANE_CLI_OPTIONS.map((opt) => {
          // Default to installed when the detection result hasn't landed
          // yet (keys missing) to avoid a false-negative flash on mount.
          const installed = toolsInstalled[String(opt.value)] !== false;
          return (
            <button
              key={String(opt.value)}
              className="empty-pane-option"
              disabled={disabled || !installed}
              onClick={(e) => {
                e.stopPropagation();
                if (disabled || !installed) return;
                onSelect(opt.value);
              }}
            >
              {opt.label}
            </button>
          );
        })}
      </div>
      {(mcpConfigUnavailable || Object.keys(profiles).length > 0) && (
        <div className="empty-pane-mcp-row" onClick={(event) => event.stopPropagation()}>
          <label className="empty-pane-mcp">
            <span>MCP</span>
            <select
              value={mcpSelection.mode === 'profile' ? `profile:${mcpSelection.profile_id}` : mcpSelection.mode}
              disabled={disabled}
              onChange={(event) => {
                const value = event.target.value;
                onMcpSelection(value === 'auto'
                  ? { mode: 'auto' }
                  : value === 'none'
                    ? { mode: 'none' }
                    : { mode: 'profile', profile_id: value.slice('profile:'.length) });
              }}
            >
              <option value="auto">{savedProfileName ? `Auto (${savedProfileName})` : 'Auto'}</option>
              <option value="none">None</option>
              {Object.entries(profiles).map(([id, profile]) => <option key={id} value={`profile:${id}`}>{profile.name}</option>)}
            </select>
          </label>
          {onClearSavedBinding && (
            <button
              type="button"
              className="empty-pane-mcp-clear"
              title={`Clear saved MCP profile ${savedProfileName}`}
              aria-label={`Clear saved MCP profile ${savedProfileName}`}
              disabled={disabled}
              onClick={onClearSavedBinding}
            >
              ×
            </button>
          )}
        </div>
      )}
      {wrapperWarnings.map((warning) => (
        <McpLaunchWrapperWarning key={`${warning.tool}-${warning.wrapper}`} warning={warning} className="mcp-launch-wrapper-warning--pane" />
      ))}
      <div className="sentinel-toggle-row">
        <div
          className="sentinel-toggle-head"
          role="button"
          tabIndex={disabled ? -1 : 0}
          aria-pressed={sentinelEnabled}
          aria-disabled={disabled}
          aria-label="Toggle sentinel protocol"
          onClick={(e) => { e.stopPropagation(); if (!disabled) onToggleSentinel(); }}
          onKeyDown={(e) => {
            if (e.key === 'Enter' || e.key === ' ') {
              e.preventDefault();
              if (!disabled) onToggleSentinel();
            }
          }}
        >
          <span className="sentinel-toggle-label">{t('sentinel.protocol')}</span>
          <span
            className={`sentinel-switch${sentinelEnabled ? ' is-on' : ''}`}
            aria-hidden="true"
          />
        </div>
      </div>
    </div>
  );
}
