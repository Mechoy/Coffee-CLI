import { useCallback, useEffect, useRef, useState } from 'react';
import {
  commands,
  isTauri,
  type WorkspaceHistoryCandidate,
  type WorkspaceRestorePlan,
  type WorkspaceSummary,
} from '../../tauri';
import { useAppState } from '../../store/app-state';
import './MultiAgentWorkspaceRecovery.css';

type RestoreResult = {
  attempt_id: string;
  plan: WorkspaceRestorePlan;
};

type PaneContinuationChoice = 'fresh_by_user' | 'skipped';

interface Props {
  onRestore: (result: RestoreResult, snapshot: WorkspaceSummary) => void;
}

const COPY = {
  zh: {
    title: '恢复协作工作区',
    reload: '重新检查',
    loading: '正在读取已保存的协作工作区…',
    saved: '上次保存',
    panes: '个窗口',
    inspect: '检查恢复条件',
    restore: '恢复工作区',
    configure: '配置窗口',
    discard: '丢弃',
    confirmDiscard: '确认丢弃',
    cancel: '取消',
    chooseHistory: '选择历史对话',
    chooseAnother: '重新绑定',
    newSession: '新建对话',
    skip: '跳过窗口',
    historyLoading: '正在读取本机历史…',
    noHistory: '没有找到同一工具和工作目录下可继续的历史对话。',
    bindingHint: '选择一个历史对话后才会恢复该窗口。',
    preflightRequired: '部分窗口暂不能恢复',
    pane: '窗口',
    empty: '空窗口',
    sentinelOn: '完成提醒开',
    sentinelOff: '完成提醒关',
    mcpAuto: 'MCP 自动',
    mcpNone: 'MCP 未启用',
    mcpProfile: 'MCP',
  },
  en: {
    title: 'Restore collaborative workspaces',
    reload: 'Reload',
    loading: 'Loading saved collaborative workspaces...',
    saved: 'Last saved',
    panes: 'panes',
    inspect: 'Check restore readiness',
    restore: 'Restore workspace',
    configure: 'Configure panes',
    discard: 'Discard',
    confirmDiscard: 'Discard workspace',
    cancel: 'Cancel',
    chooseHistory: 'Choose history',
    chooseAnother: 'Rebind history',
    newSession: 'New session',
    skip: 'Skip pane',
    historyLoading: 'Loading local history...',
    noHistory: 'No resumable history matches this tool and workspace.',
    bindingHint: 'Choose an exact history item before this pane can resume.',
    preflightRequired: 'Some panes need attention',
    pane: 'Pane',
    empty: 'Empty pane',
    sentinelOn: 'Completion alert on',
    sentinelOff: 'Completion alert off',
    mcpAuto: 'MCP auto',
    mcpNone: 'MCP off',
    mcpProfile: 'MCP',
  },
} as const;

type Copy = { [Key in keyof (typeof COPY)['zh']]: string };

function messageFrom(error: unknown): string {
  return error instanceof Error ? error.message : String(error);
}

function displayTool(tool: string | null | undefined, copy: Copy): string {
  if (!tool) return copy.empty;
  const names: Record<string, string> = {
    claude: 'Claude Code',
    codex: 'Codex',
    opencode: 'OpenCode',
  };
  return names[tool] ?? tool;
}

function workspaceName(workspace: string): string {
  const trimmed = workspace.replace(/[\\/]+$/, '');
  const splitAt = Math.max(trimmed.lastIndexOf('/'), trimmed.lastIndexOf('\\'));
  return splitAt >= 0 ? trimmed.slice(splitAt + 1) || workspace : workspace;
}

function savedAt(value: number, chinese: boolean): string {
  if (!Number.isFinite(value) || value <= 0) return '';
  return new Intl.DateTimeFormat(chinese ? 'zh-CN' : 'en-US', {
    dateStyle: 'medium',
    timeStyle: 'short',
  }).format(new Date(value));
}

function continuationLabel(state: string, copy: Copy): string {
  const labels: Record<string, string> = {
    empty: copy.empty,
    known: copy === COPY.zh ? '已关联对话' : 'History linked',
    needs_binding: copy === COPY.zh ? '需要绑定历史' : 'History required',
    fresh_by_user: copy === COPY.zh ? '将新建对话' : 'New session selected',
    skipped: copy === COPY.zh ? '已跳过' : 'Skipped',
    unsupported: copy === COPY.zh ? '无法继续' : 'Resume unavailable',
  };
  return labels[state] ?? state;
}

function planStatusLabel(status: string, copy: Copy): string {
  const labels: Record<string, string> = {
    empty: copy.empty,
    skipped: copy === COPY.zh ? '已跳过' : 'Skipped',
    resumable: copy === COPY.zh ? '可恢复' : 'Ready to resume',
    fresh: copy === COPY.zh ? '将新建对话' : 'New session',
    needs_binding: copy === COPY.zh ? '需要绑定历史' : 'History required',
    cwd_missing: copy === COPY.zh ? '工作目录不存在' : 'Workspace missing',
    tool_missing: copy === COPY.zh ? '工具未安装' : 'Tool unavailable',
    mcp_unavailable: copy === COPY.zh ? 'MCP 配置不可用' : 'MCP unavailable',
    token_invalid: copy === COPY.zh ? '对话标识无效' : 'Session no longer valid',
  };
  return labels[status] ?? status;
}

function statusTone(status: string): 'ready' | 'attention' | 'muted' | 'error' {
  if (status === 'resumable' || status === 'known' || status === 'fresh' || status === 'fresh_by_user') return 'ready';
  if (status === 'empty' || status === 'skipped') return 'muted';
  if (status === 'needs_binding') return 'attention';
  return 'error';
}

function mcpLabel(selection: unknown, copy: Copy): string {
  const value = selection as { mode?: string; profile_id?: string } | null;
  if (!value || value.mode === 'auto') return copy.mcpAuto;
  if (value.mode === 'none') return copy.mcpNone;
  if (value.mode === 'profile') return `${copy.mcpProfile}: ${value.profile_id ?? '-'}`;
  return copy.mcpAuto;
}

function isPlanReady(plan: WorkspaceRestorePlan): boolean {
  return plan.panes.some((pane) => pane.status === 'resumable' || pane.status === 'fresh');
}

function canStartFresh(status: string): boolean {
  return !['empty', 'cwd_missing', 'tool_missing', 'mcp_unavailable'].includes(status);
}

function canChooseHistory(status: string): boolean {
  return !['empty', 'cwd_missing', 'tool_missing', 'mcp_unavailable', 'unsupported'].includes(status);
}

export function MultiAgentWorkspaceRecovery({ onRestore }: Props) {
  const { state } = useAppState();
  const chinese = state.currentLang.startsWith('zh');
  const copy = chinese ? COPY.zh : COPY.en;
  const mountedRef = useRef(true);
  const requestSequenceRef = useRef(0);
  const historyTargetRef = useRef<string | null>(null);
  const [workspaces, setWorkspaces] = useState<WorkspaceSummary[]>([]);
  const [plans, setPlans] = useState<Record<string, WorkspaceRestorePlan>>({});
  const [loading, setLoading] = useState(isTauri);
  const [loadError, setLoadError] = useState<string | null>(null);
  const [cardErrors, setCardErrors] = useState<Record<string, string>>({});
  const [expandedId, setExpandedId] = useState<string | null>(null);
  const [busyId, setBusyId] = useState<string | null>(null);
  const [discardConfirmationId, setDiscardConfirmationId] = useState<string | null>(null);
  const [historyTarget, setHistoryTarget] = useState<{ snapshotId: string; paneIndex: number } | null>(null);
  const [history, setHistory] = useState<WorkspaceHistoryCandidate[]>([]);
  const [historyLoading, setHistoryLoading] = useState(false);
  const [historyError, setHistoryError] = useState<string | null>(null);

  useEffect(() => {
    mountedRef.current = true;
    return () => {
      mountedRef.current = false;
      requestSequenceRef.current += 1;
      historyTargetRef.current = null;
    };
  }, []);

  const beginRequest = useCallback(() => {
    requestSequenceRef.current += 1;
    return requestSequenceRef.current;
  }, []);
  const requestIsCurrent = useCallback(
    (requestId: number) => mountedRef.current && requestSequenceRef.current === requestId,
    [],
  );
  const setPickerTarget = useCallback((target: { snapshotId: string; paneIndex: number } | null) => {
    historyTargetRef.current = target ? `${target.snapshotId}:${target.paneIndex}` : null;
    setHistoryTarget(target);
  }, []);
  const clearHistoryPicker = useCallback(() => {
    historyTargetRef.current = null;
    setHistoryTarget(null);
    setHistory([]);
    setHistoryError(null);
    setHistoryLoading(false);
  }, []);
  const cancelHistoryPicker = useCallback(() => {
    beginRequest();
    clearHistoryPicker();
  }, [beginRequest, clearHistoryPicker]);

  const replaceSummary = useCallback((summary: WorkspaceSummary) => {
    setWorkspaces((current) => current
      .map((workspace) => workspace.snapshot_id === summary.snapshot_id ? summary : workspace)
      .sort((left, right) => right.updated_at - left.updated_at));
    setPlans((current) => {
      const next = { ...current };
      delete next[summary.snapshot_id];
      return next;
    });
  }, []);

  const reload = useCallback(async () => {
    const requestId = beginRequest();
    clearHistoryPicker();
    if (!isTauri) {
      if (requestIsCurrent(requestId)) setLoading(false);
      return;
    }
    setLoading(true);
    try {
      const snapshots = await commands.listMultiAgentWorkspaces();
      if (!requestIsCurrent(requestId)) return;
      setWorkspaces(snapshots);
      setLoadError(null);
    } catch (error) {
      if (requestIsCurrent(requestId)) setLoadError(messageFrom(error));
    } finally {
      if (requestIsCurrent(requestId)) setLoading(false);
    }
  }, [beginRequest, clearHistoryPicker, requestIsCurrent]);

  useEffect(() => { void reload(); }, [reload]);

  const inspect = useCallback(async (snapshot: WorkspaceSummary): Promise<WorkspaceRestorePlan | null> => {
    const requestId = beginRequest();
    clearHistoryPicker();
    setBusyId(`${snapshot.snapshot_id}:inspect`);
    setCardErrors((current) => ({ ...current, [snapshot.snapshot_id]: '' }));
    try {
      const plan = await commands.preflightMultiAgentWorkspace(snapshot.snapshot_id);
      if (!requestIsCurrent(requestId)) return null;
      setPlans((current) => ({ ...current, [snapshot.snapshot_id]: plan }));
      setExpandedId(snapshot.snapshot_id);
      return plan;
    } catch (error) {
      if (requestIsCurrent(requestId)) {
        setCardErrors((current) => ({ ...current, [snapshot.snapshot_id]: messageFrom(error) }));
      }
      return null;
    } finally {
      if (requestIsCurrent(requestId)) setBusyId(null);
    }
  }, [beginRequest, clearHistoryPicker, requestIsCurrent]);

  const handleRestore = useCallback(async (snapshot: WorkspaceSummary) => {
    const requestId = beginRequest();
    clearHistoryPicker();
    setBusyId(`${snapshot.snapshot_id}:restore`);
    try {
      const plan = await commands.preflightMultiAgentWorkspace(snapshot.snapshot_id);
      if (!requestIsCurrent(requestId)) return;
      setPlans((current) => ({ ...current, [snapshot.snapshot_id]: plan }));
      setExpandedId(snapshot.snapshot_id);
      if (!isPlanReady(plan)) return;
      const result = await commands.beginMultiAgentWorkspaceRestore(snapshot.snapshot_id, plan.revision);
      if (!requestIsCurrent(requestId)) {
        commands.releaseMultiAgentWorkspaceRestore(result.attempt_id).catch(() => {});
        return;
      }
      onRestore(result, snapshot);
    } catch (error) {
      if (requestIsCurrent(requestId)) {
        setCardErrors((current) => ({ ...current, [snapshot.snapshot_id]: messageFrom(error) }));
      }
    } finally {
      if (requestIsCurrent(requestId)) setBusyId(null);
    }
  }, [beginRequest, clearHistoryPicker, onRestore, requestIsCurrent]);

  const changeContinuation = useCallback(async (
    snapshot: WorkspaceSummary,
    paneIndex: number,
    choice: PaneContinuationChoice,
  ) => {
    const requestId = beginRequest();
    clearHistoryPicker();
    setBusyId(`${snapshot.snapshot_id}:pane-${paneIndex}`);
    setCardErrors((current) => ({ ...current, [snapshot.snapshot_id]: '' }));
    try {
      const updated = await commands.setMultiAgentWorkspacePaneContinuation(
        snapshot.snapshot_id,
        paneIndex,
        choice,
      );
      if (!requestIsCurrent(requestId)) return;
      replaceSummary(updated);
      const plan = await commands.preflightMultiAgentWorkspace(updated.snapshot_id);
      if (!requestIsCurrent(requestId)) return;
      setPlans((current) => ({ ...current, [updated.snapshot_id]: plan }));
      setExpandedId(updated.snapshot_id);
    } catch (error) {
      if (requestIsCurrent(requestId)) {
        setCardErrors((current) => ({ ...current, [snapshot.snapshot_id]: messageFrom(error) }));
      }
    } finally {
      if (requestIsCurrent(requestId)) setBusyId(null);
    }
  }, [beginRequest, clearHistoryPicker, replaceSummary, requestIsCurrent]);

  const openHistoryPicker = useCallback(async (snapshot: WorkspaceSummary, paneIndex: number) => {
    const requestId = beginRequest();
    const targetKey = `${snapshot.snapshot_id}:${paneIndex}`;
    setExpandedId(snapshot.snapshot_id);
    setPickerTarget({ snapshotId: snapshot.snapshot_id, paneIndex });
    setHistoryError(null);
    setHistory([]);
    setHistoryLoading(true);
    try {
      const candidates = await commands.listMultiAgentWorkspacePaneHistory(
        snapshot.snapshot_id,
        paneIndex,
      );
      if (!requestIsCurrent(requestId) || historyTargetRef.current !== targetKey) return;
      setHistory(candidates);
    } catch (error) {
      if (requestIsCurrent(requestId) && historyTargetRef.current === targetKey) {
        setHistoryError(messageFrom(error));
        setHistory([]);
      }
    } finally {
      if (requestIsCurrent(requestId) && historyTargetRef.current === targetKey) {
        setHistoryLoading(false);
      }
    }
  }, [beginRequest, requestIsCurrent, setPickerTarget]);

  const bindHistory = useCallback(async (
    snapshot: WorkspaceSummary,
    paneIndex: number,
    candidate: WorkspaceHistoryCandidate,
  ) => {
    const requestId = beginRequest();
    setBusyId(`${snapshot.snapshot_id}:pane-${paneIndex}`);
    setCardErrors((current) => ({ ...current, [snapshot.snapshot_id]: '' }));
    try {
      const updated = await commands.bindMultiAgentWorkspacePane(
        snapshot.snapshot_id,
        paneIndex,
        candidate.selection_id,
      );
      if (!requestIsCurrent(requestId)) return;
      replaceSummary(updated);
      setPickerTarget(null);
      const plan = await commands.preflightMultiAgentWorkspace(updated.snapshot_id);
      if (!requestIsCurrent(requestId)) return;
      setPlans((current) => ({ ...current, [updated.snapshot_id]: plan }));
      setExpandedId(updated.snapshot_id);
    } catch (error) {
      if (requestIsCurrent(requestId)) {
        setCardErrors((current) => ({ ...current, [snapshot.snapshot_id]: messageFrom(error) }));
      }
    } finally {
      if (requestIsCurrent(requestId)) setBusyId(null);
    }
  }, [beginRequest, replaceSummary, requestIsCurrent, setPickerTarget]);

  const discard = useCallback(async (snapshot: WorkspaceSummary) => {
    const requestId = beginRequest();
    clearHistoryPicker();
    setBusyId(`${snapshot.snapshot_id}:discard`);
    try {
      await commands.discardMultiAgentWorkspace(snapshot.snapshot_id);
      if (!requestIsCurrent(requestId)) return;
      setWorkspaces((current) => current.filter((workspace) => workspace.snapshot_id !== snapshot.snapshot_id));
      setPlans((current) => {
        const next = { ...current };
        delete next[snapshot.snapshot_id];
        return next;
      });
      if (expandedId === snapshot.snapshot_id) setExpandedId(null);
      setDiscardConfirmationId(null);
    } catch (error) {
      if (requestIsCurrent(requestId)) {
        setCardErrors((current) => ({ ...current, [snapshot.snapshot_id]: messageFrom(error) }));
      }
    } finally {
      if (requestIsCurrent(requestId)) setBusyId(null);
    }
  }, [beginRequest, clearHistoryPicker, expandedId, requestIsCurrent]);

  if (!isTauri) return null;
  if (!loading && workspaces.length === 0 && !loadError) return null;

  return (
    <section className="workspace-recovery" aria-labelledby="workspace-recovery-title">
      <div className="workspace-recovery-heading">
        <div>
          <h2 id="workspace-recovery-title">{copy.title}</h2>
          {loading && <span className="workspace-recovery-loading" aria-live="polite">{copy.loading}</span>}
        </div>
        <button
          type="button"
          className="workspace-recovery-icon-button"
          aria-label={copy.reload}
          data-tip={copy.reload}
          onClick={() => { void reload(); }}
          disabled={loading}
        >
          <svg width="15" height="15" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="1.8" strokeLinecap="round" strokeLinejoin="round" aria-hidden="true">
            <path d="M21 12a9 9 0 0 0-15.2-6.5L3 8" />
            <path d="M3 3v5h5" />
            <path d="M3 12a9 9 0 0 0 15.2 6.5L21 16" />
            <path d="M21 21v-5h-5" />
          </svg>
        </button>
      </div>

      {loadError && (
        <div className="workspace-recovery-error" role="status">
          {loadError}
        </div>
      )}

      <div className="workspace-recovery-list">
        {workspaces.map((snapshot) => {
          const expanded = expandedId === snapshot.snapshot_id;
          const plan = plans[snapshot.snapshot_id];
          const planReady = plan ? isPlanReady(plan) : false;
          const cardBusy = busyId?.startsWith(`${snapshot.snapshot_id}:`) ?? false;
          const hasAttention = plan?.panes.some((pane) => ![
            'empty',
            'skipped',
            'resumable',
            'fresh',
          ].includes(pane.status)) ?? false;

          return (
            <article key={snapshot.snapshot_id} className={`workspace-recovery-item${expanded ? ' is-expanded' : ''}`}>
              <div className="workspace-recovery-item-main">
                <button
                  type="button"
                  className="workspace-recovery-item-title"
                  onClick={() => setExpandedId(expanded ? null : snapshot.snapshot_id)}
                  aria-expanded={expanded}
                >
                  <span className="workspace-recovery-folder" aria-hidden="true">
                    <svg width="16" height="16" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="1.8" strokeLinecap="round" strokeLinejoin="round">
                      <path d="M22 19a2 2 0 0 1-2 2H4a2 2 0 0 1-2-2V5a2 2 0 0 1 2-2h5l2 3h9a2 2 0 0 1 2 2z" />
                    </svg>
                  </span>
                  <span className="workspace-recovery-item-text">
                    <strong>{workspaceName(snapshot.workspace)}</strong>
                    <span>{snapshot.workspace}</span>
                  </span>
                  <span className="workspace-recovery-item-meta">
                    <span>{snapshot.pane_count} {copy.panes}</span>
                    <span>{copy.saved} {savedAt(snapshot.updated_at, chinese)}</span>
                  </span>
                  <svg className="workspace-recovery-chevron" width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="1.8" strokeLinecap="round" strokeLinejoin="round" aria-hidden="true">
                    <path d={expanded ? 'm6 15 6-6 6 6' : 'm6 9 6 6 6-6'} />
                  </svg>
                </button>

                <div className="workspace-recovery-actions">
                  <button
                    type="button"
                    className="workspace-recovery-button workspace-recovery-button-primary"
                    onClick={() => {
                      if (plan && !planReady) setExpandedId(snapshot.snapshot_id);
                      else void handleRestore(snapshot);
                    }}
                    disabled={cardBusy}
                  >
                    {plan && !planReady ? copy.configure : copy.restore}
                  </button>
                  {discardConfirmationId === snapshot.snapshot_id ? (
                    <>
                      <button
                        type="button"
                        className="workspace-recovery-button workspace-recovery-button-danger"
                        onClick={() => { void discard(snapshot); }}
                        disabled={cardBusy}
                      >
                        {copy.confirmDiscard}
                      </button>
                      <button
                        type="button"
                        className="workspace-recovery-text-button"
                        onClick={() => setDiscardConfirmationId(null)}
                        disabled={cardBusy}
                      >
                        {copy.cancel}
                      </button>
                    </>
                  ) : (
                    <button
                      type="button"
                      className="workspace-recovery-text-button workspace-recovery-text-button-danger"
                      onClick={() => setDiscardConfirmationId(snapshot.snapshot_id)}
                      disabled={cardBusy}
                    >
                      {copy.discard}
                    </button>
                  )}
                </div>
              </div>

              <div className="workspace-recovery-pane-summary" aria-label={`${snapshot.pane_count} ${copy.panes}`}>
                {snapshot.panes.map((pane) => {
                  const planned = plan?.panes.find((candidate) => candidate.pane_index === pane.pane_index);
                  const state = planned?.status ?? pane.continuation.state;
                  return (
                    <span key={pane.pane_index} className={`workspace-recovery-chip tone-${statusTone(state)}`}>
                      <b>{pane.pane_index}</b>
                      {displayTool(pane.tool, copy)}
                    </span>
                  );
                })}
              </div>

              {expanded && (
                <div className="workspace-recovery-detail">
                  {hasAttention && <p className="workspace-recovery-attention" role="status">{copy.preflightRequired}</p>}
                  {cardErrors[snapshot.snapshot_id] && (
                    <p className="workspace-recovery-card-error" role="status">{cardErrors[snapshot.snapshot_id]}</p>
                  )}

                  <div className="workspace-recovery-pane-list">
                    {snapshot.panes.map((pane) => {
                      const planned = plan?.panes.find((candidate) => candidate.pane_index === pane.pane_index);
                      const state = planned?.status ?? pane.continuation.state;
                      const detailReason = planned?.reason ?? pane.continuation.reason;
                      const pickerOpen = historyTarget?.snapshotId === snapshot.snapshot_id
                        && historyTarget.paneIndex === pane.pane_index;
                      const paneBusy = busyId === `${snapshot.snapshot_id}:pane-${pane.pane_index}`;
                      const isEmpty = state === 'empty' || !pane.tool;

                      return (
                        <div key={pane.pane_index} className="workspace-recovery-pane-row">
                          <div className="workspace-recovery-pane-index">{copy.pane} {pane.pane_index}</div>
                          <div className="workspace-recovery-pane-info">
                            <div className="workspace-recovery-pane-title">
                              <strong>{displayTool(pane.tool, copy)}</strong>
                              <span className={`workspace-recovery-status tone-${statusTone(state)}`}>
                                {planned ? planStatusLabel(state, copy) : continuationLabel(state, copy)}
                              </span>
                            </div>
                            <div className="workspace-recovery-pane-meta">
                              <span>{mcpLabel(pane.mcp_selection, copy)}</span>
                              <span>{pane.sentinel_enabled ? copy.sentinelOn : copy.sentinelOff}</span>
                            </div>
                            {detailReason && <p className="workspace-recovery-pane-reason">{detailReason}</p>}
                          </div>

                          {!isEmpty && (
                            <div className="workspace-recovery-pane-actions">
                              {canChooseHistory(state) && (
                                <button
                                  type="button"
                                  className="workspace-recovery-text-button"
                                  onClick={() => { void openHistoryPicker(snapshot, pane.pane_index); }}
                                  disabled={cardBusy || paneBusy}
                                >
                                  {state === 'known' || state === 'resumable' ? copy.chooseAnother : copy.chooseHistory}
                                </button>
                              )}
                              {canStartFresh(state) && state !== 'fresh' && state !== 'fresh_by_user' && (
                                <button
                                  type="button"
                                  className="workspace-recovery-text-button"
                                  onClick={() => { void changeContinuation(snapshot, pane.pane_index, 'fresh_by_user' as PaneContinuationChoice); }}
                                  disabled={cardBusy || paneBusy}
                                >
                                  {copy.newSession}
                                </button>
                              )}
                              {state !== 'skipped' && (
                                <button
                                  type="button"
                                  className="workspace-recovery-text-button"
                                  onClick={() => { void changeContinuation(snapshot, pane.pane_index, 'skipped' as PaneContinuationChoice); }}
                                  disabled={cardBusy || paneBusy}
                                >
                                  {copy.skip}
                                </button>
                              )}
                            </div>
                          )}

                          {pickerOpen && (
                            <div className="workspace-recovery-history-picker">
                              <div className="workspace-recovery-history-heading">
                                <span>{copy.bindingHint}</span>
                                <button
                                  type="button"
                                  className="workspace-recovery-text-button"
                                  onClick={cancelHistoryPicker}
                                  disabled={historyLoading || paneBusy}
                                >
                                  {copy.cancel}
                                </button>
                              </div>
                              {historyLoading && <div className="workspace-recovery-history-empty">{copy.historyLoading}</div>}
                              {historyError && <div className="workspace-recovery-card-error">{historyError}</div>}
                              {!historyLoading && !historyError && history.length === 0 && (
                                <div className="workspace-recovery-history-empty">{copy.noHistory}</div>
                              )}
                              {!historyLoading && !historyError && history.length > 0 && (
                                <div className="workspace-recovery-history-list">
                                  {history.map((candidate) => (
                                    <button
                                      key={candidate.selection_id}
                                      type="button"
                                      className="workspace-recovery-history-item"
                                      onClick={() => { void bindHistory(snapshot, pane.pane_index, candidate); }}
                                      disabled={cardBusy || paneBusy}
                                    >
                                      <span>{candidate.name || displayTool(pane.tool, copy)}</span>
                                      <small>{savedAt(Number(candidate.saved_at), chinese)}</small>
                                    </button>
                                  ))}
                                </div>
                              )}
                            </div>
                          )}
                        </div>
                      );
                    })}
                  </div>

                  <div className="workspace-recovery-detail-actions">
                    <button
                      type="button"
                      className="workspace-recovery-text-button"
                      onClick={() => { void inspect(snapshot); }}
                      disabled={cardBusy}
                    >
                      {copy.inspect}
                    </button>
                  </div>
                </div>
              )}
            </article>
          );
        })}
      </div>
    </section>
  );
}
