import type { McpConfig, McpProfileSelection, ToolConfigEntry } from '../../tauri';
import { useT } from '../../i18n/useT';
import './mcp-launch-wrapper-warning.css';

type ExternalWrapper = 'Docker' | 'WSL';

export interface McpLaunchWrapperWarningInfo {
  tool: string;
  wrapper: ExternalWrapper;
}

interface Props {
  warning: McpLaunchWrapperWarningInfo | null;
  className?: string;
}

/**
 * Coffee splits launch overrides on whitespace before spawning. Mirror that
 * exact rule here and warn only for an unambiguous first executable. A shell
 * script may eventually invoke Docker or WSL too, but guessing would produce
 * misleading warnings for ordinary launch overrides.
 */
function externalWrapperForCommand(command: string): ExternalWrapper | null {
  const first = command.trim().split(/\s+/, 1)[0];
  if (!first) return null;

  const executable = first.replace(/\\/g, '/').split('/').pop()?.toLowerCase();
  if (executable === 'docker' || executable === 'docker.exe') return 'Docker';
  if (executable === 'wsl' || executable === 'wsl.exe') return 'WSL';
  return null;
}

function selectedExternalProfileId(
  selection: McpProfileSelection,
  config: McpConfig | null,
  tool: string,
  allowKnownAutoDefault: boolean,
): string | null {
  if (!config || selection.mode === 'none') return null;

  if (selection.mode === 'profile') {
    return config.profiles[selection.profile_id] ? selection.profile_id : null;
  }

  // A workspace binding takes precedence in Rust and path canonicalization is
  // backend-owned. Only claim to know Auto's result when no binding exists.
  if (!allowKnownAutoDefault || config.workspace_bindings.length > 0) return null;
  const profileId = config.defaults.agents[tool] ?? config.defaults.global;
  return profileId && config.profiles[profileId] ? profileId : null;
}

/**
 * Return a warning only when Coffee can prove both sides of the condition:
 * an external profile will be selected and this exact tool has a Docker/WSL
 * launch override. `None` deliberately bypasses the warning.
 */
export function getMcpLaunchWrapperWarning(
  selection: McpProfileSelection,
  config: McpConfig | null,
  tool: string,
  toolConfigs: Record<string, ToolConfigEntry>,
  options: { allowKnownAutoDefault?: boolean; toolLabel?: string } = {},
): McpLaunchWrapperWarningInfo | null {
  const profileId = selectedExternalProfileId(
    selection,
    config,
    tool,
    options.allowKnownAutoDefault ?? true,
  );
  if (!profileId) return null;

  const profile = config?.profiles[profileId];
  // An empty profile results in no external MCP connection attempt, so it
  // should not create noise merely because the launch command is wrapped.
  if (!profile?.servers.length) return null;

  const wrapper = externalWrapperForCommand(toolConfigs[tool]?.command ?? '');
  return wrapper ? { tool: options.toolLabel ?? tool, wrapper } : null;
}

export function McpLaunchWrapperWarning({ warning, className = '' }: Props) {
  const t = useT();
  if (!warning) return null;

  return (
    <span className={`mcp-launch-wrapper-warning ${className}`.trim()} role="status">
      <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round" aria-hidden="true">
        <path d="M10.3 2.9 1.8 18a2 2 0 0 0 1.7 3h17a2 2 0 0 0 1.7-3L13.7 2.9a2 2 0 0 0-3.4 0Z" />
        <path d="M12 9v4" />
        <path d="M12 17h.01" />
      </svg>
      <span>{t('mcp.wrapper.warning', { tool: warning.tool, wrapper: warning.wrapper })}</span>
    </span>
  );
}
