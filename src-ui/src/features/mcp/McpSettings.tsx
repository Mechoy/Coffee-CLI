import { useEffect, useMemo, useState } from 'react';
import { commands, type McpConfig, type McpEnvRef, type McpProfile, type McpServerDefinition } from '../../tauri';
import { useT } from '../../i18n/useT';
import './mcp.css';

type View = 'servers' | 'profiles';

const emptyConfig = (): McpConfig => ({
  version: 1,
  revision: 0,
  servers: {},
  profiles: {},
  defaults: { global: null, agents: {} },
  workspace_bindings: [],
  multi_agent_bindings: [],
});

const emptyServer = (): McpServerDefinition => ({
  name: '',
  enabled: true,
  transport: { type: 'stdio', command: '', args: [], env: {} },
});

function idFromName(name: string): string {
  return name.trim().toLowerCase().replace(/[^a-z0-9_-]+/g, '-').replace(/^[^a-z]+/, '').slice(0, 64);
}

function ProfileSelect({ value, profiles, onChange, allowNone = true, emptyLabel }: {
  value: string | null;
  profiles: Record<string, McpProfile>;
  onChange: (value: string | null) => void;
  allowNone?: boolean;
  emptyLabel?: string;
}) {
  const t = useT();
  return (
    <select className="mcp-select" value={value ?? ''} onChange={event => onChange(event.target.value || null)}>
      {allowNone && <option value="">{emptyLabel ?? t('mcp.none' as any)}</option>}
      {Object.entries(profiles).map(([id, profile]) => <option key={id} value={id}>{profile.name}</option>)}
    </select>
  );
}

function PairRows({ value, onChange, leftPlaceholder, rightPlaceholder }: {
  value: Record<string, McpEnvRef>;
  onChange: (value: Record<string, McpEnvRef>) => void;
  leftPlaceholder: string;
  rightPlaceholder: string;
}) {
  const rows = Object.entries(value);
  const replace = (index: number, key: string, fromEnv: string) => {
    const next: Record<string, McpEnvRef> = {};
    rows.forEach(([oldKey, oldRef], rowIndex) => {
      const target = rowIndex === index ? key : oldKey;
      const source = rowIndex === index ? fromEnv : oldRef.from_env;
      if (target) next[target] = { from_env: source };
    });
    onChange(next);
  };
  return (
    <div className="mcp-pairs">
      {rows.map(([key, ref], index) => (
        <div className="mcp-pair-row" key={`${key}-${index}`}>
          <input value={key} placeholder={leftPlaceholder} onChange={event => replace(index, event.target.value, ref.from_env)} />
          <span aria-hidden="true">←</span>
          <input value={ref.from_env} placeholder={rightPlaceholder} onChange={event => replace(index, key, event.target.value)} />
          <button type="button" className="mcp-icon-btn danger" title="Remove" onClick={() => onChange(Object.fromEntries(rows.filter((_, rowIndex) => rowIndex !== index)))}>
            <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2"><path d="M3 6h18M8 6V4h8v2m-9 0 1 14h8l1-14M10 10v6m4-6v6" /></svg>
          </button>
        </div>
      ))}
      <button type="button" className="mcp-add-row" onClick={() => onChange({ ...value, [`NEW_${rows.length + 1}`]: { from_env: '' } })}>
        <span aria-hidden="true">+</span> Add
      </button>
    </div>
  );
}

export function McpSettings() {
  const t = useT();
  const [view, setView] = useState<View>('servers');
  const [config, setConfig] = useState<McpConfig | null>(null);
  const [error, setError] = useState('');
  const [needsReload, setNeedsReload] = useState(false);
  const [recoveryToken, setRecoveryToken] = useState<string | null>(null);
  const [saving, setSaving] = useState(false);
  const [serverId, setServerId] = useState<string | null>(null);
  const [serverDraft, setServerDraft] = useState<McpServerDefinition | null>(null);
  const [draftServerId, setDraftServerId] = useState('');
  const [profileId, setProfileId] = useState<string | null>(null);
  const [profileDraft, setProfileDraft] = useState<McpProfile | null>(null);
  const [draftProfileId, setDraftProfileId] = useState('');

  const loadConfig = async () => {
    try {
      const next = await commands.getMcpConfig();
      setConfig(next);
      setServerId(null);
      setServerDraft(null);
      setProfileId(null);
      setProfileDraft(null);
      setNeedsReload(false);
      setRecoveryToken(null);
      setError('');
    } catch (reason) {
      setConfig(emptyConfig());
      setNeedsReload(false);
      setError(String(reason));
      try {
        setRecoveryToken(await commands.getMcpConfigRecoveryToken());
      } catch {
        setRecoveryToken(null);
      }
    }
  };

  useEffect(() => { void loadConfig(); }, []);

  const reloadConfig = async () => { await loadConfig(); };

  const persist = async (next: McpConfig) => {
    setSaving(true);
    setError('');
    setNeedsReload(false);
    try {
      setConfig(await commands.saveMcpConfig(next));
    } catch (reason) {
      const message = String(reason);
      setError(message);
      setNeedsReload(message.includes('reload it before saving'));
      throw reason;
    } finally {
      setSaving(false);
    }
  };

  const resetInvalidConfig = async () => {
    if (!recoveryToken) return;
    setSaving(true);
    setError('');
    setNeedsReload(false);
    try {
      const next = await commands.resetInvalidMcpConfig(recoveryToken);
      setConfig(next);
      setServerId(null);
      setServerDraft(null);
      setProfileId(null);
      setProfileDraft(null);
      setRecoveryToken(null);
    } catch (reason) {
      const message = String(reason);
      setError(message);
      setNeedsReload(message.includes('reload it before'));
    } finally {
      setSaving(false);
    }
  };

  const serverEntries = useMemo(() => Object.entries(config?.servers ?? {}), [config]);
  const profileEntries = useMemo(() => Object.entries(config?.profiles ?? {}), [config]);

  if (!config) return <div className="mcp-empty">{t('mcp.loading' as any)}</div>;

  const editServer = (id: string, server: McpServerDefinition) => {
    setServerId(id);
    setDraftServerId(id);
    setServerDraft(structuredClone(server));
  };
  const newServer = () => {
    setServerId(null);
    setDraftServerId('');
    setServerDraft(emptyServer());
  };
  const saveServer = async () => {
    if (!serverDraft) return;
    const id = draftServerId || idFromName(serverDraft.name);
    if (!serverId && config.servers[id]) {
      setError(`${t('mcp.id.exists' as any)} ${id}`);
      return;
    }
    const servers = { ...config.servers };
    servers[id] = serverDraft;
    await persist({ ...config, servers });
    setServerId(id);
    setDraftServerId(id);
  };
  const deleteServer = async (id: string) => {
    const usedBy = profileEntries.filter(([, profile]) => profile.servers.includes(id));
    if (usedBy.length) {
      setError(`${t('mcp.server.used' as any)} ${usedBy.map(([, profile]) => profile.name).join(', ')}`);
      return;
    }
    const servers = { ...config.servers };
    delete servers[id];
    await persist({ ...config, servers });
    if (serverId === id) { setServerId(null); setServerDraft(null); }
  };

  const editProfile = (id: string, profile: McpProfile) => {
    setProfileId(id);
    setDraftProfileId(id);
    setProfileDraft(structuredClone(profile));
  };
  const newProfile = () => {
    setProfileId(null);
    setDraftProfileId('');
    setProfileDraft({ name: '', servers: [] });
  };
  const saveProfile = async () => {
    if (!profileDraft) return;
    const id = draftProfileId || idFromName(profileDraft.name);
    if (!profileId && config.profiles[id]) {
      setError(`${t('mcp.id.exists' as any)} ${id}`);
      return;
    }
    const profiles = { ...config.profiles };
    profiles[id] = profileDraft;
    await persist({ ...config, profiles });
    setProfileId(id);
    setDraftProfileId(id);
  };
  const deleteProfile = async (id: string) => {
    const profiles = { ...config.profiles };
    delete profiles[id];
    const clear = (value: string | null | undefined) => value === id ? null : value ?? null;
    const next: McpConfig = {
      ...config,
      profiles,
      defaults: {
        global: clear(config.defaults.global),
        agents: Object.fromEntries(Object.entries(config.defaults.agents).map(([agent, value]) => [agent, clear(value)])),
      },
      workspace_bindings: config.workspace_bindings.filter(binding => binding.profile !== id),
      multi_agent_bindings: config.multi_agent_bindings.map(binding => ({
        ...binding,
        panes: Object.fromEntries(Object.entries(binding.panes).filter(([, value]) => value !== id)),
      })),
    };
    await persist(next);
    if (profileId === id) { setProfileId(null); setProfileDraft(null); }
  };

  const setDefault = async (key: 'global' | 'claude' | 'codex' | 'opencode', value: string | null) => {
    const defaults = { ...config.defaults, agents: { ...config.defaults.agents } };
    if (key === 'global') {
      defaults.global = value;
    } else if (value) {
      defaults.agents[key] = value;
    } else {
      delete defaults.agents[key];
    }
    await persist({ ...config, defaults });
  };

  const addWorkspaceBinding = async () => {
    if (!profileEntries.length) return;
    const { open } = await import('@tauri-apps/plugin-dialog');
    const selected = await open({ directory: true });
    if (!selected || typeof selected !== 'string') return;
    const existing = config.workspace_bindings.find(binding => binding.workspace === selected);
    const workspace_bindings = existing
      ? config.workspace_bindings
      : [...config.workspace_bindings, { workspace: selected, profile: profileEntries[0][0] }];
    await persist({ ...config, workspace_bindings });
  };

  return (
    <div className="mcp-settings">
      <div className="mcp-tabs" role="tablist">
        <button className={view === 'servers' ? 'active' : ''} onClick={() => setView('servers')}>{t('mcp.servers' as any)}</button>
        <button className={view === 'profiles' ? 'active' : ''} onClick={() => setView('profiles')}>{t('mcp.profiles' as any)}</button>
      </div>
      {error && <div className="mcp-error" role="alert"><span>{error}</span><div className="mcp-error-actions">{recoveryToken && <button type="button" disabled={saving} onClick={() => resetInvalidConfig().catch(() => {})}>{t('mcp.repair.invalid' as any)}</button>}{needsReload && <button type="button" onClick={() => reloadConfig().catch(reason => setError(String(reason)))}>{t('mcp.reload' as any)}</button>}</div></div>}

      {view === 'servers' ? (
        <div className="mcp-workspace">
          <aside className="mcp-list">
            <button className="mcp-primary-action" onClick={newServer}><span>+</span>{t('mcp.server.add' as any)}</button>
            {serverEntries.map(([id, server]) => (
              <button key={id} className={`mcp-list-row${serverId === id ? ' active' : ''}`} onClick={() => editServer(id, server)}>
                <span className={`mcp-status-dot${server.enabled ? '' : ' disabled'}`} />
                <span><strong>{server.name}</strong><small>{id} · {server.transport.type}</small></span>
              </button>
            ))}
          </aside>
          <section className="mcp-editor">
            {!serverDraft ? <div className="mcp-empty">{t('mcp.server.empty' as any)}</div> : <>
              <div className="mcp-form-grid">
                <label>{t('mcp.id' as any)}<input value={draftServerId} disabled={serverId !== null} onChange={event => setDraftServerId(event.target.value)} placeholder="chrome" /></label>
                <label>{t('mcp.name' as any)}<input value={serverDraft.name} onChange={event => setServerDraft({ ...serverDraft, name: event.target.value })} placeholder="Chrome" /></label>
              </div>
              <div className="mcp-segmented">
                <button className={serverDraft.transport.type === 'stdio' ? 'active' : ''} onClick={() => setServerDraft({ ...serverDraft, transport: { type: 'stdio', command: '', args: [], env: {} } })}>stdio</button>
                <button className={serverDraft.transport.type === 'http' ? 'active' : ''} onClick={() => setServerDraft({ ...serverDraft, transport: { type: 'http', url: '', headers: {} } })}>HTTP</button>
              </div>
              {serverDraft.transport.type === 'stdio' ? (() => { const transport = serverDraft.transport; return <>
                <label className="mcp-field">{t('mcp.command' as any)}<input value={transport.command} onChange={event => setServerDraft({ ...serverDraft, transport: { ...transport, command: event.target.value } })} placeholder="npx" /></label>
                <div className="mcp-field"><span>{t('mcp.arguments' as any)}</span>
                  <div className="mcp-args">
                    {transport.args.map((arg, index) => <div className="mcp-arg-row" key={index}>
                      <input value={arg} onChange={event => { const args = [...transport.args]; args[index] = event.target.value; setServerDraft({ ...serverDraft, transport: { ...transport, args } }); }} />
                      <button className="mcp-icon-btn danger" title="Remove" onClick={() => setServerDraft({ ...serverDraft, transport: { ...transport, args: transport.args.filter((_, row) => row !== index) } })}>×</button>
                    </div>)}
                    <button className="mcp-add-row" onClick={() => setServerDraft({ ...serverDraft, transport: { ...transport, args: [...transport.args, ''] } })}><span>+</span> Add</button>
                  </div>
                </div>
                <div className="mcp-field"><span>{t('mcp.environment' as any)}</span><PairRows value={transport.env} onChange={env => setServerDraft({ ...serverDraft, transport: { ...transport, env } })} leftPlaceholder="TARGET_VAR" rightPlaceholder="SOURCE_VAR" /></div>
              </>; })() : (() => { const transport = serverDraft.transport; return <>
                <label className="mcp-field">URL<input value={transport.url} onChange={event => setServerDraft({ ...serverDraft, transport: { ...transport, url: event.target.value } })} placeholder="http://127.0.0.1:9876/mcp" /></label>
                <div className="mcp-field"><span>{t('mcp.headers' as any)}</span><PairRows value={transport.headers} onChange={headers => setServerDraft({ ...serverDraft, transport: { ...transport, headers } })} leftPlaceholder="Authorization" rightPlaceholder="TOKEN_ENV" /></div>
              </>; })()}
              <label className="mcp-toggle"><input type="checkbox" checked={serverDraft.enabled} onChange={event => setServerDraft({ ...serverDraft, enabled: event.target.checked })} />{t('mcp.enabled' as any)}</label>
              <div className="mcp-editor-actions">
                {serverId && <button className="mcp-danger-action" onClick={() => deleteServer(serverId)}>{t('mcp.delete' as any)}</button>}
                <button className="mcp-save-action" disabled={saving} onClick={() => saveServer().catch(() => {})}>{t('mcp.save' as any)}</button>
              </div>
            </>}
          </section>
        </div>
      ) : (
        <div className="mcp-profile-view">
          <div className="mcp-profile-main">
            <aside className="mcp-list">
              <button className="mcp-primary-action" onClick={newProfile}><span>+</span>{t('mcp.profile.add' as any)}</button>
              {profileEntries.map(([id, profile]) => <button key={id} className={`mcp-list-row${profileId === id ? ' active' : ''}`} onClick={() => editProfile(id, profile)}><span><strong>{profile.name}</strong><small>{profile.servers.length} MCP · {id}</small></span></button>)}
            </aside>
            <section className="mcp-editor">
              {!profileDraft ? <div className="mcp-empty">{t('mcp.profile.empty' as any)}</div> : <>
                <div className="mcp-form-grid">
                  <label>{t('mcp.id' as any)}<input value={draftProfileId} disabled={profileId !== null} onChange={event => setDraftProfileId(event.target.value)} placeholder="web-testing" /></label>
                  <label>{t('mcp.name' as any)}<input value={profileDraft.name} onChange={event => setProfileDraft({ ...profileDraft, name: event.target.value })} placeholder="Web Testing" /></label>
                </div>
                <div className="mcp-field"><span>{t('mcp.profile.servers' as any)}</span>
                  <div className="mcp-check-list">{serverEntries.map(([id, server]) => <label key={id}><input type="checkbox" checked={profileDraft.servers.includes(id)} onChange={event => setProfileDraft({ ...profileDraft, servers: event.target.checked ? [...profileDraft.servers, id] : profileDraft.servers.filter(value => value !== id) })} /><span>{server.name}<small>{id}</small></span></label>)}</div>
                </div>
                <div className="mcp-editor-actions">
                  {profileId && <button className="mcp-danger-action" onClick={() => deleteProfile(profileId)}>{t('mcp.delete' as any)}</button>}
                  <button className="mcp-save-action" disabled={saving} onClick={() => saveProfile().catch(() => {})}>{t('mcp.save' as any)}</button>
                </div>
              </>}
            </section>
          </div>
          <div className="mcp-defaults">
            <div><span>{t('mcp.default.global' as any)}</span><ProfileSelect value={config.defaults.global} profiles={config.profiles} onChange={value => setDefault('global', value).catch(() => {})} /></div>
            {(['claude', 'codex', 'opencode'] as const).map(agent => <div key={agent}><span>{agent}</span><ProfileSelect value={config.defaults.agents[agent] ?? null} profiles={config.profiles} emptyLabel={t('mcp.inherit' as any)} onChange={value => setDefault(agent, value).catch(() => {})} /></div>)}
          </div>
          <div className="mcp-bindings">
            <div className="mcp-bindings-head"><span>{t('mcp.workspace.bindings' as any)}</span><button onClick={() => addWorkspaceBinding().catch(reason => setError(String(reason)))}>+ {t('mcp.workspace.add' as any)}</button></div>
            {config.workspace_bindings.map((binding, index) => <div className="mcp-binding-row" key={`${binding.workspace}-${index}`}><span title={binding.workspace}>{binding.workspace}</span><ProfileSelect value={binding.profile} profiles={config.profiles} allowNone={false} onChange={value => { if (!value) return; const workspace_bindings = config.workspace_bindings.map((item, row) => row === index ? { ...item, profile: value } : item); persist({ ...config, workspace_bindings }).catch(() => {}); }} /><button className="mcp-icon-btn danger" title="Remove" onClick={() => persist({ ...config, workspace_bindings: config.workspace_bindings.filter((_, row) => row !== index) }).catch(() => {})}>×</button></div>)}
          </div>
        </div>
      )}
    </div>
  );
}
