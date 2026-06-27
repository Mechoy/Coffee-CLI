// AccountFooter.tsx — bottom-left account / login entry for the left panel.
//
// First stone of the "login version" (branch: feat/account). Standard
// location: pinned to the bottom of the left sidebar, below the workspace
// tree and session history — the same place VS Code and most apps put the
// account control.
//
// Today it only renders the logged-out entry (avatar glyph + "登录"). The
// real auth flow and account state attach at `handleClick` once the auth
// backend is decided (relay+account vs LAN pairing — see the remote-dev
// discussion). The avatar is an INLINE SVG (never an external URL) on
// purpose: the old hidden profile header in TaskBoard fetched an external
// pravatar image on every launch, which we don't want to repeat.

import { useState } from 'react';
import { useT } from '../../i18n/useT';
import './AccountFooter.css';

// Placeholder account shape. Real state will come from the auth backend /
// app store once the login mechanism is chosen; kept local + null for now so
// this slot doesn't commit to a backend prematurely.
interface Account {
  name: string;
  avatarUrl?: string;
}

export function AccountFooter() {
  const t = useT();

  // Always logged-out for now. TODO(feat/account): replace this local state
  // with real auth state from the backend / app store once login lands.
  const [account] = useState<Account | null>(null);

  const handleClick = () => {
    // TODO(feat/account): open the login flow here once the auth backend
    // is decided. Intentionally a no-op stub for the first scaffolding pass.
  };

  const label = account ? account.name : (t('account.login' as any) || '登录');

  return (
    <div className="account-footer">
      <button className="account-row" onClick={handleClick} aria-label={label}>
        <span className="account-avatar">
          {account?.avatarUrl ? (
            <img src={account.avatarUrl} alt="" />
          ) : (
            <svg width="20" height="20" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round">
              <circle cx="12" cy="8" r="4" />
              <path d="M4 20c0-3.3 3.6-6 8-6s8 2.7 8 6" />
            </svg>
          )}
        </span>
        <span className="account-label">{label}</span>
      </button>
    </div>
  );
}
