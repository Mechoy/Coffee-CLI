//! Coffee-owned native Skill configuration and source-file generation.
//!
//! The file under `~/.coffee-cli/skills.json` is the only Coffee source of
//! truth. Native adapters may install its generated `SKILL.md` into a CLI's
//! documented discovery directory, but they never treat that external copy as
//! authoritative configuration.

use fs2::FileExt;
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeMap,
    fs,
    io::Write,
    path::{Path, PathBuf},
};
use uuid::Uuid;

pub const CONFIG_VERSION: u32 = 1;
pub const MAX_SKILLS: usize = 100;
pub const MAX_NAME_CHARS: usize = 120;
pub const MAX_DESCRIPTION_CHARS: usize = 512;
pub const MAX_BODY_BYTES: usize = 32 * 1024;

const LOCK_FILE: &str = ".skills.lock";
const SOURCE_SKILL_FILE: &str = "SKILL.md";

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SkillsConfig {
    #[serde(default = "config_version")]
    pub version: u32,
    /// Optimistic concurrency token for Coffee's own writers. It does not
    /// describe the native CLI state, which is inspected afresh on demand.
    #[serde(default)]
    pub revision: u64,
    #[serde(default)]
    pub skills: BTreeMap<String, CoffeeSkill>,
}

impl Default for SkillsConfig {
    fn default() -> Self {
        Self {
            version: CONFIG_VERSION,
            revision: 0,
            skills: BTreeMap::new(),
        }
    }
}

impl Default for CoffeeSkill {
    fn default() -> Self {
        Self {
            name: String::new(),
            description: String::new(),
            body: String::new(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct CoffeeSkill {
    /// User-facing label. The native Skill name is derived from the stable
    /// Coffee ID so an external directory cannot collide with user-owned
    /// skills such as `review` or `deploy`.
    pub name: String,
    /// Kept deliberately short: native CLIs use this for discovery/matching.
    pub description: String,
    /// Markdown after Coffee-generated SKILL.md frontmatter.
    pub body: String,
}

/// Holds an advisory, cross-process lock for Coffee's managed Skills state.
/// Keeping the file open releases the lock automatically when the command
/// returns or the process exits.
pub struct SkillsWriteLock {
    _file: fs::File,
}

fn config_version() -> u32 {
    CONFIG_VERSION
}

pub fn config_path() -> Result<PathBuf, String> {
    dirs::home_dir()
        .map(|home| home.join(".coffee-cli").join("skills.json"))
        .ok_or_else(|| "Could not determine the user home directory".to_string())
}

/// Protects config/source/native mutations across separate Coffee processes.
/// The Tauri state mutex handles commands in one process; this file lock also
/// covers debug and release builds that may be launched separately.
pub fn acquire_write_lock() -> Result<SkillsWriteLock, String> {
    let config = config_path()?;
    let parent = config
        .parent()
        .ok_or_else(|| format!("Path has no parent: {}", config.display()))?;
    ensure_real_directory(parent, "Coffee configuration directory")?;
    let lock_path = parent.join(LOCK_FILE);
    ensure_regular_file_or_missing(&lock_path, "Coffee Skills lock file")?;

    let file = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .open(&lock_path)
        .map_err(|error| format!("Failed to open {}: {error}", lock_path.display()))?;
    file.try_lock_exclusive().map_err(|error| {
        format!(
            "Skills configuration is being updated by another Coffee process; try again shortly ({error})"
        )
    })?;
    Ok(SkillsWriteLock { _file: file })
}

pub fn source_root() -> Result<PathBuf, String> {
    dirs::home_dir()
        .map(|home| home.join(".coffee-cli").join("skills"))
        .ok_or_else(|| "Could not determine the user home directory".to_string())
}

pub fn source_dir(skill_id: &str) -> Result<PathBuf, String> {
    validate_id(skill_id)?;
    let root = source_root()?;
    match fs::symlink_metadata(&root) {
        Ok(metadata) if metadata.is_dir() && !metadata.file_type().is_symlink() => {}
        Ok(_) => {
            return Err(format!(
                "Coffee Skills source root must be a real directory: {}",
                root.display()
            ));
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(format!("Failed to inspect {}: {error}", root.display())),
    }
    let source = root.join(skill_id);
    match fs::symlink_metadata(&source) {
        Ok(metadata) if metadata.is_dir() && !metadata.file_type().is_symlink() => {}
        Ok(_) => {
            return Err(format!(
                "Coffee Skill source must be a real directory: {}",
                source.display()
            ));
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(format!("Failed to inspect {}: {error}", source.display())),
    }
    Ok(source)
}

pub fn native_skill_name(skill_id: &str) -> Result<String, String> {
    validate_id(skill_id)?;
    Ok(format!("coffee-{skill_id}"))
}

pub fn load() -> Result<SkillsConfig, String> {
    let path = config_path()?;
    let parent = path
        .parent()
        .ok_or_else(|| format!("Path has no parent: {}", path.display()))?;
    match fs::symlink_metadata(parent) {
        Ok(metadata) if metadata.is_dir() && !metadata.file_type().is_symlink() => {
            load_from_path(&path)
        }
        Ok(_) => Err(format!(
            "Coffee configuration directory must be a real directory: {}",
            parent.display()
        )),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(SkillsConfig::default()),
        Err(error) => Err(format!("Failed to inspect {}: {error}", parent.display())),
    }
}

fn ensure_real_directory(path: &Path, label: &str) -> Result<(), String> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.is_dir() && !metadata.file_type().is_symlink() => Ok(()),
        Ok(_) => Err(format!(
            "{label} must be a real directory: {}",
            path.display()
        )),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            fs::create_dir_all(path).map_err(|create_error| {
                format!("Failed to create {}: {create_error}", path.display())
            })?;
            match fs::symlink_metadata(path) {
                Ok(metadata) if metadata.is_dir() && !metadata.file_type().is_symlink() => Ok(()),
                Ok(_) => Err(format!(
                    "{label} must be a real directory: {}",
                    path.display()
                )),
                Err(verify_error) => Err(format!(
                    "Failed to verify {} after creation: {verify_error}",
                    path.display()
                )),
            }
        }
        Err(error) => Err(format!("Failed to inspect {}: {error}", path.display())),
    }
}

fn ensure_regular_file_or_missing(path: &Path, label: &str) -> Result<(), String> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.is_file() && !metadata.file_type().is_symlink() => Ok(()),
        Ok(_) => Err(format!(
            "{label} must be a regular file: {}",
            path.display()
        )),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(format!("Failed to inspect {}: {error}", path.display())),
    }
}

fn ensure_managed_source_dir(skill_id: &str) -> Result<PathBuf, String> {
    let root = source_root()?;
    ensure_real_directory(&root, "Coffee Skills source root")?;
    let source = root.join(skill_id);
    match fs::symlink_metadata(&source) {
        Ok(metadata) if metadata.is_dir() && !metadata.file_type().is_symlink() => {}
        Ok(_) => {
            return Err(format!(
                "Coffee Skill source must be a real directory: {}",
                source.display()
            ));
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            fs::create_dir(&source).map_err(|create_error| {
                format!("Failed to create {}: {create_error}", source.display())
            })?;
        }
        Err(error) => return Err(format!("Failed to inspect {}: {error}", source.display())),
    }
    validate_managed_source_tree(&source, false)?;
    Ok(source)
}

fn validate_managed_source_tree(source: &Path, require_skill: bool) -> Result<(), String> {
    let metadata = fs::symlink_metadata(source)
        .map_err(|error| format!("Failed to inspect {}: {error}", source.display()))?;
    if !metadata.is_dir() || metadata.file_type().is_symlink() {
        return Err(format!(
            "Coffee Skill source must be a real directory: {}",
            source.display()
        ));
    }

    let mut saw_skill = false;
    for entry in fs::read_dir(source)
        .map_err(|error| format!("Failed to read {}: {error}", source.display()))?
    {
        let entry =
            entry.map_err(|error| format!("Failed to inspect {}: {error}", source.display()))?;
        let name = entry.file_name();
        if name != std::ffi::OsStr::new(SOURCE_SKILL_FILE) {
            return Err(format!(
                "Coffee Skill source {} contains an external file; Coffee will not modify it",
                source.display()
            ));
        }
        let file_type = entry
            .file_type()
            .map_err(|error| format!("Failed to inspect {}: {error}", entry.path().display()))?;
        if !file_type.is_file() || file_type.is_symlink() {
            return Err(format!(
                "Coffee Skill source file must be a regular file: {}",
                entry.path().display()
            ));
        }
        saw_skill = true;
    }
    if require_skill && !saw_skill {
        return Err(format!(
            "Coffee Skill source is missing: {}",
            source.join(SOURCE_SKILL_FILE).display()
        ));
    }
    Ok(())
}

fn read_regular_source(path: &Path) -> Result<Option<Vec<u8>>, String> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.is_file() && !metadata.file_type().is_symlink() => fs::read(path)
            .map(Some)
            .map_err(|error| format!("Failed to read {}: {error}", path.display())),
        Ok(_) => Err(format!(
            "Coffee Skill source file must be a regular file: {}",
            path.display()
        )),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(format!("Failed to inspect {}: {error}", path.display())),
    }
}

pub fn save(config: SkillsConfig) -> Result<SkillsConfig, String> {
    save_to_path(&config_path()?, &config)?;
    Ok(config)
}

pub fn load_from_path(path: &Path) -> Result<SkillsConfig, String> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.is_file() && !metadata.file_type().is_symlink() => {}
        Ok(_) => {
            return Err(format!(
                "Coffee Skills configuration must be a regular file: {}",
                path.display()
            ));
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(SkillsConfig::default());
        }
        Err(error) => return Err(format!("Failed to inspect {}: {error}", path.display())),
    }
    let body = fs::read_to_string(path)
        .map_err(|error| format!("Failed to read {}: {error}", path.display()))?;
    let config: SkillsConfig = serde_json::from_str(&body)
        .map_err(|error| format!("Invalid Skills config {}: {error}", path.display()))?;
    validate_config(&config)?;
    Ok(config)
}

pub fn save_skill(
    expected_revision: u64,
    skill_id: String,
    skill: CoffeeSkill,
) -> Result<SkillsConfig, String> {
    validate_id(&skill_id)?;
    validate_skill(&skill_id, &skill)?;

    let mut config = load()?;
    ensure_revision(&config, expected_revision)?;

    if let Some(existing) = config.skills.get(&skill_id) {
        recover_staged_source(skill_id.as_str(), existing)?;
        verify_existing_source_matches_skill(skill_id.as_str(), existing)?;
    } else {
        reject_unowned_delete_stage(skill_id.as_str())?;
    }

    let source = ensure_managed_source_dir(&skill_id)?;
    let source_path = source.join(SOURCE_SKILL_FILE);
    let old_source = read_regular_source(&source_path)?;

    write_source_skill(&skill_id, &skill)?;
    config.skills.insert(skill_id.clone(), skill);
    bump_revision(&mut config)?;

    if let Err(error) = save(config.clone()) {
        // Restore the last complete source file if persistence of the JSON
        // metadata fails. The source directory was verified as Coffee-owned
        // before this mutation, so rollback never removes arbitrary paths.
        let restore_result = match old_source {
            Some(content) => atomic_write(&source_path, &content),
            None => remove_regular_file_if_present(&source_path),
        };
        return match restore_result {
            Ok(()) => Err(error),
            Err(restore_error) => Err(format!("{error}; source rollback failed: {restore_error}")),
        };
    }

    Ok(config)
}

/// Removes a verified Coffee source after the caller has confirmed no
/// Coffee-owned native target is still installed. Source removal happens
/// before the config commit and is reconstructed if that commit fails, so a
/// failed cleanup can never leave a config-less, hidden staging directory.
pub fn delete_skill(expected_revision: u64, skill_id: &str) -> Result<SkillsConfig, String> {
    validate_id(skill_id)?;
    let mut config = load()?;
    ensure_revision(&config, expected_revision)?;
    let skill = config
        .skills
        .get(skill_id)
        .cloned()
        .ok_or_else(|| format!("Coffee Skill '{skill_id}' does not exist"))?;
    recover_staged_source(skill_id, &skill)?;
    remove_verified_source(skill_id, &skill)?;
    config.skills.remove(skill_id);
    if let Err(error) = bump_revision(&mut config).and_then(|()| save(config.clone()).map(|_| ())) {
        let restore = write_source_skill(skill_id, &skill);
        return match restore {
            Ok(()) => Err(error),
            Err(restore_error) => Err(format!(
                "{error}; failed to restore Coffee Skill source after the rejected deletion: {restore_error}"
            )),
        };
    }

    Ok(config)
}

pub fn write_source_skill(skill_id: &str, skill: &CoffeeSkill) -> Result<(), String> {
    validate_id(skill_id)?;
    validate_skill(skill_id, skill)?;
    let source_path = ensure_managed_source_dir(skill_id)?.join(SOURCE_SKILL_FILE);
    let body = render_skill_markdown(skill_id, skill)?;
    atomic_write(&source_path, body.as_bytes())
}

/// A failed source removal keeps the configuration unchanged. On the next
/// save/delete request we only restore the exact expected one-file Coffee
/// source, never an arbitrary directory that happened to use the stage name.
fn recover_staged_source(skill_id: &str, expected: &CoffeeSkill) -> Result<(), String> {
    let root = source_root()?;
    ensure_real_directory(&root, "Coffee Skills source root")?;
    let source = root.join(skill_id);
    let staged = root.join(format!(".{skill_id}.coffee-deleting"));
    let staged_metadata = match fs::symlink_metadata(&staged) {
        Ok(metadata) => Some(metadata),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
        Err(error) => return Err(format!("Failed to inspect {}: {error}", staged.display())),
    };
    let Some(staged_metadata) = staged_metadata else {
        return Ok(());
    };
    if !staged_metadata.is_dir() || staged_metadata.file_type().is_symlink() {
        return Err(format!(
            "Refusing to recover non-directory Coffee deletion stage {}",
            staged.display()
        ));
    }
    match fs::symlink_metadata(&source) {
        Ok(_) => {
            return Err(format!(
                "Cannot recover Coffee deletion stage because source already exists: {}",
                source.display()
            ));
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(format!("Failed to inspect {}: {error}", source.display())),
    }
    validate_source_matches_skill(&staged, skill_id, expected)?;
    ensure_real_directory(
        source
            .parent()
            .ok_or_else(|| format!("Path has no parent: {}", source.display()))?,
        "Coffee Skills source root",
    )?;
    fs::rename(&staged, &source).map_err(|error| {
        format!(
            "Failed to recover Coffee Skill source from {}: {error}",
            staged.display()
        )
    })
}

/// A missing source can be regenerated from `skills.json` when the user saves
/// it again. A source that still exists must match the saved record exactly:
/// Coffee never silently overwrites a manual edit made in its managed folder.
fn verify_existing_source_matches_skill(
    skill_id: &str,
    expected: &CoffeeSkill,
) -> Result<(), String> {
    let root = source_root()?;
    ensure_real_directory(&root, "Coffee Skills source root")?;
    let source = root.join(skill_id);
    match fs::symlink_metadata(&source) {
        Ok(metadata) if metadata.is_dir() && !metadata.file_type().is_symlink() => {
            validate_source_matches_skill(&source, skill_id, expected)
        }
        Ok(_) => Err(format!(
            "Coffee Skill source must be a real directory: {}",
            source.display()
        )),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(format!("Failed to inspect {}: {error}", source.display())),
    }
}

fn reject_unowned_delete_stage(skill_id: &str) -> Result<(), String> {
    let root = source_root()?;
    match fs::symlink_metadata(&root) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Ok(metadata) if metadata.is_dir() && !metadata.file_type().is_symlink() => {}
        Ok(_) => {
            return Err(format!(
                "Coffee Skills source root must be a real directory: {}",
                root.display()
            ));
        }
        Err(error) => return Err(format!("Failed to inspect {}: {error}", root.display())),
    }
    let staged = root.join(format!(".{skill_id}.coffee-deleting"));
    match fs::symlink_metadata(&staged) {
        Ok(_) => Err(format!(
            "Cannot create Skill '{skill_id}': an unfinished Coffee deletion stage exists at {}",
            staged.display()
        )),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(format!("Failed to inspect {}: {error}", staged.display())),
    }
}

fn remove_verified_source(skill_id: &str, expected: &CoffeeSkill) -> Result<(), String> {
    let root = source_root()?;
    ensure_real_directory(&root, "Coffee Skills source root")?;
    let source = root.join(skill_id);
    let metadata = match fs::symlink_metadata(&source) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(format!("Failed to inspect {}: {error}", source.display())),
    };
    if !metadata.is_dir() || metadata.file_type().is_symlink() {
        return Err(format!(
            "Refusing to remove non-directory Coffee Skill source {}",
            source.display()
        ));
    }
    validate_source_matches_skill(&source, skill_id, expected)?;

    let staged = root.join(format!(".{skill_id}.coffee-deleting"));
    match fs::symlink_metadata(&staged) {
        Ok(_) => {
            return Err(format!(
                "Cannot delete Skill '{skill_id}': Coffee deletion stage already exists at {}",
                staged.display()
            ));
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(format!("Failed to inspect {}: {error}", staged.display())),
    }
    fs::rename(&source, &staged).map_err(|error| {
        format!(
            "Failed to stage Coffee Skill source {} for deletion: {error}",
            source.display()
        )
    })?;

    if let Err(error) = validate_source_matches_skill(&staged, skill_id, expected) {
        return restore_staged_source(&staged, &source, skill_id, expected, error);
    }
    if let Err(error) = remove_regular_file_if_present(&staged.join(SOURCE_SKILL_FILE)) {
        return restore_staged_source(&staged, &source, skill_id, expected, error);
    }
    if let Err(error) = fs::remove_dir(&staged) {
        return restore_staged_source(
            &staged,
            &source,
            skill_id,
            expected,
            format!("Failed to remove {}: {error}", staged.display()),
        );
    }
    Ok(())
}

fn restore_staged_source(
    staged: &Path,
    source: &Path,
    skill_id: &str,
    expected: &CoffeeSkill,
    error: String,
) -> Result<(), String> {
    let staged_skill = staged.join(SOURCE_SKILL_FILE);
    if matches!(
        fs::symlink_metadata(&staged_skill),
        Err(source_error) if source_error.kind() == std::io::ErrorKind::NotFound
    ) {
        let rendered = match render_skill_markdown(skill_id, expected) {
            Ok(rendered) => rendered,
            Err(render_error) => {
                return Err(format!(
                    "{error}; failed to render source recovery: {render_error}"
                ))
            }
        };
        if let Err(write_error) = atomic_write(&staged_skill, rendered.as_bytes()) {
            return Err(format!(
                "{error}; failed to restore staged source content: {write_error}"
            ));
        }
    }
    match fs::rename(staged, source) {
        Ok(()) => Err(error),
        Err(restore_error) => Err(format!(
            "{error}; failed to restore Coffee source from {}: {restore_error}",
            staged.display()
        )),
    }
}

fn validate_source_matches_skill(
    source: &Path,
    skill_id: &str,
    expected: &CoffeeSkill,
) -> Result<(), String> {
    validate_managed_source_tree(source, true)?;
    let actual_path = source.join(SOURCE_SKILL_FILE);
    let actual = read_regular_source(&actual_path)?
        .ok_or_else(|| format!("Coffee Skill source is missing: {}", actual_path.display()))?;
    let expected = render_skill_markdown(skill_id, expected)?;
    if actual != expected.as_bytes() {
        return Err(format!(
            "Coffee Skill source {} was modified outside Coffee; review it before deleting",
            actual_path.display()
        ));
    }
    Ok(())
}

fn remove_regular_file_if_present(path: &Path) -> Result<(), String> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.is_file() && !metadata.file_type().is_symlink() => {
            fs::remove_file(path)
                .map_err(|error| format!("Failed to remove {}: {error}", path.display()))
        }
        Ok(_) => Err(format!(
            "Refusing to remove non-regular Coffee Skill source {}",
            path.display()
        )),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(format!("Failed to inspect {}: {error}", path.display())),
    }
}

pub fn render_skill_markdown(skill_id: &str, skill: &CoffeeSkill) -> Result<String, String> {
    validate_id(skill_id)?;
    validate_skill(skill_id, skill)?;
    // JSON double-quoted strings are valid YAML scalar strings. Using serde
    // keeps quotes, backslashes and control characters from breaking the
    // native frontmatter without introducing a YAML serializer dependency.
    let description = serde_json::to_string(&skill.description)
        .map_err(|error| format!("Failed to render Skill description: {error}"))?;
    let native_name = native_skill_name(skill_id)?;
    Ok(format!(
        "---\nname: {native_name}\ndescription: {description}\n---\n\n{}\n",
        skill.body.trim_end()
    ))
}

pub fn fingerprint(body: &[u8]) -> String {
    // This detects accidental external edits. It is deliberately not a
    // security primitive; ownership is established by paths and markers.
    let mut hash = 0xcbf2_9ce4_8422_2325_u64;
    for byte in body {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    format!("{}-{hash:016x}", body.len())
}

pub fn validate_config(config: &SkillsConfig) -> Result<(), String> {
    if config.version != CONFIG_VERSION {
        return Err(format!(
            "Unsupported Skills config version {} (this Coffee version supports {})",
            config.version, CONFIG_VERSION
        ));
    }
    if config.skills.len() > MAX_SKILLS {
        return Err(format!(
            "Coffee supports at most {MAX_SKILLS} managed Skills"
        ));
    }
    for (skill_id, skill) in &config.skills {
        validate_skill(skill_id, skill)?;
    }
    Ok(())
}

pub fn validate_id(skill_id: &str) -> Result<(), String> {
    let bytes = skill_id.as_bytes();
    let valid = (1..=63).contains(&bytes.len())
        && bytes[0].is_ascii_lowercase()
        && bytes
            .iter()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || *byte == b'-')
        && !skill_id.ends_with('-')
        && !skill_id.contains("--");
    if !valid {
        return Err(
            "Skill ID must use 1-63 lowercase letters, digits, and single hyphens; it must start with a letter and cannot end with a hyphen"
                .to_string(),
        );
    }
    Ok(())
}

pub fn validate_skill(skill_id: &str, skill: &CoffeeSkill) -> Result<(), String> {
    validate_id(skill_id)?;
    let name = skill.name.trim();
    if name.is_empty()
        || name.chars().count() > MAX_NAME_CHARS
        || name
            .chars()
            .any(|character| character == '\r' || character == '\n')
    {
        return Err(format!(
            "Skill '{skill_id}' name must be one line with at most {MAX_NAME_CHARS} characters"
        ));
    }
    let description = skill.description.trim();
    if description.is_empty()
        || description.chars().count() > MAX_DESCRIPTION_CHARS
        || description
            .chars()
            .any(|character| character == '\r' || character == '\n')
    {
        return Err(format!(
            "Skill '{skill_id}' description must be one line with at most {MAX_DESCRIPTION_CHARS} characters"
        ));
    }
    if skill.body.trim().is_empty() || skill.body.as_bytes().len() > MAX_BODY_BYTES {
        return Err(format!(
            "Skill '{skill_id}' body must be non-empty and at most {MAX_BODY_BYTES} bytes"
        ));
    }
    Ok(())
}

fn ensure_revision(config: &SkillsConfig, expected_revision: u64) -> Result<(), String> {
    if config.revision != expected_revision {
        return Err(
            "Skills configuration changed in another Coffee window; reload it before saving"
                .to_string(),
        );
    }
    Ok(())
}

fn bump_revision(config: &mut SkillsConfig) -> Result<(), String> {
    config.revision = config
        .revision
        .checked_add(1)
        .ok_or_else(|| "Skills configuration revision overflowed".to_string())?;
    Ok(())
}

fn save_to_path(path: &Path, config: &SkillsConfig) -> Result<(), String> {
    validate_config(config)?;
    let body = serde_json::to_string_pretty(config)
        .map_err(|error| format!("Failed to serialize Skills config: {error}"))?;
    atomic_write(path, format!("{body}\n").as_bytes())
}

fn atomic_write(path: &Path, body: &[u8]) -> Result<(), String> {
    let parent = path
        .parent()
        .ok_or_else(|| format!("Path has no parent: {}", path.display()))?;
    ensure_real_directory(parent, "Coffee configuration parent directory")?;
    ensure_regular_file_or_missing(path, "Coffee configuration file")?;
    let filename = path
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| format!("Path has no UTF-8 file name: {}", path.display()))?;
    let temp = parent.join(format!(".{filename}.{}.tmp", Uuid::new_v4()));

    let write_result = (|| -> Result<(), String> {
        let mut options = fs::OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let mut file = options
            .open(&temp)
            .map_err(|error| format!("Failed to create {}: {error}", temp.display()))?;
        file.write_all(body)
            .map_err(|error| format!("Failed to write {}: {error}", temp.display()))?;
        file.sync_all()
            .map_err(|error| format!("Failed to flush {}: {error}", temp.display()))?;
        drop(file);

        if let Err(first_error) = fs::rename(&temp, path) {
            // Windows may reject replacement of an open file. Move the old
            // regular file aside atomically, then restore it if the new file
            // cannot be promoted. Never remove an entry after a failed swap.
            ensure_regular_file_or_missing(path, "Coffee configuration file")?;
            if !path.exists() {
                return Err(format!(
                    "Failed to replace {}: {first_error}",
                    path.display()
                ));
            }
            let backup = parent.join(format!(".{filename}.{}.bak", Uuid::new_v4()));
            fs::rename(path, &backup).map_err(|error| {
                format!(
                    "Failed to stage previous {} after {first_error}: {error}",
                    path.display()
                )
            })?;
            if let Err(rename_error) = fs::rename(&temp, path) {
                let restore = fs::rename(&backup, path);
                return match restore {
                    Ok(()) => Err(format!(
                        "Failed to replace {}: {rename_error}; restored the previous file",
                        path.display()
                    )),
                    Err(restore_error) => Err(format!(
                        "Failed to replace {}: {rename_error}; previous file remains at {} ({restore_error})",
                        path.display(),
                        backup.display()
                    )),
                };
            }
            let _ = fs::remove_file(backup);
        }
        #[cfg(unix)]
        if let Ok(directory) = fs::File::open(parent) {
            let _ = directory.sync_all();
        }
        Ok(())
    })();

    if write_result.is_err() {
        let _ = fs::remove_file(&temp);
    }
    write_result
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn sample() -> CoffeeSkill {
        CoffeeSkill {
            name: "Research summary".to_string(),
            description: "Summarize sources with concise findings.".to_string(),
            body: "Use primary sources and identify uncertainty.".to_string(),
        }
    }

    fn temp_source_root(label: &str) -> PathBuf {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        std::env::temp_dir().join(format!(
            "coffee-skills-config-{label}-{}-{nonce}",
            std::process::id()
        ))
    }

    #[test]
    fn renders_portable_skill_markdown() {
        let markdown = render_skill_markdown("research-summary", &sample()).unwrap();
        assert!(markdown.starts_with("---\nname: coffee-research-summary\n"));
        assert!(markdown.contains("description: \"Summarize sources with concise findings.\""));
        assert!(markdown.ends_with("Use primary sources and identify uncertainty.\n"));
    }

    #[test]
    fn rejects_path_like_ids() {
        for value in ["../outside", "Coffee", "two--dash", "ends-", "with_under"] {
            assert!(validate_id(value).is_err(), "{value} should be rejected");
        }
    }

    #[test]
    fn rejects_multiline_descriptions() {
        let mut skill = sample();
        skill.description = "one\ntwo".to_string();
        assert!(validate_skill("research-summary", &skill).is_err());
    }

    #[test]
    fn detects_external_source_change_before_a_save_or_delete() {
        let source = temp_source_root("source-drift");
        fs::create_dir_all(&source).unwrap();
        let skill = sample();
        fs::write(
            source.join(SOURCE_SKILL_FILE),
            render_skill_markdown("research-summary", &skill).unwrap(),
        )
        .unwrap();
        assert!(validate_source_matches_skill(&source, "research-summary", &skill).is_ok());

        fs::write(source.join(SOURCE_SKILL_FILE), "manual change").unwrap();
        assert!(validate_source_matches_skill(&source, "research-summary", &skill).is_err());
        let _ = fs::remove_dir_all(source);
    }

    #[test]
    fn refuses_external_files_in_a_managed_source_directory() {
        let source = temp_source_root("source-extra");
        fs::create_dir_all(&source).unwrap();
        let skill = sample();
        fs::write(
            source.join(SOURCE_SKILL_FILE),
            render_skill_markdown("research-summary", &skill).unwrap(),
        )
        .unwrap();
        fs::write(source.join("reference.md"), "user content").unwrap();

        assert!(validate_source_matches_skill(&source, "research-summary", &skill).is_err());
        assert!(source.join("reference.md").is_file());
        let _ = fs::remove_dir_all(source);
    }
}
