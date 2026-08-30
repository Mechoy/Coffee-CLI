//! Adapter layer for Coffee-owned Skills in native Codex and Claude folders.
//!
//! This module deliberately does not edit `~/.codex/config.toml` or Claude
//! settings. A target is enabled only while Coffee's own installation exists;
//! this gives both CLIs a safe, native discovery path without taking ownership
//! of user configuration.

use crate::skills_config;
use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    io::Write,
    path::{Path, PathBuf},
};

const COPY_MARKER_FILE: &str = ".coffee-cli-managed.json";
const COPY_MARKER_VERSION: u32 = 1;

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "snake_case")]
pub enum NativeSkillTarget {
    Codex,
    Claude,
}

impl NativeSkillTarget {
    pub const ALL: [Self; 2] = [Self::Codex, Self::Claude];

    pub fn id(self) -> &'static str {
        match self {
            Self::Codex => "codex",
            Self::Claude => "claude",
        }
    }

    fn root(self) -> Result<PathBuf, String> {
        let home = dirs::home_dir()
            .ok_or_else(|| "Could not determine the user home directory".to_string())?;
        Ok(match self {
            Self::Codex => home.join(".agents").join("skills"),
            Self::Claude => home.join(".claude").join("skills"),
        })
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum NativeSkillStatusKind {
    Disabled,
    EnabledLinked,
    EnabledCopied,
    NeedsSync,
    Conflict,
    Drift,
    SourceMissing,
    Error,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct NativeSkillStatus {
    pub state: NativeSkillStatusKind,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
}

impl NativeSkillStatus {
    fn new(state: NativeSkillStatusKind) -> Self {
        Self {
            state,
            detail: None,
        }
    }

    fn with_detail(state: NativeSkillStatusKind, detail: impl Into<String>) -> Self {
        Self {
            state,
            detail: Some(detail.into()),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct CopyOwnershipMarker {
    version: u32,
    skill_id: String,
    source_dir: String,
    source_fingerprint: String,
    installed_fingerprint: String,
}

enum SourceCheckError {
    Missing(String),
    Drift(String),
    Error(String),
}

impl SourceCheckError {
    fn state(&self) -> NativeSkillStatusKind {
        match self {
            Self::Missing(_) => NativeSkillStatusKind::SourceMissing,
            Self::Drift(_) => NativeSkillStatusKind::Drift,
            Self::Error(_) => NativeSkillStatusKind::Error,
        }
    }

    fn detail(self) -> String {
        match self {
            Self::Missing(detail) | Self::Drift(detail) | Self::Error(detail) => detail,
        }
    }
}

pub fn inspect_all(
    config: &skills_config::SkillsConfig,
) -> BTreeMap<String, BTreeMap<String, NativeSkillStatus>> {
    config
        .skills
        .iter()
        .map(|(skill_id, skill)| {
            let statuses = NativeSkillTarget::ALL
                .into_iter()
                .map(|target| {
                    (
                        target.id().to_string(),
                        inspect_with_skill(skill_id, skill, target),
                    )
                })
                .collect();
            (skill_id.clone(), statuses)
        })
        .collect()
}

fn inspect_with_skill(
    skill_id: &str,
    skill: &skills_config::CoffeeSkill,
    target: NativeSkillTarget,
) -> NativeSkillStatus {
    let expected_source = match expected_skill_body(skill_id, skill) {
        Ok(body) => body,
        Err(error) => return NativeSkillStatus::with_detail(NativeSkillStatusKind::Error, error),
    };
    let source = match skills_config::source_dir(skill_id) {
        Ok(path) => path,
        Err(error) => return NativeSkillStatus::with_detail(NativeSkillStatusKind::Error, error),
    };
    let root = match target.root() {
        Ok(path) => path,
        Err(error) => return NativeSkillStatus::with_detail(NativeSkillStatusKind::Error, error),
    };
    inspect_at(skill_id, &expected_source, &source, &root)
}

pub fn set_enabled(
    skill_id: &str,
    target: NativeSkillTarget,
    enabled: bool,
) -> Result<NativeSkillStatus, String> {
    let source = skills_config::source_dir(skill_id)?;
    let root = target.root()?;
    if enabled {
        let config = skills_config::load()?;
        let skill = config
            .skills
            .get(skill_id)
            .ok_or_else(|| format!("Coffee Skill '{skill_id}' does not exist"))?;
        let expected_source = expected_skill_body(skill_id, skill)?;
        enable_at(skill_id, &expected_source, &source, &root)
    } else {
        disable_at(skill_id, &source, &root)
    }
}

fn inspect_at(
    skill_id: &str,
    expected_source: &[u8],
    source: &Path,
    target_root: &Path,
) -> NativeSkillStatus {
    if let Err(error) = verify_source_matches_expected(source, expected_source) {
        let state = error.state();
        return NativeSkillStatus::with_detail(state, error.detail());
    }
    let entry = match target_entry_path(skill_id, target_root) {
        Ok(path) => path,
        Err(error) => return NativeSkillStatus::with_detail(NativeSkillStatusKind::Error, error),
    };
    let metadata = match fs::symlink_metadata(&entry) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return NativeSkillStatus::new(NativeSkillStatusKind::Disabled);
        }
        Err(error) => {
            return NativeSkillStatus::with_detail(
                NativeSkillStatusKind::Error,
                format!("Failed to inspect {}: {error}", entry.display()),
            );
        }
    };

    if metadata.file_type().is_symlink() {
        return match link_points_to(&entry, source) {
            Ok(true) => NativeSkillStatus::new(NativeSkillStatusKind::EnabledLinked),
            Ok(false) => NativeSkillStatus::with_detail(
                NativeSkillStatusKind::Conflict,
                format!("{} is a link not owned by Coffee", entry.display()),
            ),
            Err(error) => NativeSkillStatus::with_detail(NativeSkillStatusKind::Error, error),
        };
    }

    if !metadata.is_dir() {
        return NativeSkillStatus::with_detail(
            NativeSkillStatusKind::Conflict,
            format!(
                "{} exists but is not a Coffee Skill directory",
                entry.display()
            ),
        );
    }

    let marker = match read_marker(&entry) {
        Ok(Some(marker)) => marker,
        Ok(None) => {
            return NativeSkillStatus::with_detail(
                NativeSkillStatusKind::Conflict,
                format!(
                    "{} is an existing user-owned Skill directory",
                    entry.display()
                ),
            );
        }
        Err(error) => return NativeSkillStatus::with_detail(NativeSkillStatusKind::Error, error),
    };

    match validate_copy_marker(&marker, skill_id, source, &entry) {
        Ok(()) => {}
        Err(error) => {
            return NativeSkillStatus::with_detail(NativeSkillStatusKind::Conflict, error)
        }
    }
    if let Err(error) = validate_managed_copy_layout(&entry) {
        return NativeSkillStatus::with_detail(NativeSkillStatusKind::Drift, error);
    }

    let installed = match fingerprint_file(&entry.join("SKILL.md")) {
        Ok(value) => value,
        Err(error) => return NativeSkillStatus::with_detail(NativeSkillStatusKind::Drift, error),
    };
    if installed != marker.installed_fingerprint {
        return NativeSkillStatus::with_detail(
            NativeSkillStatusKind::Drift,
            format!("{} was modified outside Coffee", entry.display()),
        );
    }

    let source_fingerprint = skills_config::fingerprint(expected_source);
    if source_fingerprint != marker.source_fingerprint {
        return NativeSkillStatus::with_detail(
            NativeSkillStatusKind::NeedsSync,
            "Coffee source changed; re-enable this target to synchronize its managed copy",
        );
    }

    NativeSkillStatus::new(NativeSkillStatusKind::EnabledCopied)
}

fn enable_at(
    skill_id: &str,
    expected_source: &[u8],
    source: &Path,
    target_root: &Path,
) -> Result<NativeSkillStatus, String> {
    verify_source_matches_expected(source, expected_source).map_err(SourceCheckError::detail)?;
    let entry = target_entry_path(skill_id, target_root)?;
    match inspect_at(skill_id, expected_source, source, target_root).state {
        NativeSkillStatusKind::EnabledLinked => {
            return Ok(NativeSkillStatus::new(NativeSkillStatusKind::EnabledLinked))
        }
        NativeSkillStatusKind::EnabledCopied => {
            return Ok(NativeSkillStatus::new(NativeSkillStatusKind::EnabledCopied))
        }
        NativeSkillStatusKind::NeedsSync => {
            return replace_owned_copy(skill_id, source, expected_source, &entry)
        }
        NativeSkillStatusKind::Disabled => {}
        NativeSkillStatusKind::Conflict => {
            return Err(format!(
                "Cannot install '{}' because {} is not owned by Coffee",
                skill_id,
                entry.display()
            ));
        }
        NativeSkillStatusKind::Drift => {
            return Err(format!(
                "Cannot synchronize '{}' because {} was modified outside Coffee",
                skill_id,
                entry.display()
            ));
        }
        NativeSkillStatusKind::SourceMissing => {
            return Err(format!("Coffee source is missing for '{skill_id}'"))
        }
        NativeSkillStatusKind::Error => {
            return Err(format!(
                "Cannot inspect native Skill path {}",
                entry.display()
            ));
        }
    }

    fs::create_dir_all(target_root)
        .map_err(|error| format!("Failed to create {}: {error}", target_root.display()))?;
    let canonical_source = fs::canonicalize(source).map_err(|error| {
        format!(
            "Failed to resolve Coffee source {}: {error}",
            source.display()
        )
    })?;
    match create_directory_link(&canonical_source, &entry) {
        Ok(()) => Ok(NativeSkillStatus::new(NativeSkillStatusKind::EnabledLinked)),
        Err(link_error) => {
            create_managed_copy(skill_id, &canonical_source, expected_source, &entry).map_err(|copy_error| {
                format!(
                    "Failed to link {} ({link_error}); managed-copy fallback also failed: {copy_error}",
                    entry.display()
                )
            })?;
            Ok(NativeSkillStatus::with_detail(
                NativeSkillStatusKind::EnabledCopied,
                "Directory link was unavailable; Coffee installed a managed copy instead",
            ))
        }
    }
}

fn disable_at(
    skill_id: &str,
    source: &Path,
    target_root: &Path,
) -> Result<NativeSkillStatus, String> {
    let entry = target_entry_path(skill_id, target_root)?;
    let metadata = match fs::symlink_metadata(&entry) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(NativeSkillStatus::new(NativeSkillStatusKind::Disabled));
        }
        Err(error) => return Err(format!("Failed to inspect {}: {error}", entry.display())),
    };

    if metadata.file_type().is_symlink() {
        if !link_points_to(&entry, source)? {
            return Err(format!(
                "Refusing to remove non-Coffee link {}",
                entry.display()
            ));
        }
        remove_link(&entry)?;
        return Ok(NativeSkillStatus::new(NativeSkillStatusKind::Disabled));
    }

    if !metadata.is_dir() {
        return Err(format!(
            "Refusing to remove non-directory native Skill {}",
            entry.display()
        ));
    }
    let staged = stage_verified_copy(skill_id, source, &entry, "removal")?;
    remove_managed_copy_files(skill_id, source, &staged)?;
    Ok(NativeSkillStatus::new(NativeSkillStatusKind::Disabled))
}

fn replace_owned_copy(
    skill_id: &str,
    source: &Path,
    expected_source: &[u8],
    entry: &Path,
) -> Result<NativeSkillStatus, String> {
    verify_source_matches_expected(source, expected_source).map_err(SourceCheckError::detail)?;
    let staged = stage_verified_copy(skill_id, source, entry, "synchronization")?;
    match create_managed_copy(skill_id, source, expected_source, entry) {
        Ok(()) => {
            match remove_managed_copy_files(skill_id, source, &staged) {
                Ok(()) => Ok(NativeSkillStatus::new(NativeSkillStatusKind::EnabledCopied)),
                Err(error) => Ok(NativeSkillStatus::with_detail(
                    NativeSkillStatusKind::EnabledCopied,
                    format!(
                        "Installed the synchronized copy, but retained the prior Coffee copy at {}: {error}",
                        staged.display()
                    ),
                )),
            }
        }
        Err(error) => retain_staged_copy_after_failed_replace(&staged, entry, error),
    }
}

/// Move a verified managed copy out of the native discovery directory before
/// removal or replacement, then verify it once more at the staged path. The
/// second check prevents a directory swapped by another local process between
/// the initial inspection and deletion from being removed as if Coffee owned
/// it. If revalidation fails, retain the staged directory: restoring with a
/// rename could overwrite a path created by another process after staging.
fn stage_verified_copy(
    skill_id: &str,
    source: &Path,
    entry: &Path,
    operation: &str,
) -> Result<PathBuf, String> {
    validate_owned_copy(skill_id, source, entry)?;
    let target_root = entry
        .parent()
        .ok_or_else(|| format!("Native Skill path has no parent: {}", entry.display()))?;
    let staged = unique_sibling(
        target_root,
        &format!(
            ".{}-coffee-stage",
            entry
                .file_name()
                .and_then(|name| name.to_str())
                .unwrap_or("skill")
        ),
    )?;
    fs::rename(entry, &staged).map_err(|error| {
        format!(
            "Failed to stage {} before {operation}: {error}",
            entry.display()
        )
    })?;
    if let Err(error) = validate_owned_copy(skill_id, source, &staged) {
        return Err(format!(
            "{error}; retained the staged directory at {} and did not restore {}",
            staged.display(),
            entry.display()
        ));
    }
    Ok(staged)
}

/// Do not restore with `rename(staged, entry)` after a replacement failure:
/// another process may have created `entry` since it was staged, and POSIX
/// rename may replace an empty directory. Retaining the verified old copy is
/// preferable to deleting or overwriting a path Coffee no longer owns.
fn retain_staged_copy_after_failed_replace(
    staged: &Path,
    entry: &Path,
    error: String,
) -> Result<NativeSkillStatus, String> {
    Err(format!(
        "{error}; retained the prior Coffee-managed copy at {} and did not replace {}",
        staged.display(),
        entry.display()
    ))
}

fn target_entry_path(skill_id: &str, target_root: &Path) -> Result<PathBuf, String> {
    Ok(target_root.join(skills_config::native_skill_name(skill_id)?))
}

fn create_managed_copy(
    skill_id: &str,
    source: &Path,
    source_body: &[u8],
    entry: &Path,
) -> Result<(), String> {
    let parent = entry
        .parent()
        .ok_or_else(|| format!("Native Skill path has no parent: {}", entry.display()))?;
    fs::create_dir_all(parent)
        .map_err(|error| format!("Failed to create {}: {error}", parent.display()))?;
    let source_fingerprint = skills_config::fingerprint(source_body);
    let marker = CopyOwnershipMarker {
        version: COPY_MARKER_VERSION,
        skill_id: skill_id.to_string(),
        source_dir: fs::canonicalize(source)
            .map_err(|error| {
                format!(
                    "Failed to resolve Coffee source {}: {error}",
                    source.display()
                )
            })?
            .display()
            .to_string(),
        source_fingerprint: source_fingerprint.clone(),
        installed_fingerprint: source_fingerprint,
    };
    let marker_body = serde_json::to_string_pretty(&marker)
        .map_err(|error| format!("Failed to serialize Coffee ownership marker: {error}"))?;
    let marker_body = format!("{marker_body}\n");
    // Claim the final path with an exclusive directory creation. Renaming a
    // staged directory over this path can replace an empty directory created
    // by another process on POSIX, which violates the ownership boundary.
    fs::create_dir(entry).map_err(|error| {
        format!(
            "Failed to reserve managed Skill directory {}: {error}",
            entry.display()
        )
    })?;
    let result = (|| -> Result<(), String> {
        // Publish the marker before SKILL.md. A failed copy therefore never
        // leaves a partially written native Skill discoverable by a CLI.
        publish_new_file(entry, COPY_MARKER_FILE, marker_body.as_bytes())?;
        publish_new_file(entry, "SKILL.md", source_body)?;
        validate_managed_copy_layout(entry)
    })();
    if let Err(error) = result {
        return match cleanup_failed_new_copy(entry, source_body, marker_body.as_bytes()) {
            Ok(()) => Err(error),
            Err(cleanup_error) => Err(format!(
                "{error}; retained incomplete managed-copy state at {}: {cleanup_error}",
                entry.display()
            )),
        };
    }
    Ok(())
}

fn write_new_file(path: &Path, body: &[u8]) -> std::io::Result<()> {
    let mut options = fs::OpenOptions::new();
    options.write(true).create_new(true);
    let mut file = options.open(path)?;
    file.write_all(body)?;
    file.sync_all()
}

/// Publish a fully flushed temporary file with a hard-link operation. Unlike
/// `rename`, hard links fail when another process has created the final name,
/// so Coffee cannot overwrite a concurrently-created user file.
fn publish_new_file(entry: &Path, name: &str, body: &[u8]) -> Result<(), String> {
    let temporary = unique_sibling(entry, ".coffee-new-file")?;
    if let Err(error) = write_new_file(&temporary, body) {
        let _ = remove_file_if_matches(&temporary, body);
        return Err(format!(
            "Failed to prepare {}: {error}",
            temporary.display()
        ));
    }
    let destination = entry.join(name);
    let publish_result = fs::hard_link(&temporary, &destination).map_err(|error| {
        format!(
            "Failed to publish managed Skill file {}: {error}",
            destination.display()
        )
    });
    let cleanup_result = remove_file_if_matches(&temporary, body);
    match (publish_result, cleanup_result) {
        (Ok(()), Ok(())) => Ok(()),
        (Ok(()), Err(error)) => Err(format!(
            "Published {} but failed to clean temporary {}: {error}",
            destination.display(),
            temporary.display()
        )),
        (Err(error), Ok(())) => Err(error),
        (Err(error), Err(cleanup_error)) => Err(format!(
            "{error}; failed to clean temporary {}: {cleanup_error}",
            temporary.display()
        )),
    }
}

fn cleanup_failed_new_copy(
    entry: &Path,
    source_body: &[u8],
    marker_body: &[u8],
) -> Result<(), String> {
    // Only remove files that still contain exactly the data this operation
    // wrote. If another process added or changed anything, leave it intact.
    remove_file_if_matches(&entry.join("SKILL.md"), source_body)?;
    remove_file_if_matches(&entry.join(COPY_MARKER_FILE), marker_body)?;
    fs::remove_dir(entry).map_err(|error| {
        format!(
            "could not remove now-nonempty managed Skill directory {}: {error}",
            entry.display()
        )
    })
}

fn remove_file_if_matches(path: &Path, expected: &[u8]) -> Result<(), String> {
    let metadata = match fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(format!("Failed to inspect {}: {error}", path.display())),
    };
    if !metadata.file_type().is_file() {
        return Err(format!(
            "Refusing to remove non-regular file {}",
            path.display()
        ));
    }
    let body =
        fs::read(path).map_err(|error| format!("Failed to read {}: {error}", path.display()))?;
    if body != expected {
        return Err(format!(
            "Refusing to remove changed temporary or managed file {}",
            path.display()
        ));
    }
    fs::remove_file(path).map_err(|error| format!("Failed to remove {}: {error}", path.display()))
}

fn read_marker(entry: &Path) -> Result<Option<CopyOwnershipMarker>, String> {
    let marker_path = entry.join(COPY_MARKER_FILE);
    match fs::symlink_metadata(&marker_path) {
        Ok(metadata) if metadata.file_type().is_file() => {}
        Ok(_) => {
            return Err(format!(
                "Coffee ownership marker must be a regular file: {}",
                marker_path.display()
            ));
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => {
            return Err(format!(
                "Failed to inspect {}: {error}",
                marker_path.display()
            ))
        }
    }
    let body = fs::read_to_string(&marker_path)
        .map_err(|error| format!("Failed to read {}: {error}", marker_path.display()))?;
    let marker = serde_json::from_str(&body).map_err(|error| {
        format!(
            "Invalid Coffee ownership marker {}: {error}",
            marker_path.display()
        )
    })?;
    Ok(Some(marker))
}

fn validate_copy_marker(
    marker: &CopyOwnershipMarker,
    skill_id: &str,
    source: &Path,
    entry: &Path,
) -> Result<(), String> {
    if marker.version != COPY_MARKER_VERSION || marker.skill_id != skill_id {
        return Err(format!(
            "{} has an incompatible Coffee ownership marker",
            entry.display()
        ));
    }
    let canonical_source = fs::canonicalize(source).map_err(|error| {
        format!(
            "Failed to resolve Coffee source {}: {error}",
            source.display()
        )
    })?;
    if marker.source_dir != canonical_source.display().to_string() {
        return Err(format!(
            "{} belongs to a different Coffee source",
            entry.display()
        ));
    }
    Ok(())
}

fn validate_owned_copy(skill_id: &str, source: &Path, entry: &Path) -> Result<(), String> {
    let marker = read_marker(entry)?.ok_or_else(|| {
        format!(
            "Refusing to remove user-owned native Skill {}",
            entry.display()
        )
    })?;
    validate_copy_marker(&marker, skill_id, source, entry)?;
    validate_managed_copy_layout(entry)?;
    let installed = fingerprint_file(&entry.join("SKILL.md"))?;
    if installed != marker.installed_fingerprint {
        return Err(format!(
            "Refusing to modify drifted managed Skill {}; inspect its external changes first",
            entry.display()
        ));
    }
    Ok(())
}

/// A managed fallback copy contains exactly the generated Skill and Coffee's
/// marker. Native Skill folders can contain auxiliary files, so treating an
/// unexpected entry as Coffee-owned would make disable/sync destructive.
fn validate_managed_copy_layout(entry: &Path) -> Result<(), String> {
    let mut seen = BTreeSet::new();
    let entries = fs::read_dir(entry).map_err(|error| {
        format!(
            "Failed to inspect managed Skill {}: {error}",
            entry.display()
        )
    })?;
    for child in entries {
        let child = child.map_err(|error| {
            format!(
                "Failed to inspect managed Skill {}: {error}",
                entry.display()
            )
        })?;
        let name = child.file_name();
        let rendered = name.to_string_lossy().into_owned();
        if rendered != "SKILL.md" && rendered != COPY_MARKER_FILE {
            return Err(format!(
                "{} contains an unmanaged entry '{}'; refusing to modify it",
                entry.display(),
                rendered
            ));
        }
        let metadata = fs::symlink_metadata(child.path()).map_err(|error| {
            format!(
                "Failed to inspect managed Skill entry {}: {error}",
                child.path().display()
            )
        })?;
        if !metadata.file_type().is_file() {
            return Err(format!(
                "{} contains a non-regular managed Skill entry '{}'; refusing to modify it",
                entry.display(),
                rendered
            ));
        }
        if !seen.insert(rendered.clone()) {
            return Err(format!(
                "{} contains duplicate managed Skill entry '{}'; refusing to modify it",
                entry.display(),
                rendered
            ));
        }
    }
    for required in ["SKILL.md", COPY_MARKER_FILE] {
        if !seen.contains(required) {
            return Err(format!(
                "{} is missing managed Skill entry '{}'; refusing to modify it",
                entry.display(),
                required
            ));
        }
    }
    Ok(())
}

/// Remove only the two expected Coffee files, then require the directory to
/// be empty. This deliberately avoids recursive removal after a concurrent
/// process has added an unrelated file.
fn remove_managed_copy_files(skill_id: &str, source: &Path, entry: &Path) -> Result<(), String> {
    validate_owned_copy(skill_id, source, entry)?;
    fs::remove_file(entry.join("SKILL.md")).map_err(|error| {
        format!(
            "Failed to remove managed Skill source in {}: {error}",
            entry.display()
        )
    })?;
    fs::remove_file(entry.join(COPY_MARKER_FILE)).map_err(|error| {
        format!(
            "Failed to remove managed Skill marker in {}: {error}",
            entry.display()
        )
    })?;
    fs::remove_dir(entry).map_err(|error| {
        format!(
            "Failed to remove empty managed Skill directory {}: {error}",
            entry.display()
        )
    })
}

fn expected_skill_body(
    skill_id: &str,
    skill: &skills_config::CoffeeSkill,
) -> Result<Vec<u8>, String> {
    Ok(skills_config::render_skill_markdown(skill_id, skill)?.into_bytes())
}

fn verify_source_matches_expected(
    source: &Path,
    expected_source: &[u8],
) -> Result<(), SourceCheckError> {
    let source_skill = source.join("SKILL.md");
    let metadata = match fs::symlink_metadata(&source_skill) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Err(SourceCheckError::Missing(format!(
                "Coffee source is missing: {}",
                source_skill.display()
            )));
        }
        Err(error) => {
            return Err(SourceCheckError::Error(format!(
                "Failed to inspect Coffee source {}: {error}",
                source_skill.display()
            )));
        }
    };
    if !metadata.file_type().is_file() {
        return Err(SourceCheckError::Drift(format!(
            "Coffee source {} is not a regular file",
            source_skill.display()
        )));
    }
    let body = fs::read(&source_skill).map_err(|error| {
        SourceCheckError::Error(format!(
            "Failed to read Coffee source {}: {error}",
            source_skill.display()
        ))
    })?;
    if body != expected_source {
        return Err(SourceCheckError::Drift(format!(
            "Coffee source {} differs from the Skill saved in Coffee",
            source_skill.display()
        )));
    }
    Ok(())
}

fn fingerprint_file(path: &Path) -> Result<String, String> {
    let body =
        fs::read(path).map_err(|error| format!("Failed to read {}: {error}", path.display()))?;
    Ok(skills_config::fingerprint(&body))
}

fn link_points_to(entry: &Path, source: &Path) -> Result<bool, String> {
    let target = fs::read_link(entry).map_err(|error| {
        format!(
            "Failed to read native Skill link {}: {error}",
            entry.display()
        )
    })?;
    let resolved_target = if target.is_absolute() {
        target
    } else {
        entry
            .parent()
            .ok_or_else(|| format!("Native Skill link has no parent: {}", entry.display()))?
            .join(target)
    };
    let canonical_source = fs::canonicalize(source).map_err(|error| {
        format!(
            "Failed to resolve Coffee source {}: {error}",
            source.display()
        )
    })?;
    let canonical_target = fs::canonicalize(&resolved_target).map_err(|error| {
        format!(
            "Failed to resolve native Skill link target {}: {error}",
            resolved_target.display()
        )
    })?;
    Ok(canonical_source == canonical_target)
}

fn remove_link(path: &Path) -> Result<(), String> {
    match fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(file_error) => {
            // Windows may require RemoveDirectory for a directory link. Do
            // not fall back blindly: another process may have replaced the
            // link with a real empty directory after the first unlink attempt.
            let metadata = fs::symlink_metadata(path).map_err(|inspect_error| {
                format!(
                    "Failed to remove Coffee link {}: file removal failed ({file_error}); cannot re-check path ({inspect_error})",
                    path.display()
                )
            })?;
            if !metadata.file_type().is_symlink() {
                return Err(format!(
                    "Refusing directory removal because Coffee link {} changed after inspection",
                    path.display()
                ));
            }
            fs::remove_dir(path).map_err(|directory_error| {
                format!(
                    "Failed to remove Coffee link {}: file removal failed ({file_error}); directory-link removal failed ({directory_error})",
                    path.display()
                )
            })
        }
    }
}

#[cfg(unix)]
fn create_directory_link(source: &Path, entry: &Path) -> std::io::Result<()> {
    std::os::unix::fs::symlink(source, entry)
}

#[cfg(windows)]
fn create_directory_link(source: &Path, entry: &Path) -> std::io::Result<()> {
    std::os::windows::fs::symlink_dir(source, entry)
}

#[cfg(not(any(unix, windows)))]
fn create_directory_link(_source: &Path, _entry: &Path) -> std::io::Result<()> {
    Err(std::io::Error::new(
        std::io::ErrorKind::Unsupported,
        "directory links are not supported on this platform",
    ))
}

fn unique_sibling(parent: &Path, prefix: &str) -> Result<PathBuf, String> {
    for counter in 0..10_000_u32 {
        let candidate = parent.join(format!("{prefix}-{}-{counter}", std::process::id()));
        if !candidate.exists() {
            return Ok(candidate);
        }
    }
    Err(format!(
        "Too many stale Coffee temporary directories under {}",
        parent.display()
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn temp_root(label: &str) -> PathBuf {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        std::env::temp_dir().join(format!(
            "coffee-native-skills-{label}-{}-{nonce}",
            std::process::id()
        ))
    }

    fn expected_source() -> Vec<u8> {
        skills_config::render_skill_markdown(
            "sample",
            &skills_config::CoffeeSkill {
                name: "Sample".to_string(),
                description: "Use the sample workflow.".to_string(),
                body: "Follow the sample workflow.".to_string(),
            },
        )
        .unwrap()
        .into_bytes()
    }

    fn source_at(root: &Path, body: &[u8]) -> PathBuf {
        let source = root.join("source");
        fs::create_dir_all(&source).unwrap();
        fs::write(source.join("SKILL.md"), body).unwrap();
        source
    }

    #[test]
    fn install_is_idempotent_and_disable_removes_only_our_entry() {
        let root = temp_root("install");
        let expected = expected_source();
        let source = source_at(&root, &expected);
        let native_root = root.join("native");

        let first = enable_at("sample", &expected, &source, &native_root).unwrap();
        assert!(matches!(
            first.state,
            NativeSkillStatusKind::EnabledLinked | NativeSkillStatusKind::EnabledCopied
        ));
        let second = enable_at("sample", &expected, &source, &native_root).unwrap();
        assert!(matches!(
            second.state,
            NativeSkillStatusKind::EnabledLinked | NativeSkillStatusKind::EnabledCopied
        ));
        assert_eq!(
            disable_at("sample", &source, &native_root).unwrap().state,
            NativeSkillStatusKind::Disabled
        );
        assert_eq!(
            inspect_at("sample", &expected, &source, &native_root).state,
            NativeSkillStatusKind::Disabled
        );

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn refuses_user_owned_conflicts() {
        let root = temp_root("conflict");
        let expected = expected_source();
        let source = source_at(&root, &expected);
        let native_root = root.join("native");
        let entry = target_entry_path("sample", &native_root).unwrap();
        fs::create_dir_all(&entry).unwrap();
        fs::write(entry.join("SKILL.md"), "user content").unwrap();

        assert_eq!(
            inspect_at("sample", &expected, &source, &native_root).state,
            NativeSkillStatusKind::Conflict
        );
        assert!(enable_at("sample", &expected, &source, &native_root).is_err());
        assert!(disable_at("sample", &source, &native_root).is_err());

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn copy_drift_is_detected_before_removal() {
        let root = temp_root("drift");
        let expected = expected_source();
        let source = source_at(&root, &expected);
        let native_root = root.join("native");
        let entry = target_entry_path("sample", &native_root).unwrap();
        fs::create_dir_all(&native_root).unwrap();
        create_managed_copy("sample", &source, &expected, &entry).unwrap();
        fs::write(entry.join("SKILL.md"), "external edit").unwrap();

        assert_eq!(
            inspect_at("sample", &expected, &source, &native_root).state,
            NativeSkillStatusKind::Drift
        );
        assert!(disable_at("sample", &source, &native_root).is_err());

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn copied_target_with_unknown_entries_is_drift_and_is_not_removed() {
        let root = temp_root("unknown-entry");
        let expected = expected_source();
        let source = source_at(&root, &expected);
        let native_root = root.join("native");
        let entry = target_entry_path("sample", &native_root).unwrap();
        fs::create_dir_all(&native_root).unwrap();
        create_managed_copy("sample", &source, &expected, &entry).unwrap();
        fs::write(entry.join("reference.md"), "user-owned reference").unwrap();

        assert_eq!(
            inspect_at("sample", &expected, &source, &native_root).state,
            NativeSkillStatusKind::Drift
        );
        assert!(disable_at("sample", &source, &native_root).is_err());
        assert!(entry.join("reference.md").is_file());

        let _ = fs::remove_dir_all(root);
    }

    #[cfg(unix)]
    #[test]
    fn linked_target_reports_source_drift() {
        let root = temp_root("linked-source-drift");
        let expected = expected_source();
        let source = source_at(&root, &expected);
        let native_root = root.join("native");
        let entry = target_entry_path("sample", &native_root).unwrap();
        fs::create_dir_all(&native_root).unwrap();
        create_directory_link(&source, &entry).unwrap();

        assert_eq!(
            inspect_at("sample", &expected, &source, &native_root).state,
            NativeSkillStatusKind::EnabledLinked
        );
        fs::write(source.join("SKILL.md"), "external change").unwrap();
        assert_eq!(
            inspect_at("sample", &expected, &source, &native_root).state,
            NativeSkillStatusKind::Drift
        );

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn failed_replacement_keeps_new_entry_and_staged_copy() {
        let root = temp_root("replace-recovery");
        let staged = root.join("old-copy");
        let entry = root.join("new-entry");
        fs::create_dir_all(&staged).unwrap();
        fs::write(staged.join("SKILL.md"), "old").unwrap();
        fs::create_dir_all(&entry).unwrap();
        fs::write(entry.join("user-file"), "new owner").unwrap();

        assert!(retain_staged_copy_after_failed_replace(
            &staged,
            &entry,
            "simulated replacement failure".to_string(),
        )
        .is_err());
        assert!(staged.join("SKILL.md").is_file());
        assert!(entry.join("user-file").is_file());

        let _ = fs::remove_dir_all(root);
    }
}
