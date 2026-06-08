use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use walkdir::WalkDir;

use crate::knowledge_store::{
    self, KnowledgeConfigSource, KnowledgeConfigSourceKind, KnowledgeDocument,
    KnowledgeExternalSource, KnowledgeInjectMode, KnowledgeSourceProvider, KnowledgeStorageSource,
    KnowledgeType,
};

const LOCAL_REFERENCE_MANIFEST_FILE: &str = ".locus-local-reference-manifest.json";
const MAX_LOCAL_REFERENCE_FILE_BYTES: u64 = 2 * 1024 * 1024;
const MAX_LOCAL_REFERENCE_WALK_ENTRIES: usize = 20_000;
const MAX_LOCAL_REFERENCE_IMPORT_FILES: usize = 1_000;
const MAX_LOCAL_REFERENCE_SKIPPED_REPORT: usize = 200;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LocalReferenceImportRequest {
    pub source_path: String,
    pub target_path: String,
    #[serde(default)]
    pub sync_enabled: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct LocalReferenceImportReport {
    pub target_path: String,
    pub source_path: String,
    pub imported_count: usize,
    pub skipped_count: usize,
    pub removed_count: usize,
    pub skipped: Vec<LocalReferenceSkippedFile>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct LocalReferenceSkippedFile {
    pub path: String,
    pub reason: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum LocalReferenceImportState {
    Missing,
    Ready,
    SourceMissing,
    Error,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct LocalReferenceImportStatus {
    pub state: LocalReferenceImportState,
    pub running: bool,
    pub target_path: String,
    pub source_path: String,
    pub source_locator: String,
    pub sync_enabled: bool,
    pub imported_at: Option<i64>,
    pub imported_doc_count: usize,
    pub message: String,
    pub error: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
struct LocalReferenceManifest {
    source_path: String,
    source_locator: String,
    target_path: String,
    sync_enabled: bool,
    imported_at: i64,
    files: Vec<LocalReferenceManifestEntry>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
struct LocalReferenceManifestEntry {
    source_rel_path: String,
    target_rel_path: String,
    mtime: i64,
    size: u64,
    hash: String,
}

struct LocalReferenceSourceFile {
    source_rel_path: String,
    target_rel_path: String,
    title: String,
    body: String,
    mtime: i64,
    size: u64,
    hash: String,
}

pub fn import_local_reference_snapshot(
    working_dir: &str,
    request: LocalReferenceImportRequest,
) -> Result<LocalReferenceImportReport, String> {
    let source_root = canonicalize_local_source(&request.source_path)?;
    let target_path = normalize_reference_target_path(&request.target_path)?;
    import_or_sync_local_reference_snapshot(
        working_dir,
        &source_root,
        &target_path,
        request.sync_enabled,
        None,
    )
}

pub fn sync_local_reference_snapshot(
    working_dir: &str,
    target_path: &str,
) -> Result<LocalReferenceImportReport, String> {
    let target_path = normalize_reference_target_path(target_path)?;
    let manifest = read_local_reference_manifest(working_dir, &target_path)?;
    let source_root = canonicalize_local_source(&manifest.source_path)?;
    import_or_sync_local_reference_snapshot(
        working_dir,
        &source_root,
        &target_path,
        manifest.sync_enabled,
        Some(manifest),
    )
}

pub fn get_local_reference_import_status(
    working_dir: &str,
    target_path: Option<&str>,
) -> Result<LocalReferenceImportStatus, String> {
    let normalized_target = match target_path {
        Some(path) if !path.trim().is_empty() => normalize_reference_target_path(path)?,
        _ => find_local_reference_directory(working_dir)?.unwrap_or_default(),
    };
    if normalized_target.is_empty() {
        return Ok(missing_status(
            String::new(),
            "No local reference snapshot found",
        ));
    }
    match read_local_reference_manifest(working_dir, &normalized_target) {
        Ok(manifest) => {
            let source_exists = Path::new(&manifest.source_path).exists();
            Ok(LocalReferenceImportStatus {
                state: if source_exists {
                    LocalReferenceImportState::Ready
                } else {
                    LocalReferenceImportState::SourceMissing
                },
                running: false,
                target_path: manifest.target_path,
                source_path: manifest.source_path,
                source_locator: manifest.source_locator,
                sync_enabled: manifest.sync_enabled,
                imported_at: Some(manifest.imported_at),
                imported_doc_count: manifest.files.len(),
                message: if source_exists {
                    "Local reference snapshot is ready".to_string()
                } else {
                    "Local reference source no longer exists".to_string()
                },
                error: None,
            })
        }
        Err(error) => Ok(LocalReferenceImportStatus {
            state: LocalReferenceImportState::Missing,
            running: false,
            target_path: normalized_target,
            source_path: String::new(),
            source_locator: String::new(),
            sync_enabled: false,
            imported_at: None,
            imported_doc_count: 0,
            message: "No local reference snapshot found".to_string(),
            error: if error.starts_with("Failed to read local reference manifest") {
                None
            } else {
                Some(error)
            },
        }),
    }
}

pub fn cancel_local_reference_import(
    working_dir: &str,
    target_path: Option<&str>,
) -> Result<LocalReferenceImportStatus, String> {
    get_local_reference_import_status(working_dir, target_path)
}

fn import_or_sync_local_reference_snapshot(
    working_dir: &str,
    source_root: &Path,
    target_path: &str,
    sync_enabled: bool,
    previous_manifest: Option<LocalReferenceManifest>,
) -> Result<LocalReferenceImportReport, String> {
    knowledge_store::ensure_knowledge_roots(working_dir)?;
    ensure_reference_directory(working_dir, target_path)?;

    let source_locator = file_locator(source_root)?;
    let source_id = stable_source_id(&source_locator, target_path);
    let external_source = KnowledgeExternalSource {
        provider: KnowledgeSourceProvider::LocalFolder,
        locator: Some(source_locator.clone()),
        source_id: Some(source_id),
        sync_enabled,
    };
    knowledge_store::update_directory_external_sources(
        working_dir,
        KnowledgeType::Reference,
        target_path,
        vec![external_source.clone()],
    )?;

    let mut skipped = Vec::new();
    let source_files = collect_source_files(source_root, target_path, &mut skipped)?;
    let mut next_entries = Vec::with_capacity(source_files.len());
    let mut imported_targets = BTreeSet::new();

    for source_file in source_files {
        let source_rel_path = source_file.source_rel_path.clone();
        let target_rel_path = source_file.target_rel_path.clone();
        let mtime = source_file.mtime;
        let size = source_file.size;
        let hash = source_file.hash.clone();
        imported_targets.insert(target_rel_path.clone());
        save_local_reference_document(working_dir, source_file, external_source.clone())?;
        next_entries.push(LocalReferenceManifestEntry {
            source_rel_path,
            target_rel_path,
            mtime,
            size,
            hash,
        });
    }

    let removed_count = remove_stale_snapshot_documents(
        working_dir,
        previous_manifest.as_ref(),
        &imported_targets,
    )?;

    let manifest = LocalReferenceManifest {
        source_path: source_root.to_string_lossy().to_string(),
        source_locator,
        target_path: target_path.to_string(),
        sync_enabled,
        imported_at: chrono::Utc::now().timestamp_millis(),
        files: next_entries,
    };
    write_local_reference_manifest(working_dir, target_path, &manifest)?;

    Ok(LocalReferenceImportReport {
        target_path: target_path.to_string(),
        source_path: source_root.to_string_lossy().to_string(),
        imported_count: manifest.files.len(),
        skipped_count: skipped.len(),
        removed_count,
        skipped,
    })
}

fn missing_status(target_path: String, message: &str) -> LocalReferenceImportStatus {
    LocalReferenceImportStatus {
        state: LocalReferenceImportState::Missing,
        running: false,
        target_path,
        source_path: String::new(),
        source_locator: String::new(),
        sync_enabled: false,
        imported_at: None,
        imported_doc_count: 0,
        message: message.to_string(),
        error: None,
    }
}

fn find_local_reference_directory(working_dir: &str) -> Result<Option<String>, String> {
    knowledge_store::find_reference_directory_by_external_provider(
        working_dir,
        KnowledgeSourceProvider::LocalFolder,
    )
    .map(|record| record.map(|record| record.path))
}

fn ensure_reference_directory(working_dir: &str, target_path: &str) -> Result<(), String> {
    knowledge_store::create_directory(working_dir, KnowledgeType::Reference, target_path)?;
    let mut config = knowledge_store::default_directory_config_for_type(KnowledgeType::Reference);
    config.inject_mode = KnowledgeInjectMode::None;
    config.inherit_inject_mode = false;
    config.ai_maintained = false;
    config.inherit_ai_config = false;
    config.explicit_maintenance_rules = false;
    config.allow_create_documents = false;
    config.allow_create_directories = false;
    config.allow_move_documents = false;
    config.allow_move_directories = false;
    knowledge_store::update_directory_config(
        working_dir,
        KnowledgeType::Reference,
        target_path,
        config,
    )?;
    Ok(())
}

fn collect_source_files(
    source_root: &Path,
    target_path: &str,
    skipped: &mut Vec<LocalReferenceSkippedFile>,
) -> Result<Vec<LocalReferenceSourceFile>, String> {
    let source_is_file = source_root.is_file();
    let mut files = Vec::new();
    let walker = if source_is_file {
        WalkDir::new(source_root).max_depth(1)
    } else {
        WalkDir::new(source_root)
    };
    let mut visited_entries = 0usize;

    for entry in walker
        .into_iter()
        .filter_entry(|entry| should_visit_source_entry(source_root, entry.path()))
        .filter_map(Result::ok)
    {
        visited_entries += 1;
        if visited_entries > MAX_LOCAL_REFERENCE_WALK_ENTRIES {
            return Err(format!(
                "Local reference source is too large to import as a snapshot. Select a narrower folder or split the source. Scanned more than {} entries.",
                MAX_LOCAL_REFERENCE_WALK_ENTRIES
            ));
        }
        if entry.file_type().is_dir() {
            continue;
        }
        if !entry.file_type().is_file() {
            continue;
        }
        match build_source_file(source_root, entry.path(), target_path, source_is_file) {
            Ok(Some(file)) => {
                files.push(file);
                if files.len() > MAX_LOCAL_REFERENCE_IMPORT_FILES {
                    return Err(format!(
                        "Local reference source contains more than {} supported text files. Select a narrower folder or split the source.",
                        MAX_LOCAL_REFERENCE_IMPORT_FILES
                    ));
                }
            }
            Ok(None) => {}
            Err(reason) => push_skipped_file(skipped, entry.path(), reason),
        }
    }
    files.sort_by(|left, right| left.target_rel_path.cmp(&right.target_rel_path));
    Ok(files)
}

fn should_visit_source_entry(source_root: &Path, path: &Path) -> bool {
    if path == source_root {
        return true;
    }
    let Some(name) = path.file_name().and_then(|value| value.to_str()) else {
        return true;
    };
    if name.starts_with('.') {
        return false;
    }
    !matches!(
        name.to_ascii_lowercase().as_str(),
        "node_modules"
            | "target"
            | "dist"
            | "build"
            | "out"
            | "coverage"
            | "vendor"
            | "__pycache__"
    )
}

fn push_skipped_file(skipped: &mut Vec<LocalReferenceSkippedFile>, path: &Path, reason: String) {
    if skipped.len() >= MAX_LOCAL_REFERENCE_SKIPPED_REPORT {
        return;
    }
    skipped.push(LocalReferenceSkippedFile {
        path: path.to_string_lossy().replace('\\', "/"),
        reason,
    });
}

fn build_source_file(
    source_root: &Path,
    source_path: &Path,
    target_path: &str,
    source_is_file: bool,
) -> Result<Option<LocalReferenceSourceFile>, String> {
    use sha2::Digest;

    let extension = source_path
        .extension()
        .and_then(|value| value.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase();
    if !matches!(extension.as_str(), "md" | "markdown" | "txt") {
        return Err("unsupported file type".to_string());
    }
    let metadata = std::fs::metadata(source_path)
        .map_err(|error| format!("failed to read metadata: {}", error))?;
    if metadata.len() > MAX_LOCAL_REFERENCE_FILE_BYTES {
        return Err("file too large".to_string());
    }
    let raw = std::fs::read_to_string(source_path)
        .map_err(|error| format!("failed to read text: {}", error))?;
    let source_rel_path = if source_is_file {
        source_path
            .file_name()
            .and_then(|value| value.to_str())
            .unwrap_or("document.md")
            .to_string()
    } else {
        source_path
            .strip_prefix(source_root)
            .map_err(|error| format!("failed to resolve relative path: {}", error))?
            .to_string_lossy()
            .replace('\\', "/")
    };
    let target_rel_path = format!(
        "{}/{}",
        target_path,
        source_to_target_markdown_path(&source_rel_path)?
    );
    let hash = format!("{:x}", sha2::Sha256::digest(raw.as_bytes()));
    let (title, body) = if matches!(extension.as_str(), "md" | "markdown") {
        markdown_title_and_body(&raw, source_path)
    } else {
        (title_from_path(source_path), raw)
    };
    let mtime = metadata
        .modified()
        .ok()
        .and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|duration| duration.as_secs() as i64)
        .unwrap_or(0);
    Ok(Some(LocalReferenceSourceFile {
        source_rel_path,
        target_rel_path,
        title,
        body,
        mtime,
        size: metadata.len(),
        hash,
    }))
}

fn save_local_reference_document(
    working_dir: &str,
    source_file: LocalReferenceSourceFile,
    external_source: KnowledgeExternalSource,
) -> Result<(), String> {
    let now = chrono::Utc::now().timestamp_millis();
    let document = KnowledgeDocument {
        id: format!(
            "reference-local-{}",
            stable_source_id(&source_file.target_rel_path, &source_file.hash)
        ),
        doc_type: KnowledgeType::Reference,
        path: source_file.target_rel_path,
        title: source_file.title,
        inject_mode: KnowledgeInjectMode::None,
        inherit_inject_mode: false,
        inject_mode_source: KnowledgeConfigSource {
            kind: KnowledgeConfigSourceKind::SelfValue,
            path: None,
        },
        summary_enabled: false,
        command_enabled: false,
        read_only: true,
        ai_maintained: false,
        storage_source: KnowledgeStorageSource::Project,
        inherit_ai_config: true,
        ai_config_source: Default::default(),
        explicit_maintenance_rules: false,
        external_source: Some(external_source),
        skill_enabled: None,
        skill_surface: None,
        command_trigger: None,
        argument_hint: None,
        tools: Vec::new(),
        summary: None,
        body: source_file.body,
        maintenance_rules: None,
        created_at: now,
        updated_at: now,
    };
    knowledge_store::save_document(working_dir, document)?;
    Ok(())
}

fn remove_stale_snapshot_documents(
    working_dir: &str,
    previous_manifest: Option<&LocalReferenceManifest>,
    imported_targets: &BTreeSet<String>,
) -> Result<usize, String> {
    let Some(previous_manifest) = previous_manifest else {
        return Ok(0);
    };
    let mut removed = 0usize;
    for entry in &previous_manifest.files {
        if imported_targets.contains(&entry.target_rel_path) {
            continue;
        }
        let full_path = knowledge_store::knowledge_root(working_dir)
            .join(KnowledgeType::Reference.as_str())
            .join(&entry.target_rel_path);
        if full_path.is_file() {
            std::fs::remove_file(&full_path).map_err(|error| {
                format!(
                    "Failed to remove stale local reference document '{}': {}",
                    full_path.display(),
                    error
                )
            })?;
            removed += 1;
        }
    }
    Ok(removed)
}

fn read_local_reference_manifest(
    working_dir: &str,
    target_path: &str,
) -> Result<LocalReferenceManifest, String> {
    let path = local_reference_manifest_path(working_dir, target_path);
    let raw = std::fs::read_to_string(&path).map_err(|error| {
        format!(
            "Failed to read local reference manifest '{}': {}",
            path.display(),
            error
        )
    })?;
    serde_json::from_str(&raw).map_err(|error| {
        format!(
            "Failed to parse local reference manifest '{}': {}",
            path.display(),
            error
        )
    })
}

fn write_local_reference_manifest(
    working_dir: &str,
    target_path: &str,
    manifest: &LocalReferenceManifest,
) -> Result<(), String> {
    let path = local_reference_manifest_path(working_dir, target_path);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|error| {
            format!(
                "Failed to create local reference manifest directory '{}': {}",
                parent.display(),
                error
            )
        })?;
    }
    let raw = serde_json::to_string_pretty(manifest)
        .map_err(|error| format!("Failed to serialize local reference manifest: {}", error))?;
    std::fs::write(&path, raw).map_err(|error| {
        format!(
            "Failed to write local reference manifest '{}': {}",
            path.display(),
            error
        )
    })
}

fn local_reference_manifest_path(working_dir: &str, target_path: &str) -> PathBuf {
    knowledge_store::knowledge_root(working_dir)
        .join(KnowledgeType::Reference.as_str())
        .join(target_path)
        .join(LOCAL_REFERENCE_MANIFEST_FILE)
}

fn normalize_reference_target_path(path: &str) -> Result<String, String> {
    let trimmed = path.trim().replace('\\', "/");
    let without_prefix = trimmed.strip_prefix("reference/").unwrap_or(&trimmed);
    let normalized = knowledge_store::ensure_directory_path(without_prefix)?;
    if normalized.is_empty() {
        return Err("Local reference target path is required".to_string());
    }
    Ok(normalized)
}

fn canonicalize_local_source(path: &str) -> Result<PathBuf, String> {
    let source = dunce::canonicalize(path.trim())
        .map_err(|error| format!("Failed to resolve local reference source: {}", error))?;
    if !source.is_file() && !source.is_dir() {
        return Err("Local reference source must be a file or directory".to_string());
    }
    Ok(source)
}

fn source_to_target_markdown_path(source_rel_path: &str) -> Result<String, String> {
    let normalized = source_rel_path.replace('\\', "/");
    let path = Path::new(&normalized);
    let stem = path
        .file_stem()
        .and_then(|value| value.to_str())
        .filter(|value| !value.trim().is_empty())
        .ok_or_else(|| format!("Invalid source file path: {}", source_rel_path))?;
    let parent = path
        .parent()
        .map(|value| value.to_string_lossy().replace('\\', "/"))
        .unwrap_or_default();
    let file_name = format!("{}.md", stem);
    let target = if parent.is_empty() || parent == "." {
        file_name
    } else {
        format!("{}/{}", parent, file_name)
    };
    knowledge_store::ensure_document_path(&target)
}

fn markdown_title_and_body(raw: &str, path: &Path) -> (String, String) {
    let normalized = raw.replace("\r\n", "\n").replace('\r', "\n");
    let mut lines = normalized.lines();
    if let Some(first) = lines.next() {
        if let Some(title) = first.trim().strip_prefix("# ") {
            let body = lines.collect::<Vec<_>>().join("\n").trim().to_string();
            return (title.trim().to_string(), body);
        }
    }
    (title_from_path(path), normalized)
}

fn title_from_path(path: &Path) -> String {
    path.file_stem()
        .and_then(|value| value.to_str())
        .unwrap_or("document")
        .to_string()
}

fn file_locator(path: &Path) -> Result<String, String> {
    let normalized = path.to_string_lossy().replace('\\', "/");
    if normalized.starts_with('/') {
        Ok(format!("file://{}", normalized))
    } else {
        Ok(format!("file:///{}", normalized))
    }
}

fn stable_source_id(left: &str, right: &str) -> String {
    use sha2::Digest;
    let mut hasher = sha2::Sha256::new();
    hasher.update(left.as_bytes());
    hasher.update(b"\0");
    hasher.update(right.as_bytes());
    let digest = hasher.finalize();
    format!("{:x}", digest)[..16].to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::knowledge_store::{self, KnowledgeSourceProvider, KnowledgeType};
    use tempfile::TempDir;

    #[test]
    fn imports_local_folder_as_read_only_reference_snapshot() {
        let workspace = TempDir::new().expect("workspace");
        let source = TempDir::new().expect("source");
        std::fs::create_dir_all(source.path().join("nested")).expect("nested");
        std::fs::write(source.path().join("guide.md"), "# Guide\n\nUse cover.").expect("guide");
        std::fs::write(source.path().join("nested").join("notes.txt"), "AI notes").expect("notes");
        std::fs::write(source.path().join("image.png"), [0u8, 1, 2]).expect("image");

        let report = import_local_reference_snapshot(
            &workspace.path().to_string_lossy(),
            LocalReferenceImportRequest {
                source_path: source.path().to_string_lossy().to_string(),
                target_path: "reference/local-docs".to_string(),
                sync_enabled: false,
            },
        )
        .expect("import local reference");

        assert_eq!(report.target_path, "local-docs");
        assert_eq!(report.imported_count, 2);
        assert_eq!(report.skipped_count, 1);

        let guide = knowledge_store::load_document_by_path(
            &workspace.path().to_string_lossy(),
            KnowledgeType::Reference,
            "local-docs/guide.md",
        )
        .expect("guide doc");
        assert!(guide.read_only);
        assert_eq!(guide.inject_mode, KnowledgeInjectMode::None);
        assert!(!guide.inherit_inject_mode);
        assert_eq!(
            guide.external_source.as_ref().map(|source| source.provider),
            Some(KnowledgeSourceProvider::LocalFolder)
        );
        assert_eq!(guide.body.trim(), "Use cover.");

        let notes = knowledge_store::load_document_by_path(
            &workspace.path().to_string_lossy(),
            KnowledgeType::Reference,
            "local-docs/nested/notes.md",
        )
        .expect("notes doc");
        assert_eq!(notes.body.trim(), "AI notes");

        let binding = knowledge_store::read_directory_config(
            &workspace.path().to_string_lossy(),
            KnowledgeType::Reference,
            "local-docs",
        )
        .expect("directory config");
        assert_eq!(binding.config.inject_mode, KnowledgeInjectMode::None);
        assert!(!binding.config.inherit_inject_mode);
        assert_eq!(binding.external_sources.len(), 1);
        assert_eq!(
            binding.external_sources[0].provider,
            KnowledgeSourceProvider::LocalFolder
        );
        assert!(binding.external_sources[0]
            .locator
            .as_deref()
            .unwrap_or_default()
            .starts_with("file:///"));
    }

    #[test]
    fn import_skips_common_generated_directories() {
        let workspace = TempDir::new().expect("workspace");
        let source = TempDir::new().expect("source");
        std::fs::create_dir_all(source.path().join("node_modules/pkg")).expect("node_modules");
        std::fs::create_dir_all(source.path().join("target/debug")).expect("target");
        std::fs::write(source.path().join("guide.md"), "# Guide\n\nUseful").expect("guide");
        std::fs::write(
            source.path().join("node_modules/pkg/readme.md"),
            "# Dependency\n\nSkip",
        )
        .expect("dependency");
        std::fs::write(source.path().join("target/debug/generated.txt"), "Skip")
            .expect("generated");

        let report = import_local_reference_snapshot(
            &workspace.path().to_string_lossy(),
            LocalReferenceImportRequest {
                source_path: source.path().to_string_lossy().to_string(),
                target_path: "reference/local-docs".to_string(),
                sync_enabled: false,
            },
        )
        .expect("import local reference");

        assert_eq!(report.imported_count, 1);
        assert!(knowledge_store::load_document_by_path(
            &workspace.path().to_string_lossy(),
            KnowledgeType::Reference,
            "local-docs/guide.md",
        )
        .is_ok());
        assert!(knowledge_store::load_document_by_path(
            &workspace.path().to_string_lossy(),
            KnowledgeType::Reference,
            "local-docs/node_modules/pkg/readme.md",
        )
        .is_err());
    }

    #[test]
    fn sync_local_reference_snapshot_adds_updates_and_removes_documents() {
        let workspace = TempDir::new().expect("workspace");
        let source = TempDir::new().expect("source");
        std::fs::write(source.path().join("guide.md"), "# Guide\n\nOriginal").expect("guide");

        import_local_reference_snapshot(
            &workspace.path().to_string_lossy(),
            LocalReferenceImportRequest {
                source_path: source.path().to_string_lossy().to_string(),
                target_path: "reference/local-docs".to_string(),
                sync_enabled: true,
            },
        )
        .expect("initial import");

        std::fs::write(source.path().join("guide.md"), "# Guide\n\nUpdated").expect("update");
        std::fs::write(source.path().join("new.txt"), "New note").expect("new");

        let report = sync_local_reference_snapshot(
            &workspace.path().to_string_lossy(),
            "reference/local-docs",
        )
        .expect("sync local reference");

        assert_eq!(report.imported_count, 2);
        assert_eq!(report.removed_count, 0);

        let guide = knowledge_store::load_document_by_path(
            &workspace.path().to_string_lossy(),
            KnowledgeType::Reference,
            "local-docs/guide.md",
        )
        .expect("guide doc");
        assert_eq!(guide.body.trim(), "Updated");

        let new_doc = knowledge_store::load_document_by_path(
            &workspace.path().to_string_lossy(),
            KnowledgeType::Reference,
            "local-docs/new.md",
        )
        .expect("new doc");
        assert_eq!(new_doc.body.trim(), "New note");

        std::fs::remove_file(source.path().join("guide.md")).expect("remove guide");
        let report = sync_local_reference_snapshot(
            &workspace.path().to_string_lossy(),
            "reference/local-docs",
        )
        .expect("sync remove");
        assert_eq!(report.removed_count, 1);
        assert!(knowledge_store::load_document_by_path(
            &workspace.path().to_string_lossy(),
            KnowledgeType::Reference,
            "local-docs/guide.md",
        )
        .is_err());
    }
}
