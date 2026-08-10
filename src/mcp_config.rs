//! Coffee-managed external MCP definitions, profiles, and bindings.
//!
//! Coffee is a configuration control plane only: it resolves a profile into a
//! per-session plan, while Claude/Codex/OpenCode remain the MCP clients.

use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    path::{Path, PathBuf},
};

pub const CONFIG_VERSION: u32 = 1;
pub const INTERNAL_SERVER_ID: &str = "coffee-cli";

// stdio environment aliases are applied to the Agent CLI child process so
// Claude/Codex/OpenCode can pass them onward to the MCP subprocess. Never let
// a profile replace Coffee's own process-critical environment in that child.
const RESERVED_STDIO_TARGET_ENVS: &[&str] = &[
    "HOME",
    "USERPROFILE",
    "PATH",
    "SHELL",
    "PROMPT_COMMAND",
    "COMSPEC",
    "SYSTEMROOT",
    "CODEX_HOME",
    "OPENCODE_CONFIG",
    "OPENCODE_CONFIG_CONTENT",
    "COFFEE_AGENT_RESULTS_DIR",
    "COFFEE_MODE_CWD",
    "COFFEE_CODE_THEME_MODE",
    "COFFEE_CODE_LOCALE",
    "TERM",
    "COLORTERM",
    "NODE_OPTIONS",
    "GIT_TERMINAL_PROMPT",
];

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct McpConfig {
    #[serde(default = "config_version")]
    pub version: u32,
    /// Optimistic-concurrency token for Coffee's own configuration writers.
    /// It is deliberately separate from `version`, which is the persisted
    /// schema version and must remain stable across ordinary edits.
    #[serde(default)]
    pub revision: u64,
    #[serde(default)]
    pub servers: BTreeMap<String, McpServerDefinition>,
    #[serde(default)]
    pub profiles: BTreeMap<String, McpProfile>,
    #[serde(default)]
    pub defaults: McpDefaults,
    #[serde(default)]
    pub workspace_bindings: Vec<WorkspaceBinding>,
    #[serde(default)]
    pub multi_agent_bindings: Vec<MultiAgentBinding>,
}

impl Default for McpConfig {
    fn default() -> Self {
        Self {
            version: CONFIG_VERSION,
            revision: 0,
            servers: BTreeMap::new(),
            profiles: BTreeMap::new(),
            defaults: McpDefaults::default(),
            workspace_bindings: Vec::new(),
            multi_agent_bindings: Vec::new(),
        }
    }
}

pub fn bump_revision(config: &mut McpConfig) -> Result<(), String> {
    config.revision = config
        .revision
        .checked_add(1)
        .ok_or_else(|| "MCP configuration revision overflowed".to_string())?;
    Ok(())
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct McpServerDefinition {
    pub name: String,
    pub transport: McpTransport,
    #[serde(default = "default_true")]
    pub enabled: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum McpTransport {
    Stdio {
        command: String,
        #[serde(default)]
        args: Vec<String>,
        #[serde(default)]
        env: BTreeMap<String, McpEnvRef>,
    },
    Http {
        url: String,
        #[serde(default)]
        headers: BTreeMap<String, McpEnvRef>,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct McpEnvRef {
    pub from_env: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct McpProfile {
    pub name: String,
    pub servers: Vec<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct McpDefaults {
    #[serde(default)]
    pub global: Option<String>,
    #[serde(default)]
    pub agents: BTreeMap<String, Option<String>>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct WorkspaceBinding {
    pub workspace: String,
    pub profile: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct MultiAgentBinding {
    pub workspace: String,
    #[serde(default)]
    pub panes: BTreeMap<String, String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "mode", rename_all = "snake_case")]
pub enum McpProfileSelection {
    #[default]
    Auto,
    None,
    Profile {
        profile_id: String,
    },
}

/// A persisted multi-agent workspace binding is deliberately different from
/// a session selection. `Auto` and `None` are transient launch choices, while
/// only an explicit set or clear operation may change mcp.json.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "mode", rename_all = "snake_case")]
pub enum McpMultiAgentBindingMutation {
    Set { profile_id: String },
    Clear,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedMcpServer {
    pub id: String,
    pub definition: McpServerDefinition,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SessionMcpPlan {
    pub profile_id: Option<String>,
    pub servers: Vec<ResolvedMcpServer>,
}

fn config_version() -> u32 {
    CONFIG_VERSION
}
fn default_true() -> bool {
    true
}

pub fn config_path() -> Result<PathBuf, String> {
    dirs::home_dir()
        .map(|home| home.join(".coffee-cli").join("mcp.json"))
        .ok_or_else(|| "Could not determine the user home directory".to_string())
}

pub fn load() -> Result<McpConfig, String> {
    load_from_path(&config_path()?)
}

/// Return an optimistic-concurrency token only when the persisted MCP config
/// is currently unreadable or invalid. The Settings UI uses this before
/// offering its explicit reset action; a later recovery must present the same
/// token so it cannot replace a configuration that was fixed elsewhere.
pub fn invalid_config_recovery_token() -> Result<String, String> {
    invalid_config_recovery_token_at_path(&config_path()?)
}

/// Replace the same invalid persisted configuration observed by
/// [`invalid_config_recovery_token`] with a fresh empty configuration, after
/// preserving the malformed source as a sibling `.invalid.bak` file.
///
/// This is deliberately separate from ordinary saves: callers may only use
/// it after a failed load, and the token prevents a stale empty Settings UI
/// from overwriting a valid concurrent repair.
pub fn reset_invalid_config(expected_token: &str) -> Result<McpConfig, String> {
    reset_invalid_config_at_path(&config_path()?, expected_token)
}

/// Persist a normalized configuration and return the exact representation that
/// was written. Callers keep this return value as their UI state, so a path
/// alias never leaves the editor showing a different binding than the one the
/// launcher will resolve.
pub fn save(mut config: McpConfig) -> Result<McpConfig, String> {
    normalize_bindings(&mut config);
    save_to_path(&config_path()?, &config)?;
    Ok(config)
}

fn normalize_bindings(config: &mut McpConfig) {
    for binding in &mut config.workspace_bindings {
        if let Ok(workspace) = normalize_workspace(&binding.workspace) {
            binding.workspace = workspace;
        }
    }
    for binding in &mut config.multi_agent_bindings {
        if let Ok(workspace) = normalize_workspace(&binding.workspace) {
            binding.workspace = workspace;
        }
    }
}

fn load_from_path(path: &Path) -> Result<McpConfig, String> {
    if !path.exists() {
        return Ok(McpConfig::default());
    }
    let body = fs::read_to_string(path)
        .map_err(|error| format!("Failed to read {}: {error}", path.display()))?;
    let config: McpConfig = serde_json::from_str(&body)
        .map_err(|error| format!("Invalid MCP config {}: {error}", path.display()))?;
    validate(&config)?;
    Ok(config)
}

fn invalid_config_recovery_token_at_path(path: &Path) -> Result<String, String> {
    let (_, token) = invalid_config_recovery_snapshot_at_path(path)?;
    Ok(token)
}

fn invalid_config_recovery_snapshot_at_path(path: &Path) -> Result<(String, String), String> {
    if !path.exists() {
        return Err(
            "MCP configuration is no longer invalid; reload it before resetting".to_string(),
        );
    }
    let body = fs::read_to_string(path)
        .map_err(|error| format!("Failed to read {}: {error}", path.display()))?;
    if let Ok(config) = serde_json::from_str::<McpConfig>(&body) {
        if validate(&config).is_ok() {
            return Err(
                "MCP configuration is no longer invalid; reload it before resetting".to_string(),
            );
        }
    }
    let token = recovery_fingerprint(&body);
    Ok((body, token))
}

fn reset_invalid_config_at_path(path: &Path, expected_token: &str) -> Result<McpConfig, String> {
    let (invalid_body, actual_token) = invalid_config_recovery_snapshot_at_path(path)?;
    if actual_token != expected_token {
        return Err(
            "MCP configuration changed while it was being repaired; reload it before resetting"
                .to_string(),
        );
    }

    let backup_path = next_invalid_config_backup_path(path)?;
    fs::write(&backup_path, invalid_body).map_err(|error| {
        format!(
            "Failed to back up invalid MCP config to {}: {error}",
            backup_path.display()
        )
    })?;

    let mut config = McpConfig::default();
    bump_revision(&mut config)?;
    save_to_path(path, &config)?;
    Ok(config)
}

fn next_invalid_config_backup_path(path: &Path) -> Result<PathBuf, String> {
    let primary = path.with_extension("json.invalid.bak");
    if !primary.exists() {
        return Ok(primary);
    }
    for index in 1..10_000 {
        let candidate = path.with_extension(format!("json.invalid.{index}.bak"));
        if !candidate.exists() {
            return Ok(candidate);
        }
    }
    Err(format!(
        "Too many invalid MCP config backups beside {}",
        path.display()
    ))
}

fn recovery_fingerprint(body: &str) -> String {
    // This is an optimistic-concurrency token, not a security boundary. It
    // detects an external repair between the failed load and reset without
    // exposing the malformed contents through the UI IPC channel.
    let mut hash = 0xcbf2_9ce4_8422_2325_u64;
    for byte in body.as_bytes() {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    format!("{}-{hash:016x}", body.len())
}

fn save_to_path(path: &Path, config: &McpConfig) -> Result<(), String> {
    validate(config)?;
    let parent = path
        .parent()
        .ok_or_else(|| "MCP config has no parent directory".to_string())?;
    fs::create_dir_all(parent)
        .map_err(|error| format!("Failed to create {}: {error}", parent.display()))?;

    let mut body = serde_json::to_string_pretty(config)
        .map_err(|error| format!("Failed to serialize MCP config: {error}"))?;
    body.push('\n');
    let temp = path.with_extension("json.tmp");
    fs::write(&temp, body)
        .map_err(|error| format!("Failed to write {}: {error}", temp.display()))?;
    if let Err(error) = fs::rename(&temp, path) {
        // Windows can reject replacement renames when another process has the
        // destination open. Back up the old complete config before removing
        // it; if the second rename fails, put the backup back instead of
        // turning a transient sharing/AV error into configuration loss.
        if path.exists() {
            let backup = path.with_extension("json.bak");
            fs::copy(path, &backup).map_err(|backup_error| {
                format!(
                    "Failed to back up {} before replacement: {backup_error}",
                    path.display()
                )
            })?;
            fs::remove_file(path).map_err(|remove_error| {
                format!("Failed to replace {}: {remove_error}", path.display())
            })?;
            if let Err(rename_error) = fs::rename(&temp, path) {
                let restore = fs::rename(&backup, path);
                return match restore {
                    Ok(()) => Err(format!(
                        "Failed to replace {}: {rename_error}; restored the previous configuration",
                        path.display()
                    )),
                    Err(restore_error) => Err(format!(
                        "Failed to replace {}: {rename_error}; previous configuration remains at {} ({restore_error})",
                        path.display(),
                        backup.display()
                    )),
                };
            }
            let _ = fs::remove_file(backup);
        } else {
            return Err(format!("Failed to save {}: {error}", path.display()));
        }
    }
    Ok(())
}

pub fn validate(config: &McpConfig) -> Result<(), String> {
    if config.version != CONFIG_VERSION {
        return Err(format!(
            "Unsupported MCP config version {} (this Coffee version supports {})",
            config.version, CONFIG_VERSION
        ));
    }

    for (id, server) in &config.servers {
        validate_id(id, "server")?;
        if id == INTERNAL_SERVER_ID {
            return Err(format!(
                "'{INTERNAL_SERVER_ID}' is reserved for Coffee multi-agent coordination"
            ));
        }
        validate_name(&server.name, "server", id)?;
        validate_transport(id, &server.transport)?;
    }

    for (id, profile) in &config.profiles {
        validate_id(id, "profile")?;
        validate_name(&profile.name, "profile", id)?;
        if profile.servers.is_empty() {
            return Err(format!("Profile '{id}' must contain at least one server"));
        }
        let mut seen = BTreeSet::new();
        for server_id in &profile.servers {
            if !seen.insert(server_id) {
                return Err(format!(
                    "Profile '{id}' contains duplicate server '{server_id}'"
                ));
            }
            if !config.servers.contains_key(server_id) {
                return Err(format!(
                    "Profile '{id}' references unknown server '{server_id}'"
                ));
            }
        }
    }

    validate_optional_profile(config, config.defaults.global.as_deref(), "global default")?;
    for (agent, profile) in &config.defaults.agents {
        if !matches!(agent.as_str(), "claude" | "codex" | "opencode") {
            return Err(format!("Unsupported MCP default agent '{agent}'"));
        }
        validate_optional_profile(config, profile.as_deref(), &format!("default for {agent}"))?;
    }

    let mut workspaces = BTreeSet::new();
    for binding in &config.workspace_bindings {
        validate_binding_workspace(&binding.workspace)?;
        if !workspaces.insert(&binding.workspace) {
            return Err(format!(
                "Duplicate workspace MCP binding for '{}'",
                binding.workspace
            ));
        }
        validate_profile_ref(config, &binding.profile, "workspace binding")?;
    }

    let mut multi_workspaces = BTreeSet::new();
    for binding in &config.multi_agent_bindings {
        validate_binding_workspace(&binding.workspace)?;
        if !multi_workspaces.insert(&binding.workspace) {
            return Err(format!(
                "Duplicate multi-agent MCP binding for '{}'",
                binding.workspace
            ));
        }
        for (pane, profile) in &binding.panes {
            if !matches!(pane.as_str(), "1" | "2" | "3" | "4") {
                return Err(format!("Invalid multi-agent pane '{pane}' (expected 1..4)"));
            }
            validate_profile_ref(config, profile, "multi-agent binding")?;
        }
    }
    Ok(())
}

fn validate_id(id: &str, kind: &str) -> Result<(), String> {
    let mut chars = id.chars();
    let first = chars
        .next()
        .ok_or_else(|| format!("MCP {kind} id cannot be empty"))?;
    if !first.is_ascii_lowercase()
        || id.len() > 64
        || !chars.all(|ch| ch.is_ascii_lowercase() || ch.is_ascii_digit() || ch == '_' || ch == '-')
    {
        return Err(format!(
            "Invalid MCP {kind} id '{id}' (use lowercase letters, digits, '_' or '-', starting with a letter)"
        ));
    }
    Ok(())
}

fn validate_name(name: &str, kind: &str, id: &str) -> Result<(), String> {
    let trimmed = name.trim();
    if trimmed.is_empty() || trimmed.chars().count() > 100 {
        return Err(format!(
            "MCP {kind} '{id}' must have a name between 1 and 100 characters"
        ));
    }
    Ok(())
}

fn validate_transport(id: &str, transport: &McpTransport) -> Result<(), String> {
    match transport {
        McpTransport::Stdio { command, args, env } => {
            if command.trim().is_empty() || command.contains('\0') {
                return Err(format!("MCP server '{id}' has an invalid stdio command"));
            }
            if args.iter().any(|arg| arg.contains('\0')) {
                return Err(format!(
                    "MCP server '{id}' contains a NUL byte in stdio args"
                ));
            }
            for (target, source) in env {
                validate_env_name(target, &format!("MCP server '{id}' target env"))?;
                if RESERVED_STDIO_TARGET_ENVS
                    .iter()
                    .any(|reserved| target.eq_ignore_ascii_case(reserved))
                {
                    return Err(format!(
                        "MCP server '{id}' cannot override reserved target environment variable '{target}'"
                    ));
                }
                validate_env_name(&source.from_env, &format!("MCP server '{id}' source env"))?;
            }
        }
        McpTransport::Http { url, headers } => {
            let parsed = tauri::Url::parse(url)
                .map_err(|error| format!("MCP server '{id}' has an invalid URL: {error}"))?;
            if !matches!(parsed.scheme(), "http" | "https") || parsed.host_str().is_none() {
                return Err(format!(
                    "MCP server '{id}' URL must use http or https and include a host"
                ));
            }
            if !parsed.username().is_empty() || parsed.password().is_some() {
                return Err(format!(
                    "MCP server '{id}' URL must not include credentials; use an environment-backed HTTP header instead"
                ));
            }
            for (header, source) in headers {
                if header.trim().is_empty() || header.chars().any(|ch| ch.is_control() || ch == ':')
                {
                    return Err(format!("MCP server '{id}' has an invalid HTTP header name"));
                }
                validate_env_name(&source.from_env, &format!("MCP server '{id}' header env"))?;
            }
        }
    }
    Ok(())
}

fn validate_env_name(name: &str, context: &str) -> Result<(), String> {
    let mut chars = name.chars();
    let Some(first) = chars.next() else {
        return Err(format!("{context} cannot be empty"));
    };
    if !(first.is_ascii_alphabetic() || first == '_')
        || !chars.all(|ch| ch.is_ascii_alphanumeric() || ch == '_')
    {
        return Err(format!(
            "{context} '{name}' is not a valid environment variable name"
        ));
    }
    Ok(())
}

fn validate_binding_workspace(workspace: &str) -> Result<(), String> {
    if workspace.trim().is_empty() || !Path::new(workspace).is_absolute() {
        return Err(format!(
            "MCP workspace binding must use an absolute path: '{workspace}'"
        ));
    }
    Ok(())
}

fn validate_optional_profile(
    config: &McpConfig,
    id: Option<&str>,
    context: &str,
) -> Result<(), String> {
    if let Some(id) = id {
        validate_profile_ref(config, id, context)?;
    }
    Ok(())
}

fn validate_profile_ref(config: &McpConfig, id: &str, context: &str) -> Result<(), String> {
    if config.profiles.contains_key(id) {
        Ok(())
    } else {
        Err(format!("Unknown MCP profile '{id}' in {context}"))
    }
}

pub fn normalize_workspace(workspace: &str) -> Result<String, String> {
    let path = Path::new(workspace);
    if !path.is_absolute() {
        return Err(format!("Workspace path must be absolute: '{workspace}'"));
    }
    let canonical = path
        .canonicalize()
        .map_err(|error| format!("Could not resolve workspace '{workspace}': {error}"))?;
    let rendered = canonical.to_string_lossy().to_string();
    #[cfg(target_os = "windows")]
    let rendered = rendered
        .strip_prefix(r"\\?\")
        .unwrap_or(&rendered)
        .to_string();
    Ok(rendered)
}

/// Return the saved external MCP profile for one multi-agent pane. This uses
/// the same workspace normalization as launch-time resolution, so the UI does
/// not misreport a binding when a workspace was opened through a path alias.
pub fn multi_agent_binding_profile(
    config: &McpConfig,
    workspace: &str,
    pane: u8,
) -> Result<Option<String>, String> {
    if !(1..=4).contains(&pane) {
        return Err("MCP pane binding must target pane 1..4".to_string());
    }
    validate(config)?;
    let workspace = normalize_workspace(workspace)?;
    Ok(config
        .multi_agent_bindings
        .iter()
        .find(|binding| binding.workspace == workspace)
        .and_then(|binding| binding.panes.get(&pane.to_string()))
        .cloned())
}

/// Apply an intentional saved-binding edit. Session-level `Auto` and `None`
/// never reach this boundary, which prevents a temporary choice from erasing
/// a workspace + pane binding.
pub fn apply_multi_agent_binding_mutation(
    config: &mut McpConfig,
    workspace: &str,
    pane: u8,
    mutation: McpMultiAgentBindingMutation,
) -> Result<bool, String> {
    if !(1..=4).contains(&pane) {
        return Err("MCP pane binding must target pane 1..4".to_string());
    }
    validate(config)?;
    let workspace = normalize_workspace(workspace)?;
    let pane = pane.to_string();

    match mutation {
        McpMultiAgentBindingMutation::Set { profile_id } => {
            if !config.profiles.contains_key(&profile_id) {
                return Err(format!("Unknown MCP profile '{profile_id}'"));
            }
            if let Some(binding) = config
                .multi_agent_bindings
                .iter_mut()
                .find(|binding| binding.workspace == workspace)
            {
                if binding
                    .panes
                    .get(&pane)
                    .is_some_and(|current| current == &profile_id)
                {
                    return Ok(false);
                }
                binding.panes.insert(pane, profile_id);
                return Ok(true);
            }
            config.multi_agent_bindings.push(MultiAgentBinding {
                workspace,
                panes: BTreeMap::from([(pane, profile_id)]),
            });
            Ok(true)
        }
        McpMultiAgentBindingMutation::Clear => {
            let Some(index) = config
                .multi_agent_bindings
                .iter()
                .position(|binding| binding.workspace == workspace)
            else {
                return Ok(false);
            };
            let removed = config.multi_agent_bindings[index]
                .panes
                .remove(&pane)
                .is_some();
            if config.multi_agent_bindings[index].panes.is_empty() {
                config.multi_agent_bindings.remove(index);
            }
            Ok(removed)
        }
    }
}

pub fn resolve_session_plan(
    config: &McpConfig,
    selection: &McpProfileSelection,
    agent: &str,
    workspace: Option<&str>,
    multi_agent_pane: Option<u8>,
) -> Result<SessionMcpPlan, String> {
    // Explicit None is a recovery escape hatch. Callers that already hold an
    // in-memory config (for example an editor with unsaved invalid fields)
    // must be able to start a session without validating or resolving it.
    if matches!(selection, McpProfileSelection::None) {
        return Ok(SessionMcpPlan::default());
    }
    validate(config)?;
    // A tab can retain a workspace path after it has been deleted or moved.
    // That must not prevent a normal Agent launch: the terminal layer will
    // preserve its established fallback-to-home behavior. It only means a
    // workspace-specific MCP binding cannot apply for this launch.
    let workspace = workspace
        .filter(|value| !value.trim().is_empty())
        .and_then(|value| normalize_workspace(value).ok());

    let profile_id = match selection {
        McpProfileSelection::Profile { profile_id } => Some(profile_id.clone()),
        McpProfileSelection::None => unreachable!("handled before config validation"),
        McpProfileSelection::Auto => {
            if let Some(pane) = multi_agent_pane {
                workspace.as_deref().and_then(|workspace| {
                    config
                        .multi_agent_bindings
                        .iter()
                        .find(|binding| binding.workspace == workspace)
                        .and_then(|binding| binding.panes.get(&pane.to_string()))
                        .cloned()
                })
            } else {
                workspace
                    .as_deref()
                    .and_then(|workspace| {
                        config
                            .workspace_bindings
                            .iter()
                            .find(|binding| binding.workspace == workspace)
                            .map(|binding| binding.profile.clone())
                    })
                    .or_else(|| config.defaults.agents.get(agent).cloned().flatten())
                    .or_else(|| config.defaults.global.clone())
            }
        }
    };

    let Some(profile_id) = profile_id else {
        return Ok(SessionMcpPlan::default());
    };
    let profile = config
        .profiles
        .get(&profile_id)
        .ok_or_else(|| format!("Unknown MCP profile '{profile_id}'"))?;
    validate_profile_stdio_env_targets(config, &profile_id, profile)?;
    let mut servers = Vec::with_capacity(profile.servers.len());
    for server_id in &profile.servers {
        let definition = config.servers.get(server_id).ok_or_else(|| {
            format!("Profile '{profile_id}' references unknown server '{server_id}'")
        })?;
        if !definition.enabled {
            return Err(format!(
                "MCP server '{server_id}' is disabled but required by profile '{profile_id}'"
            ));
        }
        ensure_environment_available(server_id, &definition.transport)?;
        servers.push(ResolvedMcpServer {
            id: server_id.clone(),
            definition: definition.clone(),
        });
    }
    Ok(SessionMcpPlan {
        profile_id: Some(profile_id),
        servers,
    })
}

fn validate_profile_stdio_env_targets(
    config: &McpConfig,
    profile_id: &str,
    profile: &McpProfile,
) -> Result<(), String> {
    let mut targets: BTreeMap<String, String> = BTreeMap::new();
    for server_id in &profile.servers {
        let Some(server) = config.servers.get(server_id) else {
            continue;
        };
        let McpTransport::Stdio { env, .. } = &server.transport else {
            continue;
        };
        for (target, source) in env {
            let key = environment_target_key(target);
            if let Some(previous_source) = targets.insert(key, source.from_env.clone()) {
                if previous_source != source.from_env {
                    return Err(format!(
                        "MCP profile '{profile_id}' maps target environment variable '{target}' to both '{previous_source}' and '{}'",
                        source.from_env
                    ));
                }
            }
        }
    }
    Ok(())
}

fn environment_target_key(target: &str) -> String {
    #[cfg(target_os = "windows")]
    {
        target.to_ascii_uppercase()
    }
    #[cfg(not(target_os = "windows"))]
    {
        target.to_string()
    }
}

fn ensure_environment_available(server_id: &str, transport: &McpTransport) -> Result<(), String> {
    let refs: Vec<&McpEnvRef> = match transport {
        McpTransport::Stdio { env, .. } => env.values().collect(),
        McpTransport::Http { headers, .. } => headers.values().collect(),
    };
    for reference in refs {
        if std::env::var_os(&reference.from_env).is_none() {
            return Err(format!(
                "MCP server '{server_id}' requires environment variable '{}'",
                reference.from_env
            ));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_config(workspace: &str) -> McpConfig {
        McpConfig {
            servers: BTreeMap::from([
                (
                    "chrome".into(),
                    McpServerDefinition {
                        name: "Chrome".into(),
                        enabled: true,
                        transport: McpTransport::Stdio {
                            command: "npx".into(),
                            args: vec!["chrome-mcp".into()],
                            env: BTreeMap::new(),
                        },
                    },
                ),
                (
                    "burp".into(),
                    McpServerDefinition {
                        name: "Burp".into(),
                        enabled: true,
                        transport: McpTransport::Http {
                            url: "http://127.0.0.1:9876/mcp".into(),
                            headers: BTreeMap::new(),
                        },
                    },
                ),
            ]),
            profiles: BTreeMap::from([(
                "web".into(),
                McpProfile {
                    name: "Web".into(),
                    servers: vec!["chrome".into(), "burp".into()],
                },
            )]),
            defaults: McpDefaults {
                global: Some("web".into()),
                agents: BTreeMap::new(),
            },
            workspace_bindings: vec![WorkspaceBinding {
                workspace: workspace.into(),
                profile: "web".into(),
            }],
            multi_agent_bindings: vec![MultiAgentBinding {
                workspace: workspace.into(),
                panes: BTreeMap::from([("2".into(), "web".into())]),
            }],
            ..McpConfig::default()
        }
    }

    #[test]
    fn validates_ids_and_references() {
        let root = std::env::current_dir().unwrap().canonicalize().unwrap();
        let config = sample_config(&root.to_string_lossy());
        assert!(validate(&config).is_ok());

        let mut invalid = config.clone();
        invalid
            .profiles
            .get_mut("web")
            .unwrap()
            .servers
            .push("missing".into());
        assert!(validate(&invalid)
            .unwrap_err()
            .contains("unknown server 'missing'"));
    }

    #[test]
    fn explicit_none_overrides_defaults() {
        let root = std::env::current_dir().unwrap().canonicalize().unwrap();
        let config = sample_config(&root.to_string_lossy());
        let plan = resolve_session_plan(
            &config,
            &McpProfileSelection::None,
            "codex",
            Some(&root.to_string_lossy()),
            None,
        )
        .unwrap();
        assert!(plan.profile_id.is_none());
        assert!(plan.servers.is_empty());
    }

    #[test]
    fn explicit_none_is_available_when_the_config_is_invalid() {
        let invalid = McpConfig {
            version: CONFIG_VERSION + 1,
            ..McpConfig::default()
        };
        let plan = resolve_session_plan(&invalid, &McpProfileSelection::None, "codex", None, None)
            .unwrap();
        assert!(plan.profile_id.is_none());
        assert!(plan.servers.is_empty());
    }

    #[test]
    fn multi_agent_auto_does_not_inherit_global_default() {
        let root = std::env::current_dir().unwrap().canonicalize().unwrap();
        let config = sample_config(&root.to_string_lossy());
        let pane_one = resolve_session_plan(
            &config,
            &McpProfileSelection::Auto,
            "codex",
            Some(&root.to_string_lossy()),
            Some(1),
        )
        .unwrap();
        assert!(pane_one.servers.is_empty());

        let pane_two = resolve_session_plan(
            &config,
            &McpProfileSelection::Auto,
            "codex",
            Some(&root.to_string_lossy()),
            Some(2),
        )
        .unwrap();
        assert_eq!(pane_two.profile_id.as_deref(), Some("web"));
        assert_eq!(pane_two.servers.len(), 2);
    }

    #[test]
    fn multi_agent_binding_mutations_preserve_unrelated_panes() {
        let root = std::env::current_dir().unwrap().canonicalize().unwrap();
        let workspace = root.to_string_lossy();
        let mut config = sample_config(&workspace);

        assert_eq!(
            multi_agent_binding_profile(&config, &workspace, 2)
                .unwrap()
                .as_deref(),
            Some("web")
        );
        assert!(apply_multi_agent_binding_mutation(
            &mut config,
            &workspace,
            1,
            McpMultiAgentBindingMutation::Set {
                profile_id: "web".into(),
            },
        )
        .unwrap());
        assert_eq!(
            multi_agent_binding_profile(&config, &workspace, 1)
                .unwrap()
                .as_deref(),
            Some("web")
        );

        // Re-selecting the already saved profile should not create a config
        // revision or overwrite an unrelated pane binding.
        assert!(!apply_multi_agent_binding_mutation(
            &mut config,
            &workspace,
            1,
            McpMultiAgentBindingMutation::Set {
                profile_id: "web".into(),
            },
        )
        .unwrap());

        assert!(apply_multi_agent_binding_mutation(
            &mut config,
            &workspace,
            1,
            McpMultiAgentBindingMutation::Clear,
        )
        .unwrap());
        assert_eq!(
            multi_agent_binding_profile(&config, &workspace, 1).unwrap(),
            None
        );
        assert_eq!(
            multi_agent_binding_profile(&config, &workspace, 2)
                .unwrap()
                .as_deref(),
            Some("web")
        );
        assert!(!apply_multi_agent_binding_mutation(
            &mut config,
            &workspace,
            1,
            McpMultiAgentBindingMutation::Clear,
        )
        .unwrap());
    }

    #[test]
    fn save_and_load_are_round_trippable() {
        let root = std::env::current_dir().unwrap().canonicalize().unwrap();
        let config = sample_config(&root.to_string_lossy());
        let dir =
            std::env::temp_dir().join(format!("coffee-mcp-config-test-{}", uuid::Uuid::new_v4()));
        let path = dir.join("mcp.json");
        save_to_path(&path, &config).unwrap();
        assert_eq!(load_from_path(&path).unwrap(), config);
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn reset_invalid_config_replaces_the_observed_invalid_snapshot() {
        let dir =
            std::env::temp_dir().join(format!("coffee-mcp-recovery-test-{}", uuid::Uuid::new_v4()));
        let path = dir.join("mcp.json");
        let invalid_body = "{ definitely not valid JSON";
        fs::create_dir_all(&dir).unwrap();
        fs::write(&path, invalid_body).unwrap();

        let token = invalid_config_recovery_token_at_path(&path).unwrap();
        let recovered = reset_invalid_config_at_path(&path, &token).unwrap();

        assert_eq!(
            recovered,
            McpConfig {
                revision: 1,
                ..McpConfig::default()
            }
        );
        assert_eq!(load_from_path(&path).unwrap(), recovered);
        assert_eq!(
            fs::read_to_string(path.with_extension("json.invalid.bak")).unwrap(),
            invalid_body
        );
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn reset_invalid_config_never_overwrites_a_valid_concurrent_repair() {
        let dir =
            std::env::temp_dir().join(format!("coffee-mcp-recovery-test-{}", uuid::Uuid::new_v4()));
        let path = dir.join("mcp.json");
        fs::create_dir_all(&dir).unwrap();
        fs::write(&path, "{ definitely not valid JSON").unwrap();
        let token = invalid_config_recovery_token_at_path(&path).unwrap();

        let valid = McpConfig {
            revision: 7,
            ..McpConfig::default()
        };
        save_to_path(&path, &valid).unwrap();
        let before = fs::read_to_string(&path).unwrap();

        assert!(reset_invalid_config_at_path(&path, &token)
            .unwrap_err()
            .contains("no longer invalid"));
        assert_eq!(fs::read_to_string(&path).unwrap(), before);
        assert_eq!(load_from_path(&path).unwrap(), valid);
        assert!(!path.with_extension("json.invalid.bak").exists());
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn normalize_bindings_canonicalizes_workspace_aliases() {
        let root = std::env::current_dir().unwrap().canonicalize().unwrap();
        let alias = root.join(".").to_string_lossy().to_string();
        let mut config = sample_config(&alias);
        normalize_bindings(&mut config);

        assert_eq!(
            config.workspace_bindings[0].workspace,
            root.to_string_lossy()
        );
        assert_eq!(
            config.multi_agent_bindings[0].workspace,
            root.to_string_lossy()
        );
    }

    #[test]
    fn rejects_future_versions_without_overwriting() {
        let config = McpConfig {
            version: CONFIG_VERSION + 1,
            ..McpConfig::default()
        };
        assert!(validate(&config)
            .unwrap_err()
            .contains("Unsupported MCP config version"));
    }

    #[test]
    fn stale_workspace_skips_binding_without_blocking_auto_profile() {
        let root = std::env::current_dir().unwrap().canonicalize().unwrap();
        let config = sample_config(&root.to_string_lossy());
        let plan = resolve_session_plan(
            &config,
            &McpProfileSelection::Auto,
            "codex",
            Some("/definitely/missing/coffee-mcp-workspace"),
            None,
        )
        .unwrap();

        // The global default still applies; a stale remembered workspace is
        // not a reason to refuse the whole Agent launch.
        assert_eq!(plan.profile_id.as_deref(), Some("web"));
    }

    #[test]
    fn rejects_reserved_stdio_target_environment() {
        let root = std::env::current_dir().unwrap().canonicalize().unwrap();
        let mut config = sample_config(&root.to_string_lossy());
        config.servers.get_mut("chrome").unwrap().transport = McpTransport::Stdio {
            command: "npx".into(),
            args: Vec::new(),
            env: BTreeMap::from([(
                "PATH".into(),
                McpEnvRef {
                    from_env: "MCP_PATH".into(),
                },
            )]),
        };

        assert!(validate(&config)
            .unwrap_err()
            .contains("reserved target environment variable 'PATH'"));
    }

    #[test]
    fn rejects_http_urls_with_embedded_credentials() {
        let root = std::env::current_dir().unwrap().canonicalize().unwrap();
        let mut config = sample_config(&root.to_string_lossy());
        config.servers.get_mut("burp").unwrap().transport = McpTransport::Http {
            url: "https://user:token@example.com/mcp".into(),
            headers: BTreeMap::new(),
        };

        assert!(validate(&config)
            .unwrap_err()
            .contains("URL must not include credentials"));
    }

    #[test]
    fn rejects_conflicting_profile_stdio_target_environment() {
        let root = std::env::current_dir().unwrap().canonicalize().unwrap();
        let mut config = sample_config(&root.to_string_lossy());
        config.servers.get_mut("chrome").unwrap().transport = McpTransport::Stdio {
            command: "npx".into(),
            args: Vec::new(),
            env: BTreeMap::from([(
                "API_KEY".into(),
                McpEnvRef {
                    from_env: "CHROME_API_KEY".into(),
                },
            )]),
        };
        config.servers.get_mut("burp").unwrap().transport = McpTransport::Stdio {
            command: "npx".into(),
            args: Vec::new(),
            env: BTreeMap::from([(
                "API_KEY".into(),
                McpEnvRef {
                    from_env: "BURP_API_KEY".into(),
                },
            )]),
        };

        assert!(resolve_session_plan(
            &config,
            &McpProfileSelection::Profile {
                profile_id: "web".into(),
            },
            "codex",
            None,
            None,
        )
        .unwrap_err()
        .contains("maps target environment variable 'API_KEY'"));
    }

    #[test]
    fn configuration_revision_increments_without_touching_schema_version() {
        let mut config = McpConfig::default();
        bump_revision(&mut config).unwrap();
        assert_eq!(config.version, CONFIG_VERSION);
        assert_eq!(config.revision, 1);
    }
}
