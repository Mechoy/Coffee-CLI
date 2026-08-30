import { useCallback, useEffect, useMemo, useRef, useState } from 'react';
import {
  commands,
  type CoffeeSkill,
  type NativeSkillStatus,
  type NativeSkillStatusKind,
  type NativeSkillTarget,
  type SkillsOverview,
} from '../../tauri';
import { useT } from '../../i18n/useT';
import './skills.css';

const TARGETS: Array<{ id: NativeSkillTarget; label: string }> = [
  { id: 'codex', label: 'Codex' },
  { id: 'claude', label: 'Claude Code' },
];

const MAX_BODY_BYTES = 32 * 1024;

const emptySkill = (): CoffeeSkill => ({
  name: '',
  description: '',
  body: '',
});

function idFromName(name: string): string {
  return name
    .trim()
    .toLowerCase()
    .replace(/[^a-z0-9]+/g, '-')
    .replace(/^-+|-+$/g, '')
    .replace(/^[^a-z]+/, '')
    .replace(/-+/g, '-')
    .slice(0, 63);
}

function isValidSkillId(value: string): boolean {
  return /^[a-z][a-z0-9]*(?:-[a-z0-9]+)*$/.test(value) && value.length <= 63;
}

function bodyByteLength(value: string): number {
  return new TextEncoder().encode(value).length;
}

function statusLabel(t: ReturnType<typeof useT>, state: NativeSkillStatusKind): string {
  return t(`skills.state.${state}` as any);
}

function statusClass(state: NativeSkillStatusKind): string {
  if (state === 'enabled_linked' || state === 'enabled_copied') return 'ready';
  if (state === 'needs_sync') return 'attention';
  if (state === 'conflict' || state === 'drift' || state === 'source_missing' || state === 'error') return 'error';
  return 'muted';
}

function targetAction(state: NativeSkillStatusKind): 'enable' | 'disable' | 'sync' | null {
  if (state === 'disabled') return 'enable';
  if (state === 'needs_sync') return 'sync';
  if (state === 'enabled_linked' || state === 'enabled_copied' || state === 'drift') return 'disable';
  return null;
}

function statusTitle(t: ReturnType<typeof useT>, status: NativeSkillStatus): string | undefined {
  if (status.detail) return status.detail;
  return status.state === 'enabled_linked' || status.state === 'enabled_copied'
    ? t('skills.future.sessions' as any)
    : undefined;
}

export function SkillsSettings() {
  const t = useT();
  const [overview, setOverview] = useState<SkillsOverview | null>(null);
  const [selectedId, setSelectedId] = useState<string | null>(null);
  const [draftId, setDraftId] = useState('');
  const [draft, setDraft] = useState<CoffeeSkill | null>(null);
  const [error, setError] = useState('');
  const [loading, setLoading] = useState(true);
  const [busy, setBusy] = useState<string | null>(null);

  // Keep these values in refs so an asynchronous overview response always
  // judges freshness against the draft that exists when it resolves, rather
  // than the state captured when the request began.
  const mountedRef = useRef(true);
  const overviewLoadGenerationRef = useRef(0);
  const selectedIdRef = useRef<string | null>(null);
  const draftDirtyRef = useRef(false);
  const draftBaselineRevisionRef = useRef<number | null>(null);

  const setSelected = (id: string | null) => {
    selectedIdRef.current = id;
    setSelectedId(id);
  };

  const markDraftClean = (revision: number | null) => {
    draftDirtyRef.current = false;
    draftBaselineRevisionRef.current = revision;
  };

  const markDraftDirty = () => {
    draftDirtyRef.current = true;
  };

  const invalidatePendingLoads = () => {
    overviewLoadGenerationRef.current += 1;
    setLoading(false);
  };

  const applyOverview = useCallback((next: SkillsOverview) => {
    setOverview(next);
    const currentId = selectedIdRef.current;
    if (!currentId) return;

    const latestSkill = next.config.skills[currentId];
    if (!latestSkill) {
      setSelected(null);
      if (!draftDirtyRef.current) {
        setDraftId('');
        setDraft(null);
        markDraftClean(null);
      }
      return;
    }

    // A clean draft is merely a rendered copy of persisted data, so it can
    // safely follow a refresh. A dirty draft keeps its original revision: a
    // later save must conflict instead of silently overwriting another window.
    if (!draftDirtyRef.current) {
      setDraftId(currentId);
      setDraft(structuredClone(latestSkill));
      markDraftClean(next.config.revision);
    }
  }, []);

  const load = useCallback(async () => {
    const generation = ++overviewLoadGenerationRef.current;
    setLoading(true);
    setError('');
    try {
      const next = await commands.getSkillsOverview();
      if (!mountedRef.current || generation !== overviewLoadGenerationRef.current) return;
      applyOverview(next);
    } catch (reason) {
      if (!mountedRef.current || generation !== overviewLoadGenerationRef.current) return;
      setError(String(reason));
    } finally {
      if (mountedRef.current && generation === overviewLoadGenerationRef.current) {
        setLoading(false);
      }
    }
  }, [applyOverview]);

  useEffect(() => {
    mountedRef.current = true;
    void load();
    return () => {
      mountedRef.current = false;
      overviewLoadGenerationRef.current += 1;
    };
  }, [load]);

  const entries = useMemo(
    () => Object.entries(overview?.config.skills ?? {}),
    [overview],
  );

  const selectSkill = (id: string, skill: CoffeeSkill) => {
    setSelected(id);
    setDraftId(id);
    setDraft(structuredClone(skill));
    markDraftClean(overview?.config.revision ?? null);
    setError('');
  };

  const newSkill = () => {
    setSelected(null);
    setDraftId('');
    setDraft(emptySkill());
    markDraftClean(overview?.config.revision ?? null);
    setError('');
  };

  const updateDraft = (next: CoffeeSkill) => {
    markDraftDirty();
    setDraft(next);
  };

  const saveSkill = async () => {
    if (!overview || !draft) return;
    const id = (selectedId ?? draftId.trim()) || idFromName(draft.name);
    if (!isValidSkillId(id)) {
      setError(t('skills.id.required' as any));
      return;
    }
    if (bodyByteLength(draft.body) > MAX_BODY_BYTES) return;
    invalidatePendingLoads();
    setBusy('save');
    setError('');
    try {
      // Deliberately use the revision from which this draft was made. A
      // refreshed overview must not grant an old edited draft permission to
      // overwrite an external change.
      const expectedRevision = draftBaselineRevisionRef.current ?? overview.config.revision;
      const next = await commands.saveCoffeeSkill(expectedRevision, id, draft);
      applyOverview(next);
      setSelected(id);
      setDraftId(id);
      setDraft(structuredClone(next.config.skills[id]));
      markDraftClean(next.config.revision);
    } catch (reason) {
      setError(String(reason));
    } finally {
      setBusy(null);
    }
  };

  const deleteSkill = async () => {
    if (!overview || !selectedId) return;
    if (!window.confirm(t('skills.delete.confirm' as any))) return;
    invalidatePendingLoads();
    setBusy('delete');
    setError('');
    try {
      const expectedRevision = draftBaselineRevisionRef.current ?? overview.config.revision;
      const next = await commands.deleteCoffeeSkill(expectedRevision, selectedId);
      applyOverview(next);
      setSelected(null);
      setDraftId('');
      setDraft(null);
      markDraftClean(null);
    } catch (reason) {
      setError(String(reason));
    } finally {
      setBusy(null);
    }
  };

  const changeTarget = async (target: NativeSkillTarget, enabled: boolean) => {
    if (!selectedId) return;
    const key = `target-${target}`;
    invalidatePendingLoads();
    setBusy(key);
    setError('');
    try {
      applyOverview(await commands.setNativeSkillEnabled(selectedId, target, enabled));
    } catch (reason) {
      setError(String(reason));
    } finally {
      setBusy(null);
    }
  };

  if (loading && !overview) return <div className="skills-empty">{t('skills.loading' as any)}</div>;

  const statusFor = (target: NativeSkillTarget): NativeSkillStatus =>
    selectedId
      ? overview?.statuses[selectedId]?.[target] ?? { state: 'disabled' }
      : { state: 'disabled' };

  const bodyBytes = draft ? bodyByteLength(draft.body) : 0;
  const bodyTooLarge = bodyBytes > MAX_BODY_BYTES;
  const interactionLocked = loading || busy !== null;

  return (
    <div className="skills-settings">
      <div className="skills-heading">
        <span>{t('skills.personal' as any)}</span>
        <button type="button" className="skills-refresh" onClick={() => void load()} disabled={interactionLocked}>
          {t('skills.refresh' as any)}
        </button>
      </div>
      {error && <div className="skills-error" role="alert"><span>{error}</span><button type="button" disabled={interactionLocked} onClick={() => void load()}>{t('skills.reload' as any)}</button></div>}
      <div className="skills-workspace">
        <aside className="skills-list">
          <button type="button" className="skills-primary-action" onClick={newSkill} disabled={interactionLocked}>
            <span aria-hidden="true">+</span>{t('skills.add' as any)}
          </button>
          {entries.map(([id, skill]) => {
            const targetStates = TARGETS.map(target => overview?.statuses[id]?.[target.id]?.state ?? 'disabled');
            const hasEnabled = targetStates.some(state => state === 'enabled_linked' || state === 'enabled_copied');
            const hasIssue = targetStates.some(state => ['needs_sync', 'conflict', 'drift', 'source_missing', 'error'].includes(state));
            return (
              <button type="button" key={id} className={`skills-list-row${selectedId === id ? ' active' : ''}`} disabled={interactionLocked} onClick={() => selectSkill(id, skill)}>
                <span className={`skills-status-dot${hasIssue ? ' issue' : hasEnabled ? '' : ' muted'}`} />
                <span><strong>{skill.name}</strong><small>{id}</small></span>
              </button>
            );
          })}
        </aside>
        <section className="skills-editor">
          {!draft ? <div className="skills-empty">{t('skills.empty' as any)}</div> : <>
            <div className="skills-form-grid">
              <label>{t('skills.id' as any)}<input value={selectedId ?? draftId} disabled={selectedId !== null || interactionLocked} onChange={event => { markDraftDirty(); setDraftId(event.target.value); }} placeholder="research-summary" /></label>
              <label>{t('skills.name' as any)}<input value={draft.name} disabled={interactionLocked} onChange={event => updateDraft({ ...draft, name: event.target.value })} placeholder={t('skills.name.placeholder' as any)} /></label>
            </div>
            <label className="skills-field">{t('skills.description' as any)}<input value={draft.description} disabled={interactionLocked} maxLength={512} onChange={event => updateDraft({ ...draft, description: event.target.value })} placeholder={t('skills.description.placeholder' as any)} /></label>
            <label className="skills-field skills-body-field"><span>{t('skills.body' as any)}<small className={bodyTooLarge ? 'over-limit' : ''}>{bodyBytes} / {MAX_BODY_BYTES}</small></span><textarea value={draft.body} disabled={interactionLocked} className={bodyTooLarge ? 'over-limit' : ''} aria-invalid={bodyTooLarge} onChange={event => updateDraft({ ...draft, body: event.target.value })} placeholder={t('skills.body.placeholder' as any)} spellCheck={false} /></label>
            {selectedId && <div className="skills-targets">
              {TARGETS.map(target => {
                const status = statusFor(target.id);
                const action = targetAction(status.state);
                const actionLabel = action === 'enable'
                  ? t('skills.enable' as any)
                  : action === 'disable'
                    ? t('skills.disable' as any)
                    : action === 'sync'
                      ? t('skills.sync' as any)
                      : null;
                const mutationKey = `target-${target.id}`;
                return <div className="skills-target-row" key={target.id}>
                  <span className="skills-target-name">{target.label}</span>
                  <span className={`skills-state ${statusClass(status.state)}`} title={statusTitle(t, status)}>{statusLabel(t, status.state)}</span>
                  {actionLabel && <button type="button" className={action === 'disable' ? 'skills-target-danger' : 'skills-target-action'} disabled={interactionLocked} onClick={() => void changeTarget(target.id, action !== 'disable')}>{busy === mutationKey ? t('skills.working' as any) : actionLabel}</button>}
                </div>;
              })}
            </div>}
            <div className="skills-editor-actions">
              {selectedId && <button type="button" className="skills-danger-action" disabled={interactionLocked} onClick={() => void deleteSkill()}>{t('skills.delete' as any)}</button>}
              <button type="button" className="skills-save-action" disabled={interactionLocked || bodyTooLarge} onClick={() => void saveSkill()}>{busy === 'save' ? t('skills.working' as any) : t('skills.save' as any)}</button>
            </div>
          </>}
        </section>
      </div>
    </div>
  );
}
