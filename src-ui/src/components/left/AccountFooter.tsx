// AccountFooter.tsx — bottom-left account / login entry + remote-host menu.
//
// Branch: feat/account. CLICKABLE DEMO with hardcoded data so the full UX
// (login → account bar → expand-up host list → hover tools submenu → logout)
// is testable without any backend. Everything marked DEMO below is throwaway:
// real auth state + the server-returned host list + the remote tool launch
// replace it later. The remote tool list / icons will come from the remote
// machine's own Coffee CLI detection × our bundled icons (see project notes).

import { useState } from 'react';
import { useT } from '../../i18n/useT';
import './AccountFooter.css';

interface Account {
  name: string;
  email: string;
}

type OS = 'windows' | 'macos' | 'linux' | 'local';

interface ToolItem {
  id: string;
  label: string;
}

interface RemoteHost {
  id: string;
  addr: string;
  os: OS;
  online: boolean;
  current?: boolean;
  tools?: ToolItem[];
}

// ── DEMO DATA (feat/account) — replace with real auth + server host list ──
const DEMO_ACCOUNT: Account = { name: 'QiYu', email: 'qiyu@coffeecli.com' };
const DEMO_HOSTS: RemoteHost[] = [
  {
    id: 'h1',
    addr: 'root@110.11.110.110',
    os: 'windows',
    online: true,
    tools: [
      { id: 'claude', label: 'Claude Code' },
      { id: 'codex', label: 'Codex' },
      { id: 'terminal', label: '终端' },
    ],
  },
  { id: 'h2', addr: 'root@120.12.120.120', os: 'macos', online: false },
  { id: 'local', addr: '本机 123.123.123.123', os: 'local', online: true, current: true },
];

function OsIcon({ os }: { os: OS }) {
  if (os === 'windows') {
    return (
      <svg className="os" viewBox="0 0 24 24" fill="currentColor"><rect x="3" y="3" width="8.2" height="8.2" rx=".5" /><rect x="12.8" y="3" width="8.2" height="8.2" rx=".5" /><rect x="3" y="12.8" width="8.2" height="8.2" rx=".5" /><rect x="12.8" y="12.8" width="8.2" height="8.2" rx=".5" /></svg>
    );
  }
  if (os === 'macos') {
    return (
      <svg className="os" viewBox="0 0 24 24" fill="currentColor"><path d="M17.05 12.5c-.03-2.6 2.13-3.85 2.22-3.91-1.21-1.77-3.1-2.01-3.77-2.04-1.6-.16-3.13.94-3.94.94-.81 0-2.07-.92-3.4-.9-1.75.03-3.36 1.02-4.26 2.58-1.82 3.16-.47 7.83 1.3 10.39.86 1.25 1.89 2.66 3.24 2.61 1.3-.05 1.79-.84 3.36-.84 1.57 0 2.01.84 3.39.81 1.4-.02 2.29-1.28 3.15-2.54.55-.8.9-1.5 1.1-2-.03-.01-2.7-1.05-2.73-4.15zM14.6 4.84c.72-.87 1.2-2.08 1.07-3.28-1.03.04-2.28.69-3.02 1.56-.66.77-1.24 2-1.08 3.18 1.15.09 2.32-.59 3.03-1.46z" /></svg>
    );
  }
  if (os === 'linux') {
    return (
      <svg className="os" viewBox="0 0 24 24" fill="currentColor"><path d="M12 2.2c-1.9 0-2.9 1.6-2.9 3.6 0 .8.1 1.5.1 2.1 0 .8-1.5 2-2.3 4.1-.9 2.1-1.6 3.7-1 4.4.3.4.8.2 1.1.5.3.4.2 1.1.9 1.5.8.4 3.4.7 5.1.7s4.3-.3 5.1-.7c.7-.4.6-1.1.9-1.5.3-.3.8-.1 1.1-.5.6-.7-.1-2.3-1-4.4-.8-2.1-2.3-3.3-2.3-4.1 0-.6.1-1.3.1-2.1 0-2-1-3.6-2.9-3.6z" /></svg>
    );
  }
  // local — this machine
  return (
    <svg className="os" viewBox="0 0 24 24" fill="currentColor"><circle cx="12" cy="8" r="4" /><path d="M4 20c0-3.3 3.6-6 8-6s8 2.7 8 6z" /></svg>
  );
}

function ToolIcon({ id }: { id: string }) {
  if (id === 'claude') {
    return <svg className="ti-ic" viewBox="0 0 24 24" fill="#d97757"><path d="M12 2l1.6 6.2L20 6l-4.6 4.6L22 12l-6.4 1.4L20 18l-6.4-2.2L12 22l-1.6-6.2L4 18l4.6-4.6L2 12l6.4-1.4L4 6l6.4 2.2z" /></svg>;
  }
  if (id === 'codex') {
    return <svg className="ti-ic" viewBox="0 0 24 24" fill="none" stroke="#4a9eff" strokeWidth="2"><circle cx="12" cy="12" r="9" /><path d="M8.5 12.5l2.5 2.5 4.5-5" /></svg>;
  }
  return <svg className="ti-ic" viewBox="0 0 24 24" fill="none" stroke="#4a9eff" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round"><rect x="2.5" y="4" width="19" height="16" rx="2.5" /><path d="M7 9.5l3 2.5-3 2.5M13 15h4" /></svg>;
}

export function AccountFooter() {
  const t = useT();
  // DEMO: starts logged-out; clicking 登录 fake-logs-in. Real auth replaces this.
  const [account, setAccount] = useState<Account | null>(null);
  const [open, setOpen] = useState(false);

  const handleFooterClick = () => {
    if (!account) {
      // DEMO fake-login. TODO(feat/account): open the real login flow.
      setAccount(DEMO_ACCOUNT);
      return;
    }
    setOpen((v) => !v);
  };

  const handleLogout = () => {
    setAccount(null);
    setOpen(false);
  };

  const handleLaunch = (_host: RemoteHost, _tool: ToolItem) => {
    // TODO(feat/account): launch the remote tool here. Demo just closes.
    setOpen(false);
  };

  return (
    <div className="account-footer">
      {open && account && (
        <>
          <div className="account-backdrop" onClick={() => setOpen(false)} />
          <div className="host-menu" role="menu">
            {DEMO_HOSTS.map((host) => {
              const canExpand = host.online && !host.current && !!host.tools?.length;
              return (
                <div key={host.id} className={`host-item${canExpand ? ' expandable' : ''}`}>
                  <div className={`host-row${host.online ? '' : ' offline'}`}>
                    <OsIcon os={host.os} />
                    <span className="host-addr">{host.addr}</span>
                    {host.current ? (
                      <span className="host-badge">当前</span>
                    ) : (
                      <>
                        <span className={`host-dot ${host.online ? 'on' : 'off'}`} />
                        <span className="host-status">{host.online ? '在线' : '离线'}</span>
                        {canExpand && (
                          <svg className="host-chev" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2.6" strokeLinecap="round" strokeLinejoin="round"><path d="M9 18l6-6-6-6" /></svg>
                        )}
                      </>
                    )}
                  </div>
                  {canExpand && (
                    <div className="host-submenu">
                      {host.tools!.map((tool, i) => (
                        <button key={tool.id} className={`tool-item${i === 0 ? ' first' : ''}`} onClick={() => handleLaunch(host, tool)}>
                          <ToolIcon id={tool.id} />
                          <span>{tool.label}</span>
                          {i === 0 && <span className="tool-run">启动</span>}
                        </button>
                      ))}
                    </div>
                  )}
                </div>
              );
            })}
            <div className="host-divider" />
            <button className="host-logout" onClick={handleLogout}>
              <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round"><path d="M9 21H5a2 2 0 0 1-2-2V5a2 2 0 0 1 2-2h4M16 17l5-5-5-5M21 12H9" /></svg>
              退出登录
            </button>
          </div>
        </>
      )}

      <button className={`account-row${open ? ' is-open' : ''}`} onClick={handleFooterClick}>
        {account ? (
          <>
            <span className="account-avatar logged">{account.name.charAt(0).toUpperCase()}</span>
            <span className="account-id">
              <b>{account.name}</b>
              <small>{account.email}</small>
            </span>
            <svg className="account-chev" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2.2" strokeLinecap="round" strokeLinejoin="round"><path d="M18 15l-6-6-6 6" /></svg>
          </>
        ) : (
          <>
            <span className="account-avatar">
              <svg width="20" height="20" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round"><circle cx="12" cy="8" r="4" /><path d="M4 20c0-3.3 3.6-6 8-6s8 2.7 8 6" /></svg>
            </span>
            <span className="account-label">{t('account.login' as any) || '登录'}</span>
          </>
        )}
      </button>
    </div>
  );
}
