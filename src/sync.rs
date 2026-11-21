use anyhow::{bail, Context, Result};
use std::path::Path;
use tracing::{info, warn};

use crate::cache::CacheManager;
use crate::db::Database;
use crate::filter::DevFilter;
use crate::snapshot::SnapshotEngine;
use crate::wal::{ChangeType, WalManager};

#[derive(Debug, PartialEq, Eq)]
pub enum SyncFileResult {
    Uploaded(u64),
    Conflict(String),
    Unchanged,
}

#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct SyncReport {
    pub files_uploaded: usize,
    pub files_deleted: usize,
    pub bytes_transferred: u64,
    pub conflicts_detected: usize,
    pub auto_snapshot_id: Option<String>,
}

#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct RestoreReport {
    pub files_restored: usize,
    pub files_skipped: usize,
    pub bytes: u64,
    pub hints: Vec<String>,
}

#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct BundleSyncReport {
    pub files_packaged: usize,
    pub original_bytes: u64,
    pub bundle_bytes: u64,
    pub bundle_path: String,
    pub manifest_path: String,
    pub auto_snapshot_id: Option<String>,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct BundleFileEntry {
    pub path: String,
    pub size_bytes: u64,
    pub mtime: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sha256: Option<String>,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct BundleManifest {
    pub version: String,
    pub project_name: String,
    pub created_at: String,
    pub total_files: usize,
    pub total_bytes: u64,
    pub entries: Vec<BundleFileEntry>,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct RemoteProjectSummary {
    pub name: String,
    pub path: String,
    pub file_count: usize,
    pub total_bytes: u64,
    pub last_modified: Option<String>,
    pub original_local_path: Option<String>,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct DevDriveMeta {
    pub project_name: String,
    pub original_local_path: String,
    pub created_at: String,
    #[serde(default = "default_meta_version")]
    pub devdrive_version: String,
}

fn default_meta_version() -> String {
    "0.1.0".to_string()
}

/// Validates that a project name contains no path traversal sequences or invalid characters
pub fn validate_project_name(name: &str) -> Result<()> {
    let trimmed = name.trim();
    if trimmed.is_empty() {
        bail!("Proje adı boş olamaz.");
    }
    if trimmed == "." || trimmed == ".." {
        bail!("Proje adı '.' veya '..' olamaz.");
    }
    if trimmed.contains('/') || trimmed.contains('\\') || trimmed.contains("..") {
        bail!("Proje adı geçersiz karakterler veya yol ayracı ('/', '\\', '..') içeremez.");
    }
    Ok(())
}

/// Writes metadata file .devdrive-meta.json to the root of the remote backup directory
pub fn write_devdrive_meta(remote_path: &Path, project_name: &str, original_local_path: &str) -> Result<()> {
    if !remote_path.exists() {
        std::fs::create_dir_all(remote_path)?;
    }
    let meta = DevDriveMeta {
        project_name: project_name.to_string(),
        original_local_path: original_local_path.to_string(),
        created_at: chrono::Utc::now().to_rfc3339(),
        devdrive_version: "0.1.0".to_string(),
    };
    let meta_path = remote_path.join(".devdrive-meta.json");
    let json = serde_json::to_string_pretty(&meta)?;
    std::fs::write(&meta_path, json)?;
    Ok(())
}

/// Reads .devdrive-meta.json from remote directory if available
pub fn read_devdrive_meta(remote_path: &Path) -> Option<DevDriveMeta> {
    let meta_path = remote_path.join(".devdrive-meta.json");
    if meta_path.exists()
        && let Ok(content) = std::fs::read_to_string(&meta_path)
            && let Ok(meta) = serde_json::from_str::<DevDriveMeta>(&content) {
                return Some(meta);
            }
    None
}

/// Reads bundle manifest.json from remote project directory if available
pub fn read_bundle_manifest<P: AsRef<Path>>(remote_project_dir: P) -> Option<BundleManifest> {
    let manifest_path = remote_project_dir.as_ref().join("manifest.json");
    if manifest_path.exists()
        && let Ok(content) = std::fs::read_to_string(&manifest_path)
            && let Ok(manifest) = serde_json::from_str::<BundleManifest>(&content) {
                return Some(manifest);
            }
    None
}

/// Scans remote_drive_base_path for backed up projects
pub fn list_remote_projects<P: AsRef<Path>>(base_path: P) -> Result<Vec<RemoteProjectSummary>> {
    let base = base_path.as_ref();
    if !base.exists() || !base.is_dir() {
        return Ok(Vec::new());
    }

    let mut projects = Vec::new();

    let entries = std::fs::read_dir(base)?;
    for entry in entries.filter_map(|e| e.ok()) {
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }

        let dir_name = entry.file_name().to_string_lossy().to_string();
        if dir_name.starts_with('.') {
            continue;
        }

        let filter = DevFilter::for_project(&path);
        let meta = read_devdrive_meta(&path);
        let project_name = meta
            .as_ref()
            .map(|m| m.project_name.clone())
            .unwrap_or_else(|| dir_name.clone());
        let original_path = meta.map(|m| m.original_local_path);

        let mut file_count = 0;
        let mut total_bytes = 0u64;
        let mut latest_mtime: Option<std::time::SystemTime> = None;

        if let Some(bundle_manifest) = read_bundle_manifest(&path) {
            file_count = bundle_manifest.total_files;
            total_bytes = bundle_manifest.total_bytes;
            let bundle_tar = path.join("bundle.tar.gz");
            if let Ok(m) = bundle_tar.metadata().and_then(|meta| meta.modified()) {
                latest_mtime = Some(m);
            }
        } else {
            for f in walkdir::WalkDir::new(&path).into_iter().filter_map(|e| e.ok()) {
                if !f.file_type().is_file() {
                    continue;
                }
                if f.path().components().any(|c| c.as_os_str() == ".trash") {
                    continue;
                }
                let fname = f.file_name().to_string_lossy();
                if fname.contains(".conflict_") || fname == ".devdrive-meta.json" {
                    continue;
                }
                if let Ok(rel) = f.path().strip_prefix(&path)
                    && filter.categorize(rel) == crate::filter::FileCategory::Source {
                        file_count += 1;
                        if let Ok(m) = f.metadata() {
                            total_bytes += m.len();
                            if let Ok(modified) = m.modified() {
                                latest_mtime = match latest_mtime {
                                    Some(curr) => Some(curr.max(modified)),
                                    None => Some(modified),
                                };
                            }
                        }
                    }
            }
        }

        let last_modified = latest_mtime.map(|t| {
            let dt: chrono::DateTime<chrono::Utc> = t.into();
            dt.to_rfc3339()
        });

        projects.push(RemoteProjectSummary {
            name: project_name,
            path: path.to_string_lossy().to_string(),
            file_count,
            total_bytes,
            last_modified,
            original_local_path: original_path,
        });
    }

    projects.sort_by_key(|a| a.name.to_lowercase());
    Ok(projects)
}

pub struct SyncEngine<'a> {
    db: &'a mut Database,
    cache_mgr: &'a CacheManager,
    _filter: DevFilter,
}

impl<'a> SyncEngine<'a> {
    pub fn new(db: &'a mut Database, cache_mgr: &'a CacheManager) -> Self {
        Self {
            db,
            cache_mgr,
            _filter: DevFilter::new(),
        }
    }

    /// Safely synchronizes a file with Conflict Detection and SHA-256 Checksum Verification
    pub fn safe_sync_file(src_file: &Path, dst_file: &Path) -> Result<SyncFileResult> {
        if !src_file.exists() || !src_file.is_file() {
            return Ok(SyncFileResult::Unchanged);
        }

        if let Some(parent) = dst_file.parent()
            && std::fs::create_dir_all(parent).is_err() {
                // If parent or an ancestor exists as a 0-byte non-directory file (e.g. Google Drive stub collision), clean it up
                let mut cur = parent.to_path_buf();
                while let Some(p) = cur.parent() {
                    if cur.is_file()
                        && let Ok(m) = cur.metadata()
                            && m.len() == 0 {
                                let _ = std::fs::remove_file(&cur);
                            }
                    cur = p.to_path_buf();
                }
                std::fs::create_dir_all(parent)?;
            }


        // Hızlı Stat Kontrolü: Boyut ve mtime (saniye bazında) aynıysa SHA-256 hesaplamadan atla
        if dst_file.exists()
            && let (Ok(meta_src), Ok(meta_dst)) = (src_file.metadata(), dst_file.metadata())
                && meta_src.len() == meta_dst.len() {
                    let mtime_src = filetime::FileTime::from_last_modification_time(&meta_src);
                    let mtime_dst = filetime::FileTime::from_last_modification_time(&meta_dst);
                    if mtime_src.seconds() == mtime_dst.seconds() {
                        return Ok(SyncFileResult::Unchanged);
                    }
                }

        let src_hash = CacheManager::compute_sha256(src_file)?;

        // If destination exists, check for conflict
        if dst_file.exists() {
            let dst_hash = CacheManager::compute_sha256(dst_file)?;
            if src_hash == dst_hash {
                if let Ok(meta_src) = src_file.metadata() {
                    let mtime_src = filetime::FileTime::from_last_modification_time(&meta_src);
                    let _ = filetime::set_file_mtime(dst_file, mtime_src);
                }
                return Ok(SyncFileResult::Unchanged);
            }

            // Hashes differ. Check timestamps to detect conflicts.
            if let (Ok(meta_src), Ok(meta_dst)) = (src_file.metadata(), dst_file.metadata())
                && let (Ok(m_src), Ok(m_dst)) = (meta_src.modified(), meta_dst.modified()) {
                    // If remote file is strictly newer than local, do NOT overwrite! Create a conflict file.
                    if m_dst > m_src {
                        let timestamp = chrono::Utc::now().format("%Y%m%d_%H%M%S");
                        let file_stem = dst_file.file_stem().and_then(|s| s.to_str()).unwrap_or("file");
                        let ext = dst_file.extension().and_then(|e| e.to_str()).unwrap_or("");
                        let conflict_name = if ext.is_empty() {
                            format!("{}.conflict_{}", file_stem, timestamp)
                        } else {
                            format!("{}.conflict_{}.{}", file_stem, timestamp, ext)
                        };

                        let conflict_path = dst_file.parent().unwrap_or(dst_file).join(conflict_name);
                        std::fs::copy(src_file, &conflict_path)?;
                        warn!(
                            "⚠️ Conflict Shield: Remote file {} is newer! Preserved original and saved conflict copy to {}",
                            dst_file.display(),
                            conflict_path.display()
                        );
                        return Ok(SyncFileResult::Conflict(conflict_path.to_string_lossy().to_string()));
                    }
                }
        }

        // Perform safe in-place copy
        CacheManager::clone_file(src_file, dst_file)
            .with_context(|| format!("Failed to copy file to {}", dst_file.display()))?;

        // Transfer modification time from local source to remote destination so mtimes match
        if let Ok(meta) = src_file.metadata() {
            let mtime = filetime::FileTime::from_last_modification_time(&meta);
            let _ = filetime::set_file_mtime(dst_file, mtime);
        }

        // SHA-256 Checksum Integrity Verification
        let verified_dst_hash = CacheManager::compute_sha256(dst_file)?;
        if src_hash != verified_dst_hash {
            bail!(
                "Integrity failure: Checksum mismatch on {}. Expected {}, got {}",
                dst_file.display(),
                src_hash,
                verified_dst_hash
            );
        }

        let bytes = src_file.metadata()?.len();
        Ok(SyncFileResult::Uploaded(bytes))
    }

    /// Moves a deleted file to the remote .trash vault instead of destroying it
    pub fn safe_soft_delete(dst_file: &Path, remote_root: &Path) -> Result<bool> {
        if !dst_file.exists() {
            return Ok(false);
        }

        let trash_dir = remote_root.join(".trash");
        std::fs::create_dir_all(&trash_dir)?;

        let file_name = dst_file.file_name().and_then(|f| f.to_str()).unwrap_or("file");
        let timestamp = chrono::Utc::now().format("%Y%m%d_%H%M%S");
        let mut trash_target = trash_dir.join(format!("{}_{}", timestamp, file_name));
        let mut counter = 0;
        while trash_target.exists() {
            counter += 1;
            trash_target = trash_dir.join(format!("{}_{}_{}", timestamp, counter, file_name));
        }

        std::fs::rename(dst_file, &trash_target)?;
        info!("Vault Soft-Delete: Safely moved deleted file to {}", trash_target.display());
        Ok(true)
    }

    /// Synchronizes pending WAL events for a project to the remote Drive directory
    pub fn sync_pending<P: AsRef<Path>, Q: AsRef<Path>>(
        &mut self,
        local_project_path: P,
        remote_drive_path: Q,
        auto_snapshot: bool,
    ) -> Result<SyncReport> {
        let local_path = local_project_path.as_ref().canonicalize()?;
        let remote_path = remote_drive_path.as_ref().to_path_buf();
        std::fs::create_dir_all(&remote_path)?;

        let auto_snap_id = if auto_snapshot {
            let mut engine = SnapshotEngine::new(self.db, self.cache_mgr);
            match engine.create_snapshot(&local_path, "Auto-snapshot before sync (Pre-sync shield)") {
                Ok(manifest) => {
                    info!("📸 Pre-Sync Auto-Snapshot captured: {}", manifest.id);
                    Some(manifest.id)
                }
                Err(err) => {
                    warn!("Could not capture pre-sync auto-snapshot: {}", err);
                    None
                }
            }
        } else {
            None
        };

        let project_opt = self.db.get_project_by_local_path(&local_path.to_string_lossy())?;

        let pending = if let Some(ref proj) = project_opt {
            let wal = WalManager::new(self.db);
            wal.get_debounced_pending(proj.id)?
        } else {
            Vec::new()
        };

        let filter = DevFilter::for_project(&local_path);

        let mut uploaded = 0;
        let mut deleted = 0;
        let mut conflicts = 0;
        let mut bytes = 0u64;
        let mut completed_ids = Vec::new();

        for event in &pending {
            let rel = Path::new(&event.relative_path);
            if !filter.is_syncable(rel) {
                tracing::info!("Shielding non-syncable pending event from Drive: {}", event.relative_path);
                completed_ids.extend(event.all_ids());
                continue;
            }

            let src_file = local_path.join(rel);
            let dst_file = remote_path.join(rel);

            match ChangeType::from_str(&event.event_type) {
                ChangeType::Created | ChangeType::Modified => {
                    match Self::safe_sync_file(&src_file, &dst_file) {
                        Ok(SyncFileResult::Uploaded(b)) => {
                            bytes += b;
                            uploaded += 1;
                            completed_ids.extend(event.all_ids());
                            if let Some(ref proj) = project_opt
                                && let Ok(meta) = src_file.metadata() {
                                    let hash = CacheManager::compute_sha256(&src_file).unwrap_or_default();
                                    let mtime = meta.modified().ok()
                                        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                                        .map(|d| d.as_secs() as i64)
                                        .unwrap_or(0);
                                    let _ = self.db.upsert_file_metadata(
                                        proj.id,
                                        &event.relative_path,
                                        meta.len() as i64,
                                        &hash,
                                        "Source",
                                        mtime,
                                        mtime,
                                        true,
                                    );
                                }
                        }
                        Ok(SyncFileResult::Conflict(_)) => {
                            conflicts += 1;
                            completed_ids.extend(event.all_ids());
                        }
                        Ok(SyncFileResult::Unchanged) => {
                            completed_ids.extend(event.all_ids());
                            if let Some(ref proj) = project_opt
                                && let Ok(meta) = src_file.metadata() {
                                    let hash = CacheManager::compute_sha256(&src_file).unwrap_or_default();
                                    let mtime = meta.modified().ok()
                                        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                                        .map(|d| d.as_secs() as i64)
                                        .unwrap_or(0);
                                    let _ = self.db.upsert_file_metadata(
                                        proj.id,
                                        &event.relative_path,
                                        meta.len() as i64,
                                        &hash,
                                        "Source",
                                        mtime,
                                        mtime,
                                        true,
                                    );
                                }
                        }
                        Err(e) => warn!("Sync error on {}: {}", event.relative_path, e),
                    }
                }
                ChangeType::Deleted => {
                    match Self::safe_soft_delete(&dst_file, &remote_path) {
                        Ok(true) => {
                            deleted += 1;
                            completed_ids.extend(event.all_ids());
                            if let Some(ref proj) = project_opt {
                                let _ = self.db.delete_file_metadata(proj.id, &event.relative_path);
                            }
                        }
                        Ok(false) => {
                            completed_ids.extend(event.all_ids());
                        }
                        Err(e) => {
                            warn!("Safe soft delete error on {}: {}", event.relative_path, e);
                        }
                    }
                }
            }
        }

        self.db.mark_wal_events_synced(&completed_ids)?;

        if let Some(ref proj) = project_opt {
            let now = chrono::Utc::now().to_rfc3339();
            let _ = self.db.update_project_last_synced(proj.id, &now);
        }

        info!(
            "Sync completed: {} uploaded, {} deleted, {} conflicts, {} bytes transferred",
            uploaded, deleted, conflicts, bytes
        );

        Ok(SyncReport {
            files_uploaded: uploaded,
            files_deleted: deleted,
            bytes_transferred: bytes,
            conflicts_detected: conflicts,
            auto_snapshot_id: auto_snap_id,
        })
    }

    /// Performs a full synchronization of all source files in the project to remote Drive
    pub fn sync_all<P: AsRef<Path>, Q: AsRef<Path>>(
        &mut self,
        local_project_path: P,
        remote_drive_path: Q,
        auto_snapshot: bool,
    ) -> Result<SyncReport> {
        self.sync_all_with_options(local_project_path, remote_drive_path, auto_snapshot, false, false, |_, _| {})
    }

    /// Performs a full synchronization with a live progress callback (processed, total)
    pub fn sync_all_with_progress<P: AsRef<Path>, Q: AsRef<Path>, F: FnMut(usize, usize)>(
        &mut self,
        local_project_path: P,
        remote_drive_path: Q,
        auto_snapshot: bool,
        progress_cb: F,
    ) -> Result<SyncReport> {
        self.sync_all_with_options(local_project_path, remote_drive_path, auto_snapshot, false, false, progress_cb)
    }

    /// Performs a full synchronization with options for pruning remote deletions and forcing thresholds
    pub fn sync_all_with_options<P: AsRef<Path>, Q: AsRef<Path>, F: FnMut(usize, usize)>(
        &mut self,
        local_project_path: P,
        remote_drive_path: Q,
        auto_snapshot: bool,
        prune: bool,
        force: bool,
        mut progress_cb: F,
    ) -> Result<SyncReport> {
        let local_path = local_project_path.as_ref().canonicalize()?;
        let remote_path = remote_drive_path.as_ref().to_path_buf();
        std::fs::create_dir_all(&remote_path)?;

        let auto_snap_id = if auto_snapshot {
            let mut engine = SnapshotEngine::new(self.db, self.cache_mgr);
            match engine.create_snapshot(&local_path, "Auto-snapshot before sync (Pre-sync shield)") {
                Ok(manifest) => {
                    info!("📸 Pre-Sync Auto-Snapshot captured: {}", manifest.id);
                    Some(manifest.id)
                }
                Err(err) => {
                    warn!("Could not capture pre-sync auto-snapshot: {}", err);
                    None
                }
            }
        } else {
            None
        };

        // Write devdrive meta file if not already present
        let project_name = local_path
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_else(|| "project".to_string());
        if !remote_path.join(".devdrive-meta.json").exists() {
            let _ = write_devdrive_meta(&remote_path, &project_name, &local_path.to_string_lossy());
        }

        let filter = DevFilter::for_project(&local_path);
        let mut uploaded = 0;
        let mut conflicts = 0;
        let mut bytes = 0u64;

        let mut source_entries: Vec<_> = walkdir::WalkDir::new(&local_path)
            .into_iter()
            .filter_entry(|e| {
                if e.file_type().is_dir() {
                    let is_root = e.depth() == 1;
                    if e.depth() > 0 && filter.should_skip_dir_entry(e.path(), is_root) {
                        return false;
                    }
                    if let Ok(rel) = e.path().strip_prefix(&local_path)
                        && filter.categorize(rel) == crate::filter::FileCategory::Ephemeral {
                            return false;
                        }
                    true
                } else {
                    true
                }
            })
            .filter_map(|e| e.ok())
            .filter(|e| e.file_type().is_file())
            .filter(|e| {
                if let Ok(rel) = e.path().strip_prefix(&local_path) {
                    filter.categorize(rel) == crate::filter::FileCategory::Source
                } else {
                    false
                }
            })
            .collect();

        // Yoğunluk / Boyut Anomali Kalkanı (Heuristic Guard)
        {
            let mut dir_file_counts: std::collections::HashMap<std::path::PathBuf, usize> = std::collections::HashMap::new();
            let mut dir_byte_counts: std::collections::HashMap<std::path::PathBuf, u64> = std::collections::HashMap::new();
            for entry in &source_entries {
                if let Ok(rel) = entry.path().strip_prefix(&local_path) {
                    let size = entry.metadata().map(|m| m.len()).unwrap_or(0);
                    let mut curr = rel.parent();
                    while let Some(parent) = curr {
                        if parent.as_os_str().is_empty() {
                            break;
                        }
                        *dir_file_counts.entry(parent.to_path_buf()).or_insert(0) += 1;
                        *dir_byte_counts.entry(parent.to_path_buf()).or_insert(0) += size;
                        curr = parent.parent();
                    }
                }
            }
            let mut anomalous_dirs: Vec<std::path::PathBuf> = Vec::new();
            for (dir, &count) in &dir_file_counts {
                let bytes = dir_byte_counts.get(dir).copied().unwrap_or(0);
                if count > 2500 || bytes > 250 * 1024 * 1024 {
                    anomalous_dirs.push(dir.clone());
                }
            }
            if !anomalous_dirs.is_empty() {
                anomalous_dirs.sort_by_key(|p| p.components().count());
                let mut top_anomalies: Vec<std::path::PathBuf> = Vec::new();
                for dir in anomalous_dirs {
                    if !top_anomalies.iter().any(|top| dir.starts_with(top)) {
                        let count = dir_file_counts.get(&dir).copied().unwrap_or(0);
                        let bytes = dir_byte_counts.get(&dir).copied().unwrap_or(0);
                        warn!(
                            "⚠️ Yoğunluk/Boyut Anomali Kalkanı: '{}' alt klasörü tek başına {} dosya ve {:.2} MB içeriyor. Ağır Veri/Ortam Klasörü olarak otomatik atlandı.",
                            dir.display(),
                            count,
                            bytes as f64 / (1024.0 * 1024.0)
                        );
                        top_anomalies.push(dir);
                    }
                }
                source_entries.retain(|entry| {
                    if let Ok(rel) = entry.path().strip_prefix(&local_path) {
                        !top_anomalies.iter().any(|top| rel.starts_with(top))
                    } else {
                        true
                    }
                });
            }
        }


        let total_files = source_entries.len();

        let project_opt = self.db.get_project_by_local_path(&local_path.to_string_lossy())?;

        for (idx, entry) in source_entries.into_iter().enumerate() {
            if let Ok(rel) = entry.path().strip_prefix(&local_path) {
                let dst_file = remote_path.join(rel);
                match Self::safe_sync_file(entry.path(), &dst_file) {
                    Ok(SyncFileResult::Uploaded(b)) => {
                        bytes += b;
                        uploaded += 1;
                        if let Some(ref proj) = project_opt
                            && let Ok(meta) = entry.metadata() {
                                let hash = CacheManager::compute_sha256(entry.path()).unwrap_or_default();
                                let mtime = meta.modified().ok()
                                    .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                                    .map(|d| d.as_secs() as i64)
                                    .unwrap_or(0);
                                let _ = self.db.upsert_file_metadata(
                                    proj.id,
                                    &rel.to_string_lossy(),
                                    meta.len() as i64,
                                    &hash,
                                    "Source",
                                    mtime,
                                    mtime,
                                    true,
                                );
                            }
                    }
                    Ok(SyncFileResult::Conflict(_)) => {
                        conflicts += 1;
                    }
                    Ok(SyncFileResult::Unchanged) => {
                        if let Some(ref proj) = project_opt
                            && let Ok(meta) = entry.metadata() {
                                let hash = CacheManager::compute_sha256(entry.path()).unwrap_or_default();
                                let mtime = meta.modified().ok()
                                    .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                                    .map(|d| d.as_secs() as i64)
                                    .unwrap_or(0);
                                let _ = self.db.upsert_file_metadata(
                                    proj.id,
                                    &rel.to_string_lossy(),
                                    meta.len() as i64,
                                    &hash,
                                    "Source",
                                    mtime,
                                    mtime,
                                    true,
                                );
                            }
                    }
                    Err(e) => warn!("Sync error on {}: {}", rel.display(), e),
                }
            }
            progress_cb(idx + 1, total_files);
        }

        let mut deleted = 0;
        if prune {
            deleted = Self::prune_remote(&local_path, &remote_path, force)?;
        }

        if let Some(ref proj) = project_opt {
            let now = chrono::Utc::now().to_rfc3339();
            let _ = self.db.update_project_last_synced(proj.id, &now);
        }

        info!(
            "Full sync completed: {} uploaded, {} deleted, {} conflicts, {} bytes transferred",
            uploaded, deleted, conflicts, bytes
        );

        Ok(SyncReport {
            files_uploaded: uploaded,
            files_deleted: deleted,
            bytes_transferred: bytes,
            conflicts_detected: conflicts,
            auto_snapshot_id: auto_snap_id,
        })
    }

    /// Prunes remote files that no longer exist locally or are no longer Source files.
    /// Files are moved to .trash vault instead of being permanently destroyed.
    pub fn prune_remote<P: AsRef<Path>, Q: AsRef<Path>>(
        local_project_path: P,
        remote_drive_path: Q,
        force: bool,
    ) -> Result<usize> {
        let local_path = local_project_path.as_ref().canonicalize()?;
        let remote_path = remote_drive_path.as_ref().to_path_buf();
        if !remote_path.exists() {
            return Ok(0);
        }

        let filter = DevFilter::for_project(&local_path);

        // 1. Gather all local source files
        let local_sources: std::collections::HashSet<std::path::PathBuf> = walkdir::WalkDir::new(&local_path)
            .into_iter()
            .filter_entry(|e| {
                if e.file_type().is_dir() {
                    let is_root = e.depth() == 1;
                    if e.depth() > 0 && filter.should_skip_dir_entry(e.path(), is_root) {
                        return false;
                    }
                    if let Ok(rel) = e.path().strip_prefix(&local_path)
                        && filter.categorize(rel) == crate::filter::FileCategory::Ephemeral {
                            return false;
                        }
                    true
                } else {
                    true
                }
            })
            .filter_map(|e| e.ok())
            .filter(|e| e.file_type().is_file())
            .filter_map(|e| {
                let rel = e.path().strip_prefix(&local_path).ok()?;

                if filter.categorize(rel) == crate::filter::FileCategory::Source {
                    Some(rel.to_path_buf())
                } else {
                    None
                }
            })
            .collect();

        // Safety gate 1: Local project must have at least one Source file
        if local_sources.is_empty() {
            bail!("Safety Gate: Local project has no Source files. Prune aborted to prevent disaster.");
        }

        // 2. Gather all remote tracked files (excluding .trash, .conflict_*, .devdrive-meta.json)
        let mut remote_files: Vec<std::path::PathBuf> = Vec::new();
        for entry in walkdir::WalkDir::new(&remote_path).into_iter().filter_map(|e| e.ok()) {
            if !entry.file_type().is_file() {
                continue;
            }
            if entry.path().components().any(|c| c.as_os_str() == ".trash") {
                continue;
            }
            let fname = entry.file_name().to_string_lossy();
            if fname.contains(".conflict_") || fname == ".devdrive-meta.json" {
                continue;
            }
            if let Ok(rel) = entry.path().strip_prefix(&remote_path) {
                remote_files.push(rel.to_path_buf());
            }
        }

        let total_remote_files = remote_files.len();

        // 3. Identify files to prune (files present remotely but absent from local sources)
        let to_prune: Vec<std::path::PathBuf> = remote_files
            .into_iter()
            .filter(|rel| !local_sources.contains(rel))
            .collect();

        if to_prune.is_empty() {
            return Ok(0);
        }

        // Safety gate 2: If pruning > 50% of remote files, require --force
        if total_remote_files > 0 && (to_prune.len() as f64 / total_remote_files as f64) > 0.5 && !force {
            bail!(
                "Safety Gate: Pruning {}/{} files (>50% of remote). Use --force to proceed.",
                to_prune.len(),
                total_remote_files
            );
        }

        // 4. Soft delete candidate files to .trash vault
        let mut pruned_count = 0;
        for rel in &to_prune {
            let dst_file = remote_path.join(rel);
            if Self::safe_soft_delete(&dst_file, &remote_path)? {
                pruned_count += 1;
            }
        }

        // 5. Clean up newly empty directories in remote (excluding root and .trash)
        Self::clean_empty_dirs(&remote_path)?;

        info!("Prune completed: {} files safely moved to remote .trash vault", pruned_count);
        Ok(pruned_count)
    }

    /// Recursively removes empty directories in remote (leaves .trash and non-empty dirs alone)
    pub fn clean_empty_dirs(root: &Path) -> Result<()> {
        let mut dirs: Vec<std::path::PathBuf> = walkdir::WalkDir::new(root)
            .into_iter()
            .filter_map(|e| e.ok())
            .filter(|e| e.file_type().is_dir() && e.path() != root)
            .filter(|e| !e.path().components().any(|c| c.as_os_str() == ".trash"))
            .map(|e| e.path().to_path_buf())
            .collect();

        // Sort by path depth descending so child dirs are checked before parent dirs
        dirs.sort_by_key(|p| std::cmp::Reverse(p.components().count()));

        for dir in dirs {
            if let Ok(mut read) = std::fs::read_dir(&dir)
                && read.next().is_none() {
                    let _ = std::fs::remove_dir(&dir);
                }
        }
        Ok(())
    }

    /// Restores a project from remote Google Drive backup directory to target local directory
    pub fn restore_from_remote<P: AsRef<Path>, Q: AsRef<Path>>(
        &mut self,
        remote_path: P,
        target_path: Q,
        force: bool,
    ) -> Result<RestoreReport> {
        let remote_dir = remote_path.as_ref();
        if !remote_dir.exists() || !remote_dir.is_dir() {
            bail!("Remote directory '{}' does not exist or is not a directory.", remote_dir.display());
        }

        let target_dir = target_path.as_ref();
        if !target_dir.exists() {
            std::fs::create_dir_all(target_dir)
                .with_context(|| format!("Failed to create target directory {}", target_dir.display()))?;
        } else {
            let mut entries = std::fs::read_dir(target_dir)
                .with_context(|| format!("Failed to read target directory {}", target_dir.display()))?;
            let has_entries = entries.next().is_some();
            if has_entries && !force {
                bail!(
                    "Target directory '{}' is not empty. Use --force to restore anyway.",
                    target_dir.display()
                );
            }
        }

        let canonical_target = target_dir.canonicalize()?;
        let canonical_remote = remote_dir.canonicalize()?;
        let filter = DevFilter::for_project(&canonical_remote);

        let mut files_restored = 0;
        let mut files_skipped = 0;
        let mut total_bytes = 0u64;
        let mut restored_relative_paths: Vec<(std::path::PathBuf, u64, String)> = Vec::new();

        for entry in walkdir::WalkDir::new(&canonical_remote).into_iter().filter_map(|e| e.ok()) {
            if !entry.file_type().is_file() {
                continue;
            }

            // Skip .trash/ directory
            if entry.path().components().any(|c| c.as_os_str() == ".trash") {
                continue;
            }

            let fname = entry.file_name().to_string_lossy();
            // Skip *.conflict_* files
            if fname.contains(".conflict_") {
                continue;
            }

            // Skip .devdrive-meta.json
            if fname == ".devdrive-meta.json" {
                continue;
            }

            let rel = match entry.path().strip_prefix(&canonical_remote) {
                Ok(r) => r,
                Err(_) => continue,
            };

            // Skip non-Source files
            if filter.categorize(rel) != crate::filter::FileCategory::Source {
                continue;
            }

            let dst_file = canonical_target.join(rel);
            let src_file = entry.path();
            let src_hash = CacheManager::compute_sha256(src_file)?;

            if dst_file.exists() {
                let dst_hash = CacheManager::compute_sha256(&dst_file)?;
                if src_hash == dst_hash {
                    files_skipped += 1;
                    continue;
                }

                // Hashes differ
                if force {
                    let ts = chrono::Utc::now().format("%Y%m%d_%H%M%S");
                    let file_stem = dst_file.file_stem().and_then(|s| s.to_str()).unwrap_or("file");
                    let ext = dst_file.extension().and_then(|e| e.to_str()).unwrap_or("");
                    let backup_name = if ext.is_empty() {
                        format!("{}.restore_backup_{}", file_stem, ts)
                    } else {
                        format!("{}.restore_backup_{}.{}", file_stem, ts, ext)
                    };
                    let parent_dir = dst_file.parent().unwrap_or(&canonical_target);
                    let mut backup_path = parent_dir.join(&backup_name);
                    let mut counter = 0;
                    while backup_path.exists() {
                        counter += 1;
                        let new_backup_name = if ext.is_empty() {
                            format!("{}.restore_backup_{}_{}", file_stem, ts, counter)
                        } else {
                            format!("{}.restore_backup_{}_{}.{}", file_stem, ts, counter, ext)
                        };
                        backup_path = parent_dir.join(new_backup_name);
                    }
                    std::fs::rename(&dst_file, &backup_path)?;
                    info!("Force Restore Backup: Moved existing {} to {}", dst_file.display(), backup_path.display());
                } else {
                    bail!(
                        "Target file '{}' exists and differs from remote. Use --force to overwrite.",
                        dst_file.display()
                    );
                }
            }

            if let Some(parent) = dst_file.parent() {
                std::fs::create_dir_all(parent)?;
            }

            // Perform copy
            std::fs::copy(src_file, &dst_file)?;

            // Transfer modification time exactly
            let src_meta = src_file.metadata()?;
            let mtime = filetime::FileTime::from_last_modification_time(&src_meta);
            filetime::set_file_mtime(&dst_file, mtime)?;

            // SHA-256 integrity verification
            let verified_dst_hash = CacheManager::compute_sha256(&dst_file)?;
            if src_hash != verified_dst_hash {
                bail!(
                    "Integrity failure: Checksum mismatch on restored file {}. Expected {}, got {}",
                    dst_file.display(),
                    src_hash,
                    verified_dst_hash
                );
            }

            let file_size = src_meta.len();
            files_restored += 1;
            total_bytes += file_size;
            restored_relative_paths.push((rel.to_path_buf(), file_size, verified_dst_hash));
        }

        // Determine project name: from .devdrive-meta.json if present, else folder name
        let project_name = if let Some(meta) = read_devdrive_meta(&canonical_remote) {
            meta.project_name
        } else {
            canonical_remote
                .file_name()
                .map(|n| n.to_string_lossy().to_string())
                .unwrap_or_else(|| {
                    canonical_target
                        .file_name()
                        .map(|n| n.to_string_lossy().to_string())
                        .unwrap_or_else(|| "restored-project".to_string())
                })
        };

        // Register project in Database (without running initial sync_all!)
        let proj = self.db.register_project(
            &project_name,
            &canonical_target.to_string_lossy(),
            &canonical_remote.to_string_lossy(),
        )?;

        // Populate file metadata in DB
        let now = chrono::Utc::now().timestamp();
        for (rel, size, hash) in restored_relative_paths {
            let _ = self.db.upsert_file_metadata(
                proj.id,
                &rel.to_string_lossy(),
                size as i64,
                &hash,
                "source",
                now,
                now,
                true,
            );
        }

        // Generate hints based on project files
        let mut hints = Vec::new();
        if canonical_target.join("package.json").exists() {
            hints.push("node_modules yedeklenmedi: `npm install` veya `pnpm install` çalıştırın".to_string());
        }
        if canonical_target.join("Cargo.toml").exists() {
            hints.push("`cargo build` çalıştırarak derlemeyi başlatabilirsiniz".to_string());
        }
        if canonical_target.join("pyproject.toml").exists() || canonical_target.join("requirements.txt").exists() {
            hints.push("Python sanal ortamı kurun ve bağımlılıkları yükleyin (`python -m venv .venv && pip install -r requirements.txt`)".to_string());
        }
        if canonical_target.join("go.mod").exists() {
            hints.push("Go bağımlılıklarını indirin (`go mod download` / `go mod vendor`)".to_string());
        }
        if !canonical_target.join(".git").exists() {
            hints.push("Git geçmişi yedeklenmedi. Repo'yu ayrıca klonlayın veya `git init` yapın".to_string());
        }
        if canonical_target.join(".env.example").exists() && !canonical_target.join(".env").exists() {
            hints.push(".env güvenlik nedeniyle yedeklenmez, ortam değişkenlerinizi .env.example'a bakarak elle oluşturun".to_string());
        }

        Ok(RestoreReport {
            files_restored,
            files_skipped,
            bytes: total_bytes,
            hints,
        })
    }

    /// Predictive Cache Warming: Pre-loads critical entry points into local hot cache
    pub fn warm_cache<P: AsRef<Path>>(&self, project_path: P) -> Result<usize> {
        let project_path = project_path.as_ref().canonicalize()?;
        let critical_patterns = [
            "package.json",
            "Cargo.toml",
            "pyproject.toml",
            "requirements.txt",
            "tsconfig.json",
            "README.md",
            "src/main.rs",
            "src/index.ts",
            "src/index.js",
            "src/App.tsx",
            "app/page.tsx",
            "main.py",
        ];

        let mut warmed = 0;
        for pattern in &critical_patterns {
            let candidate = project_path.join(pattern);
            if candidate.exists() && candidate.is_file()
                && let Ok(content) = std::fs::read(&candidate) {
                    let _ = self.cache_mgr.store_content(&content);
                    warmed += 1;
                }
        }

        info!("Predictive warm cache activated: {} hot files primed", warmed);
        Ok(warmed)
    }

    /// Bundled synchronization method on SyncEngine instance
    pub fn sync_bundle<P: AsRef<Path>, Q: AsRef<Path>>(
        &mut self,
        local_project_path: P,
        remote_project_dir: Q,
        auto_snapshot: bool,
    ) -> Result<BundleSyncReport> {
        let include_git = crate::config::DevDriveConfig::load()
            .map(|c| c.include_git)
            .unwrap_or(true);
        self.sync_bundle_with_options(local_project_path, remote_project_dir, auto_snapshot, include_git)
    }

    /// Bundled synchronization method on SyncEngine instance with progress reporting
    /// If `progress_cb` returns `false`, packaging is aborted immediately and temp files are cleaned up.
    pub fn sync_bundle_with_progress<P: AsRef<Path>, Q: AsRef<Path>, F>(
        &mut self,
        local_project_path: P,
        remote_project_dir: Q,
        auto_snapshot: bool,
        progress_cb: F,
    ) -> Result<BundleSyncReport>
    where
        F: FnMut(&str, usize, usize) -> bool + Send,
    {
        let include_git = crate::config::DevDriveConfig::load()
            .map(|c| c.include_git)
            .unwrap_or(true);
        self.sync_bundle_with_options_and_progress(local_project_path, remote_project_dir, auto_snapshot, include_git, progress_cb)
    }

    /// Bundled synchronization method on SyncEngine instance with include_git option
    pub fn sync_bundle_with_options<P: AsRef<Path>, Q: AsRef<Path>>(
        &mut self,
        local_project_path: P,
        remote_project_dir: Q,
        auto_snapshot: bool,
        include_git: bool,
    ) -> Result<BundleSyncReport> {
        self.sync_bundle_with_options_and_progress(local_project_path, remote_project_dir, auto_snapshot, include_git, |_, _, _| true)
    }

    /// Bundled synchronization method on SyncEngine instance with include_git and progress callback
    pub fn sync_bundle_with_options_and_progress<P: AsRef<Path>, Q: AsRef<Path>, F>(
        &mut self,
        local_project_path: P,
        remote_project_dir: Q,
        auto_snapshot: bool,
        include_git: bool,
        progress_cb: F,
    ) -> Result<BundleSyncReport>
    where
        F: FnMut(&str, usize, usize) -> bool + Send,
    {
        let report = sync_project_bundle_with_options_and_progress(
            local_project_path.as_ref(),
            remote_project_dir.as_ref(),
            auto_snapshot,
            include_git,
            progress_cb,
        )?;

        // Update project WAL and last_synced_at in database
        if let Ok(canonical) = local_project_path.as_ref().canonicalize()
            && let Ok(Some(proj)) = self.db.get_project_by_local_path(&canonical.to_string_lossy()) {
                let pending = self.db.get_pending_wal_events(proj.id).unwrap_or_default();
                if !pending.is_empty() {
                    let ids: Vec<i64> = pending.iter().map(|e| e.id).collect();
                    let _ = self.db.mark_wal_events_synced(&ids);
                }
                let _ = self.db.prune_synced_wal_events();
                let now = chrono::Utc::now().to_rfc3339();
                let _ = self.db.update_project_last_synced(proj.id, &now);
            }

        Ok(report)
    }
}

/// Atomically synchronizes a project using bundle packaging (tar.gz + manifest.json)
pub fn sync_project_bundle<P: AsRef<Path>, Q: AsRef<Path>>(
    local_project_path: P,
    remote_project_dir: Q,
    auto_snapshot: bool,
) -> Result<BundleSyncReport> {
    let include_git = crate::config::DevDriveConfig::load()
        .map(|c| c.include_git)
        .unwrap_or(true);
    sync_project_bundle_with_options(local_project_path, remote_project_dir, auto_snapshot, include_git)
}

/// Atomically synchronizes a project using bundle packaging (tar.gz + manifest.json) with options
///
/// Features:
/// 1. Scans clean source code using DevFilter::for_project (ephemerals like node_modules, .onnx, target/ are excluded)
/// 2. If include_git is true, safely archives .git directory (excluding lock files)
/// 3. Atomically packages files into bundle.tar.gz via flate2 and tar
/// 4. Writes lightweight manifest.json and .devdrive-meta.json
/// 5. Leaves only index/bundle files in remote directory instead of thousands of individual files
pub fn sync_project_bundle_with_options<P: AsRef<Path>, Q: AsRef<Path>>(
    local_project_path: P,
    remote_project_dir: Q,
    auto_snapshot: bool,
    include_git: bool,
) -> Result<BundleSyncReport> {
    sync_project_bundle_with_options_and_progress(
        local_project_path,
        remote_project_dir,
        auto_snapshot,
        include_git,
        |_, _, _| true,
    )
}

/// Applies density and size anomaly guard:
/// If any subdirectory in a project contains > 2500 files or > 250 MB alone,
/// prunes that subtree and logs a warning.
pub fn apply_density_and_size_guard(source_entries: &mut Vec<(std::path::PathBuf, std::path::PathBuf)>) {
    let mut dir_file_counts: std::collections::HashMap<std::path::PathBuf, usize> = std::collections::HashMap::new();
    let mut dir_byte_counts: std::collections::HashMap<std::path::PathBuf, u64> = std::collections::HashMap::new();

    for (abs_path, rel_path) in source_entries.iter() {
        let size = abs_path.metadata().map(|m| m.len()).unwrap_or(0);
        let mut curr = rel_path.parent();
        while let Some(parent) = curr {
            if parent.as_os_str().is_empty() {
                break;
            }
            *dir_file_counts.entry(parent.to_path_buf()).or_insert(0) += 1;
            *dir_byte_counts.entry(parent.to_path_buf()).or_insert(0) += size;
            curr = parent.parent();
        }
    }

    let mut anomalous_dirs: Vec<std::path::PathBuf> = Vec::new();
    for (dir, &count) in &dir_file_counts {
        let bytes = dir_byte_counts.get(dir).copied().unwrap_or(0);
        if count > 2500 || bytes > 250 * 1024 * 1024 {
            anomalous_dirs.push(dir.clone());
        }
    }

    if !anomalous_dirs.is_empty() {
        anomalous_dirs.sort_by_key(|p| p.components().count());
        let mut top_anomalies: Vec<std::path::PathBuf> = Vec::new();
        for dir in anomalous_dirs {
            if !top_anomalies.iter().any(|top| dir.starts_with(top)) {
                let count = dir_file_counts.get(&dir).copied().unwrap_or(0);
                let bytes = dir_byte_counts.get(&dir).copied().unwrap_or(0);
                warn!(
                    "⚠️ Yoğunluk/Boyut Anomali Kalkanı: '{}' alt klasörü tek başına {} dosya ve {:.2} MB içeriyor. Ağır Veri/Ortam Klasörü olarak otomatik atlandı.",
                    dir.display(),
                    count,
                    bytes as f64 / (1024.0 * 1024.0)
                );
                top_anomalies.push(dir);
            }
        }

        source_entries.retain(|(_, rel_path)| {
            !top_anomalies.iter().any(|top| rel_path.starts_with(top))
        });
    }
}

/// Atomically synchronizes a project using bundle packaging (tar.gz + manifest.json) with options and live progress reporting
pub fn sync_project_bundle_with_options_and_progress<P: AsRef<Path>, Q: AsRef<Path>, F>(
    local_project_path: P,
    remote_project_dir: Q,
    auto_snapshot: bool,
    include_git: bool,
    mut progress_cb: F,
) -> Result<BundleSyncReport>
where
    F: FnMut(&str, usize, usize) -> bool + Send,
{
    let local_path = local_project_path
        .as_ref()
        .canonicalize()
        .with_context(|| format!("Yerel proje yolu bulunamadı: {}", local_project_path.as_ref().display()))?;

    if !local_path.is_dir() {
        bail!("Yerel proje yolu bir dizin değil: {}", local_path.display());
    }

    let remote_dir = remote_project_dir.as_ref();
    std::fs::create_dir_all(remote_dir)
        .with_context(|| format!("Uzak proje dizini oluşturulamadı: {}", remote_dir.display()))?;

    let project_name = local_path
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_else(|| "project".to_string());
    validate_project_name(&project_name)?;

    // 1. Auto-Snapshot Shield (if enabled and DB available)
    let auto_snap_id = if auto_snapshot {
        let db_path = Database::default_path();
        let cache_dir = CacheManager::default_cache_dir();
        if let (Ok(mut db), Ok(cache_mgr)) = (
            Database::open(&db_path),
            CacheManager::new(&cache_dir, 20 * 1024 * 1024 * 1024),
        ) {
            let mut engine = SnapshotEngine::new(&mut db, &cache_mgr);
            match engine.create_snapshot(&local_path, "Auto-snapshot before bundle sync (Pre-sync shield)") {
                Ok(manifest) => {
                    info!("📸 Pre-Sync Auto-Snapshot captured: {}", manifest.id);
                    Some(manifest.id)
                }
                Err(err) => {
                    warn!("Could not capture pre-sync auto-snapshot: {}", err);
                    None
                }
            }
        } else {
            None
        }
    } else {
        None
    };

    // 2. Scan clean source files using DevFilter with signature detection
    let filter = DevFilter::for_project(&local_path);
    let mut source_entries: Vec<(std::path::PathBuf, std::path::PathBuf)> = walkdir::WalkDir::new(&local_path)
        .into_iter()
        .filter_entry(|e| {
            if e.file_type().is_dir() {
                let is_root = e.depth() == 1;
                if e.depth() > 0 && filter.should_skip_dir_entry(e.path(), is_root) {
                    return false;
                }
                if let Ok(rel) = e.path().strip_prefix(&local_path)
                    && filter.categorize(rel) == crate::filter::FileCategory::Ephemeral {
                        return false;
                    }
                true
            } else {
                true
            }
        })
        .filter_map(|e| e.ok())
        .filter(|e| e.file_type().is_file())
        .filter_map(|e| {
            if let Ok(rel) = e.path().strip_prefix(&local_path) {
                if filter.categorize(rel) == crate::filter::FileCategory::Source {
                    Some((e.path().to_path_buf(), rel.to_path_buf()))
                } else {
                    None
                }
            } else {
                None
            }
        })
        .collect();

    // 2b. Include git history (.git) safely if enabled and present in a valid git repository
    if include_git {
        let git_dir = local_path.join(".git");
        let is_valid_git_dir = git_dir.is_dir() && git_dir.join("HEAD").is_file();
        let is_valid_git_file = git_dir.is_file();
        if is_valid_git_dir {
            for entry in walkdir::WalkDir::new(&git_dir).into_iter().filter_map(|e| e.ok()) {
                if entry.file_type().is_file() {
                    let file_name = entry.file_name().to_string_lossy();
                    // Exclude transient git lock files (e.g. index.lock, HEAD.lock)
                    if file_name.ends_with(".lock") {
                        continue;
                    }
                    if let Ok(rel) = entry.path().strip_prefix(&local_path) {
                        source_entries.push((entry.path().to_path_buf(), rel.to_path_buf()));
                    }
                }
            }
        } else if is_valid_git_file {
            // Git worktree pointer or submodule file
            let file_name = git_dir.file_name().map(|n| n.to_string_lossy()).unwrap_or_default();
            if !file_name.ends_with(".lock")
                && let Ok(rel) = git_dir.strip_prefix(&local_path) {
                    source_entries.push((git_dir.clone(), rel.to_path_buf()));
                }
        }
    }

    // Yoğunluk / Boyut Anomali Kalkanı (Heuristic Guard):
    apply_density_and_size_guard(&mut source_entries);

    // Sort entries deterministically and deduplicate for reproducible archives
    source_entries.sort_by(|a, b| a.1.cmp(&b.1));
    source_entries.dedup_by(|a, b| a.1 == b.1);

    // Unique temporary files for atomic write
    let tmp_suffix = format!("{}_{}", std::process::id(), chrono::Utc::now().timestamp_nanos_opt().unwrap_or(0));
    let temp_bundle_path = remote_dir.join(format!(".bundle.tar.gz.tmp.{}", tmp_suffix));
    let final_bundle_path = remote_dir.join("bundle.tar.gz");

    let temp_manifest_path = remote_dir.join(format!(".manifest.json.tmp.{}", tmp_suffix));
    let final_manifest_path = remote_dir.join("manifest.json");

    let mut manifest_entries = Vec::with_capacity(source_entries.len());
    let mut original_bytes = 0u64;
    let total_files = source_entries.len();

    // 3. Build tar.gz atomically with flate2
    let packaging_result = (|| -> Result<()> {
        let tar_file = std::fs::File::create(&temp_bundle_path)
            .with_context(|| format!("Failed to create temporary bundle archive at {}", temp_bundle_path.display()))?;
        let enc = flate2::write::GzEncoder::new(tar_file, flate2::Compression::default());
        let mut tar_builder = tar::Builder::new(enc);

        for (idx, (abs_path, rel_path)) in source_entries.iter().enumerate() {
            let meta = abs_path
                .metadata()
                .with_context(|| format!("Failed to read metadata for {}", abs_path.display()))?;
            let size = meta.len();
            original_bytes += size;

            let mtime = meta
                .modified()
                .ok()
                .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                .map(|d| d.as_secs() as i64)
                .unwrap_or(0);

            let rel_str = rel_path.to_string_lossy().to_string();
            let sha256 = CacheManager::compute_sha256(abs_path).ok();

            // Progress callback for live telemetry; returns false if cancelled by user
            if !progress_cb(&rel_str, idx + 1, total_files) {
                bail!("İşlem kullanıcı tarafından durduruldu");
            }

            manifest_entries.push(BundleFileEntry {
                path: rel_str,
                size_bytes: size,
                mtime,
                sha256,
            });

            let mut file = std::fs::File::open(abs_path)
                .with_context(|| format!("Failed to open source file {}", abs_path.display()))?;

            let mut header = tar::Header::new_gnu();
            header.set_entry_type(tar::EntryType::Regular);
            header.set_size(size);
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                header.set_mode(meta.permissions().mode());
            }
            #[cfg(not(unix))]
            {
                header.set_mode(0o644);
            }
            header.set_mtime(mtime as u64);
            header.set_cksum();
            tar_builder
                .append_data(&mut header, rel_path, &mut file)
                .with_context(|| format!("Failed to append {} to tar archive", rel_path.display()))?;
        }

        let encoder = tar_builder.into_inner()?;
        let mut finished_file = encoder.finish()?;
        use std::io::Write;
        finished_file.flush()?;
        Ok(())
    })();

    if let Err(e) = packaging_result {
        let _ = std::fs::remove_file(&temp_bundle_path);
        return Err(e);
    }

    // Atomic rename of bundle archive
    if let Err(e) = std::fs::rename(&temp_bundle_path, &final_bundle_path) {
        let _ = std::fs::remove_file(&temp_bundle_path);
        bail!("Failed to atomically rename bundle archive to {}: {}", final_bundle_path.display(), e);
    }

    // 4. Build and atomically write manifest.json
    let manifest = BundleManifest {
        version: "1.0.0".to_string(),
        project_name: project_name.clone(),
        created_at: chrono::Utc::now().to_rfc3339(),
        total_files: manifest_entries.len(),
        total_bytes: original_bytes,
        entries: manifest_entries,
    };

    let manifest_json = serde_json::to_string_pretty(&manifest)?;
    if let Err(e) = std::fs::write(&temp_manifest_path, manifest_json) {
        let _ = std::fs::remove_file(&temp_manifest_path);
        bail!("Failed to write temporary manifest: {}", e);
    }
    if let Err(e) = std::fs::rename(&temp_manifest_path, &final_manifest_path) {
        let _ = std::fs::remove_file(&temp_manifest_path);
        bail!("Failed to atomically rename manifest to {}: {}", final_manifest_path.display(), e);
    }

    // 5. Write metadata file .devdrive-meta.json
    write_devdrive_meta(remote_dir, &project_name, &local_path.to_string_lossy())?;

    let bundle_bytes = final_bundle_path.metadata().map(|m| m.len()).unwrap_or(0);

    info!(
        "Bundle sync completed for '{}': {} files packaged, {} -> {} ({:.1}% compression)",
        project_name,
        source_entries.len(),
        original_bytes,
        bundle_bytes,
        if original_bytes > 0 {
            (1.0 - (bundle_bytes as f64 / original_bytes as f64)) * 100.0
        } else {
            0.0
        }
    );

    Ok(BundleSyncReport {
        files_packaged: source_entries.len(),
        original_bytes,
        bundle_bytes,
        bundle_path: final_bundle_path.to_string_lossy().to_string(),
        manifest_path: final_manifest_path.to_string_lossy().to_string(),
        auto_snapshot_id: auto_snap_id,
    })
}

/// Restores a project from a remote bundle archive (bundle.tar.gz) into destination directory
/// with strict path traversal security checks and SHA-256 integrity verification
pub fn restore_project_bundle<P: AsRef<Path>, Q: AsRef<Path>>(
    remote_project_dir: P,
    local_destination_dir: Q,
) -> Result<RestoreReport> {
    let remote_dir = remote_project_dir.as_ref();
    if !remote_dir.exists() || !remote_dir.is_dir() {
        bail!("Uzak proje dizini bulunamadı: {}", remote_dir.display());
    }

    let bundle_path = if remote_dir.join("bundle.tar.gz").exists() {
        remote_dir.join("bundle.tar.gz")
    } else if remote_dir.join("tree.tar.gz").exists() {
        remote_dir.join("tree.tar.gz")
    } else {
        bail!(
            "Uzak dizinde 'bundle.tar.gz' veya 'tree.tar.gz' arşivi bulunamadı: {}",
            remote_dir.display()
        );
    };

    let target_dir = local_destination_dir.as_ref();
    if !target_dir.exists() {
        std::fs::create_dir_all(target_dir)
            .with_context(|| format!("Hedef yerel dizin oluşturulamadı: {}", target_dir.display()))?;
    }
    let canonical_target = target_dir
        .canonicalize()
        .with_context(|| format!("Hedef yerel dizin çözümlenemedi: {}", target_dir.display()))?;

    let file = std::fs::File::open(&bundle_path)
        .with_context(|| format!("Arşiv açılamadı: {}", bundle_path.display()))?;
    let gz = flate2::read::GzDecoder::new(file);
    let mut archive = tar::Archive::new(gz);

    let mut files_restored = 0;
    let mut total_bytes = 0u64;

    for entry_res in archive.entries()? {
        let mut entry = entry_res?;
        let entry_path = entry.path()?.to_path_buf();

        // 1. Strict Path Traversal Check
        for component in entry_path.components() {
            match component {
                std::path::Component::Normal(_) | std::path::Component::CurDir => {}
                _ => {
                    bail!(
                        "Güvenlik Uyarısı: Arşiv içinde zararlı yol geçişi (Path Traversal) tespit edildi: '{}'",
                        entry_path.display()
                    );
                }
            }
        }
        if entry_path.is_absolute() {
            bail!(
                "Güvenlik Uyarısı: Arşiv içinde mutlak yol (Absolute Path) tespit edildi: '{}'",
                entry_path.display()
            );
        }

        let dst_file = canonical_target.join(&entry_path);

        // Ensure dst_file starts with canonical_target
        if !dst_file.starts_with(&canonical_target) {
            bail!(
                "Güvenlik Uyarısı: Hedef dizin dışına taşma tespit edildi: '{}'",
                dst_file.display()
            );
        }

        // Symlink security check
        let entry_type = entry.header().entry_type();
        if (entry_type.is_symlink() || entry_type.is_hard_link())
            && let Ok(Some(link_target)) = entry.link_name()
                && (link_target.is_absolute()
                    || link_target.components().any(|c| matches!(c, std::path::Component::ParentDir)))
                {
                    bail!(
                        "Güvenlik Uyarısı: Güvenli olmayan sembolik bağ bağlantısı tespit edildi: '{}'",
                        link_target.display()
                    );
                }

        if entry_type.is_dir() {
            std::fs::create_dir_all(&dst_file)?;
        } else if entry_type.is_file() {
            if let Some(parent) = dst_file.parent() {
                std::fs::create_dir_all(parent)?;
            }
            let size = entry.header().size()?;
            entry
                .unpack(&dst_file)
                .with_context(|| format!("Dosya açılamadı: {}", dst_file.display()))?;
            files_restored += 1;
            total_bytes += size;
        }
    }

    // 2. Cryptographic Checksum Integrity Check from manifest if available
    if let Some(manifest) = read_bundle_manifest(remote_dir) {
        for item in &manifest.entries {
            if let Some(ref expected_hash) = item.sha256 {
                let restored_file = canonical_target.join(&item.path);
                if restored_file.exists() && restored_file.is_file() {
                    let actual_hash = CacheManager::compute_sha256(&restored_file)?;
                    if &actual_hash != expected_hash {
                        bail!(
                            "Bütünlük Doğrulama Hatası: {} dosyasının sağlama toplamı uyuşmuyor! Beklenen: {}, Gerçekleşen: {}",
                            restored_file.display(),
                            expected_hash,
                            actual_hash
                        );
                    }
                }
            }
        }
    }

    // 3. Register project in DB if database is available
    if let Ok(mut db) = Database::open(Database::default_path()) {
        let project_name = if let Some(meta) = read_devdrive_meta(remote_dir) {
            meta.project_name
        } else {
            remote_dir
                .file_name()
                .map(|n| n.to_string_lossy().to_string())
                .unwrap_or_else(|| "restored-project".to_string())
        };
        let _ = db.register_project(
            &project_name,
            &canonical_target.to_string_lossy(),
            &remote_dir.to_string_lossy(),
        );
    }

    // 4. Produce helpful hints
    let mut hints = Vec::new();
    if canonical_target.join(".git").exists() {
        hints.push("Git geçmişi eksiksiz geri yüklendi (`git log` ile kontrol edebilirsiniz)".to_string());
    }
    if canonical_target.join("package.json").exists() {
        hints.push("node_modules yedeklenmedi: `npm install` veya `pnpm install` çalıştırın".to_string());
    }
    if canonical_target.join("Cargo.toml").exists() {
        hints.push("`cargo build` çalıştırarak derlemeyi başlatabilirsiniz".to_string());
    }
    if canonical_target.join("pyproject.toml").exists() || canonical_target.join("requirements.txt").exists() {
        hints.push(
            "Python sanal ortamı kurun ve bağımlılıkları yükleyin (`python -m venv .venv && pip install -r requirements.txt`)"
                .to_string(),
        );
    }
    if canonical_target.join("go.mod").exists() {
        hints.push("Go bağımlılıklarını indirin (`go mod download` / `go mod vendor`)".to_string());
    }
    if !canonical_target.join(".git").exists() {
        hints.push("Git geçmişi yedeklenmedi. Repo'yu ayrıca klonlayın veya `git init` yapın".to_string());
    }
    if canonical_target.join(".env.example").exists() && !canonical_target.join(".env").exists() {
        hints.push(
            ".env güvenlik nedeniyle yedeklenmez, ortam değişkenlerinizi .env.example'a bakarak elle oluşturun"
                .to_string(),
        );
    }

    Ok(RestoreReport {
        files_restored,
        files_skipped: 0,
        bytes: total_bytes,
        hints,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn test_sync_engine() -> Result<()> {
        let temp = tempdir()?;
        let local_dir = temp.path().join("local");
        let remote_dir = temp.path().join("remote_drive");
        let cache_dir = temp.path().join("cache");
        std::fs::create_dir_all(&local_dir)?;
        std::fs::create_dir_all(&remote_dir)?;

        let mut db = Database::open(":memory:")?;
        let proj = db.register_project(
            "test-app",
            &local_dir.canonicalize()?.to_string_lossy(),
            &remote_dir.to_string_lossy(),
        )?;

        let cache_mgr = CacheManager::new(&cache_dir, 1024 * 1024 * 1024)?;

        // Create local file
        let src_code = local_dir.join("main.rs");
        std::fs::write(&src_code, "fn main() { println!(\"Hi\"); }")?;

        // Record in WAL
        {
            let mut wal = WalManager::new(&mut db);
            wal.record_change(proj.id, ChangeType::Created, Path::new("main.rs"))?;
        }

        // Run sync
        {
            let mut sync = SyncEngine::new(&mut db, &cache_mgr);
            let report = sync.sync_pending(&local_dir, &remote_dir, false)?;
            assert_eq!(report.files_uploaded, 1);
            assert!(remote_dir.join("main.rs").exists());
        }

        // Test Conflict Shield:
        // Remote file modified with newer timestamp and different content
        let remote_file = remote_dir.join("main.rs");
        std::thread::sleep(std::time::Duration::from_millis(50));
        std::fs::write(&remote_file, "fn main() { println!(\"Remote edit\"); }")?;

        // Local file also edited with older simulated timestamp
        let local_file = local_dir.join("main.rs");
        std::fs::write(&local_file, "fn main() { println!(\"Local edit\"); }")?;

        // Reset local file modified time to be older than remote
        let older_time = filetime::FileTime::from_unix_time(1000000, 0);
        filetime::set_file_mtime(&local_file, older_time)?;

        {
            let mut wal = WalManager::new(&mut db);
            wal.record_change(proj.id, ChangeType::Modified, Path::new("main.rs"))?;
        }

        {
            let mut sync = SyncEngine::new(&mut db, &cache_mgr);
            let conflict_report = sync.sync_pending(&local_dir, &remote_dir, false)?;
            assert_eq!(conflict_report.conflicts_detected, 1);
        }

        // Test Soft-Delete Vault:
        {
            let mut wal = WalManager::new(&mut db);
            wal.record_change(proj.id, ChangeType::Deleted, Path::new("main.rs"))?;
        }

        {
            let mut sync = SyncEngine::new(&mut db, &cache_mgr);
            let delete_report = sync.sync_pending(&local_dir, &remote_dir, false)?;
            assert_eq!(delete_report.files_deleted, 1);
            assert!(remote_dir.join(".trash").exists());
        }

        Ok(())
    }

    #[test]
    fn test_clean_empty_dirs() -> Result<()> {
        let temp = tempdir()?;
        let root = temp.path().join("root");
        let dir_a = root.join("a/b/c");
        let trash_empty = root.join(".trash/empty_sub");
        let non_empty = root.join("keep/has_file");

        std::fs::create_dir_all(&dir_a)?;
        std::fs::create_dir_all(&trash_empty)?;
        std::fs::create_dir_all(&non_empty)?;
        std::fs::write(non_empty.join("sample.txt"), b"keep")?;

        SyncEngine::clean_empty_dirs(&root)?;

        // Empty dirs a/b/c must be pruned
        assert!(!dir_a.exists());
        assert!(!root.join("a").exists());

        // .trash and non_empty must remain intact
        assert!(trash_empty.exists(), ".trash contents must NEVER be pruned by clean_empty_dirs");
        assert!(non_empty.exists());
        assert!(root.exists(), "Root dir must not be removed");

        Ok(())
    }

    #[test]
    fn test_devdrive_meta_and_list_remote() -> Result<()> {
        let temp = tempdir()?;
        let remote_base = temp.path().join("DriveBase");
        let project_dir = remote_base.join("my-cool-app");
        std::fs::create_dir_all(project_dir.join("src"))?;
        std::fs::write(project_dir.join("src/lib.rs"), b"pub fn hi() {}")?;

        // Write meta
        write_devdrive_meta(&project_dir, "my-cool-app", "/Users/dev/my-cool-app")?;

        // Read meta
        let meta = read_devdrive_meta(&project_dir);
        assert!(meta.is_some());
        let m = meta.unwrap();
        assert_eq!(m.project_name, "my-cool-app");
        assert_eq!(m.original_local_path, "/Users/dev/my-cool-app");

        // List remote projects
        let list = list_remote_projects(&remote_base)?;
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].name, "my-cool-app");
        assert_eq!(list[0].file_count, 1);
        assert!(list[0].total_bytes > 0);
        assert!(list[0].last_modified.is_some());

        Ok(())
    }

    #[test]
    fn test_bundle_sync_offline_pack_and_restore() -> Result<()> {
        let temp = tempdir()?;
        let local_dir = temp.path().join("my-project");
        let remote_dir = temp.path().join("DriveBase").join("my-project");
        let restore_dir = temp.path().join("restored-project");

        // 1. Create mock clean source code
        std::fs::create_dir_all(local_dir.join("src"))?;
        std::fs::create_dir_all(local_dir.join("assets"))?;
        std::fs::write(local_dir.join("src/main.rs"), b"fn main() { println!(\"DevDrive Bundle\"); }")?;
        std::fs::write(local_dir.join("package.json"), b"{\"name\": \"devdrive-bundle\", \"version\": \"1.0.0\"}")?;
        std::fs::write(local_dir.join("assets/logo.png"), b"PNG_FAKE_IMAGE_DATA_1234567890")?;

        // 2. Create mock junk / ephemeral files that must be excluded
        std::fs::create_dir_all(local_dir.join("node_modules/lodash"))?;
        std::fs::write(local_dir.join("node_modules/lodash/index.js"), b"module.exports = {};")?;
        std::fs::write(local_dir.join("model.onnx"), b"LARGE_AI_MODEL_ONNX_BLOB")?;
        std::fs::create_dir_all(local_dir.join("target/debug"))?;
        std::fs::write(local_dir.join("target/debug/app"), b"COMPILED_BINARY_JUNK")?;

        // 3. Execute bundle sync
        let report = sync_project_bundle(&local_dir, &remote_dir, false)?;
        assert_eq!(report.files_packaged, 3, "Only 3 clean source files should be packaged");
        assert!(report.bundle_bytes > 0);
        assert!(report.original_bytes > 0);

        // 4. Verify remote directory contains ONLY bundle.tar.gz and manifest.json (and hidden .devdrive-meta.json)
        assert!(remote_dir.join("bundle.tar.gz").exists(), "bundle.tar.gz must exist");
        assert!(remote_dir.join("manifest.json").exists(), "manifest.json must exist");
        assert!(remote_dir.join(".devdrive-meta.json").exists(), ".devdrive-meta.json must exist");

        // Ensure no loose source or junk directories were created in remote
        assert!(!remote_dir.join("src").exists(), "Loose src/ directory must NOT exist in remote");
        assert!(!remote_dir.join("node_modules").exists(), "node_modules must NOT exist in remote");
        assert!(!remote_dir.join("model.onnx").exists(), "model.onnx must NOT exist in remote");

        // Check non-hidden files in remote_dir are strictly bundle.tar.gz and manifest.json
        let non_hidden_files: Vec<String> = std::fs::read_dir(&remote_dir)?
            .filter_map(|e| e.ok())
            .map(|e| e.file_name().to_string_lossy().to_string())
            .filter(|name| !name.starts_with('.'))
            .collect();
        assert_eq!(non_hidden_files.len(), 2, "Remote directory should strictly contain 2 non-hidden files");
        assert!(non_hidden_files.contains(&"bundle.tar.gz".to_string()));
        assert!(non_hidden_files.contains(&"manifest.json".to_string()));

        // 5. Verify manifest.json contents
        let manifest = read_bundle_manifest(&remote_dir).expect("Bundle manifest must be parseable");
        assert_eq!(manifest.total_files, 3);
        assert_eq!(manifest.entries.len(), 3);

        let manifest_paths: Vec<&str> = manifest.entries.iter().map(|e| e.path.as_str()).collect();
        assert!(manifest_paths.contains(&"src/main.rs"));
        assert!(manifest_paths.contains(&"package.json"));
        assert!(manifest_paths.contains(&"assets/logo.png"));
        assert!(!manifest_paths.iter().any(|p| p.contains("node_modules")));
        assert!(!manifest_paths.iter().any(|p| p.ends_with(".onnx")));
        assert!(!manifest_paths.iter().any(|p| p.starts_with("target")));

        // 6. Restore project bundle to another temporary destination directory
        let restore_report = restore_project_bundle(&remote_dir, &restore_dir)?;
        assert_eq!(restore_report.files_restored, 3);

        // Verify all source files restored accurately
        let restored_main = std::fs::read_to_string(restore_dir.join("src/main.rs"))?;
        assert_eq!(restored_main, "fn main() { println!(\"DevDrive Bundle\"); }");

        let restored_pkg = std::fs::read_to_string(restore_dir.join("package.json"))?;
        assert_eq!(restored_pkg, "{\"name\": \"devdrive-bundle\", \"version\": \"1.0.0\"}");

        let restored_logo = std::fs::read(restore_dir.join("assets/logo.png"))?;
        assert_eq!(restored_logo, b"PNG_FAKE_IMAGE_DATA_1234567890");

        // Verify ephemeral / junk files were NEVER restored
        assert!(!restore_dir.join("node_modules").exists());
        assert!(!restore_dir.join("model.onnx").exists());
        assert!(!restore_dir.join("target").exists());

        // Verify hints
        assert!(restore_report.hints.iter().any(|h| h.contains("npm install")));

        // Verify list_remote_projects reads bundle manifest seamlessly
        let drive_base = temp.path().join("DriveBase");
        let remote_summary = list_remote_projects(&drive_base)?;
        assert_eq!(remote_summary.len(), 1);
        assert_eq!(remote_summary[0].name, "my-project");
        assert_eq!(remote_summary[0].file_count, 3);
        assert!(remote_summary[0].total_bytes > 0);

        Ok(())
    }

    #[test]
    fn test_bundle_restore_path_traversal_protection() -> Result<()> {
        use std::io::Write;
        let temp = tempdir()?;
        let remote_dir = temp.path().join("malicious_remote");
        let restore_dir = temp.path().join("safe_restore");
        std::fs::create_dir_all(&remote_dir)?;
        std::fs::create_dir_all(&restore_dir)?;

        // Craft a raw malicious tar.gz containing path traversal entry "../evil.txt"
        let malicious_tar_path = remote_dir.join("bundle.tar.gz");
        {
            let file = std::fs::File::create(&malicious_tar_path)?;
            let mut gz = flate2::write::GzEncoder::new(file, flate2::Compression::default());

            let mut header = [0u8; 512];
            let name_bytes = b"../evil.txt";
            header[..name_bytes.len()].copy_from_slice(name_bytes);
            header[100..108].copy_from_slice(b"0000644\0");
            header[108..116].copy_from_slice(b"0000000\0");
            header[116..124].copy_from_slice(b"0000000\0");

            let payload = b"ATTACK_PAYLOAD";
            let size_str = format!("{:011o}\0", payload.len());
            header[124..136].copy_from_slice(size_str.as_bytes());
            header[136..148].copy_from_slice(b"00000000000\0");
            header[156] = b'0';
            header[257..263].copy_from_slice(b"ustar\0");
            header[263..265].copy_from_slice(b"00");

            header[148..156].fill(b' ');
            let sum: u32 = header.iter().map(|&b| b as u32).sum();
            let chksum_str = format!("{:06o}\0 ", sum);
            header[148..156].copy_from_slice(chksum_str.as_bytes());

            gz.write_all(&header)?;
            gz.write_all(payload)?;
            let padding = (512 - (payload.len() % 512)) % 512;
            if padding > 0 {
                gz.write_all(&vec![0u8; padding])?;
            }
            gz.write_all(&[0u8; 1024])?;
            gz.finish()?;
        }

        // restore_project_bundle MUST detect and reject path traversal
        let result = restore_project_bundle(&remote_dir, &restore_dir);
        assert!(result.is_err(), "Path traversal must be blocked with an error");
        let err_msg = result.err().unwrap().to_string();
        assert!(
            err_msg.contains("Path Traversal") || err_msg.contains("Güvenlik Uyarısı"),
            "Error must specify path traversal detection: {}",
            err_msg
        );

        // Ensure the evil file was never created outside the destination directory
        assert!(!temp.path().join("evil.txt").exists(), "Malicious file must NOT be written outside target");

        Ok(())
    }

    #[test]
    fn test_bundle_sync_preserves_git_history_excluding_locks() -> Result<()> {
        let temp = tempdir()?;
        let local_dir = temp.path().join("my-repo");
        let remote_dir = temp.path().join("DriveBackup/my-repo");
        let restore_dir = temp.path().join("restored-repo");

        std::fs::create_dir_all(local_dir.join(".git/refs/heads"))?;
        std::fs::create_dir_all(local_dir.join(".git/objects/ab"))?;
        std::fs::create_dir_all(local_dir.join("src"))?;

        std::fs::write(local_dir.join(".git/HEAD"), "ref: refs/heads/main\n")?;
        std::fs::write(local_dir.join(".git/config"), "[core]\nrepositoryformatversion = 0\n")?;
        std::fs::write(local_dir.join(".git/refs/heads/main"), "11223344556677889900aabbccddeeff11223344\n")?;
        std::fs::write(local_dir.join(".git/objects/ab/cdef1234"), b"OBJ_BYTES")?;
        // Lock file that MUST be excluded
        std::fs::write(local_dir.join(".git/index.lock"), "TEMPORARY LOCK")?;
        std::fs::write(local_dir.join(".git/HEAD.lock"), "TEMPORARY LOCK 2")?;
        std::fs::write(local_dir.join("src/main.rs"), "fn main() { println!(\"git repo\"); }")?;

        // 1. Sync with include_git = true
        let report = sync_project_bundle_with_options(&local_dir, &remote_dir, false, true)?;
        assert!(report.files_packaged >= 5);

        let manifest = read_bundle_manifest(&remote_dir).expect("Manifest must exist");
        let paths: Vec<&str> = manifest.entries.iter().map(|e| e.path.as_str()).collect();
        assert!(paths.contains(&"src/main.rs"));
        assert!(paths.contains(&".git/HEAD"));
        assert!(paths.contains(&".git/config"));
        assert!(paths.contains(&".git/refs/heads/main"));
        assert!(paths.contains(&".git/objects/ab/cdef1234"));
        // Lock files must be excluded
        assert!(!paths.iter().any(|p| p.ends_with(".lock")));

        // 2. Restore and verify
        let restore_report = restore_project_bundle(&remote_dir, &restore_dir)?;
        assert!(restore_dir.join(".git/HEAD").exists());
        assert!(restore_dir.join(".git/config").exists());
        assert!(restore_dir.join(".git/refs/heads/main").exists());
        assert!(restore_dir.join(".git/objects/ab/cdef1234").exists());
        assert!(!restore_dir.join(".git/index.lock").exists(), "Lock files must never be restored");
        assert!(!restore_dir.join(".git/HEAD.lock").exists(), "Lock files must never be restored");
        assert!(restore_report.hints.iter().any(|h| h.contains("Git geçmişi")));

        // 3. Sync with include_git = false
        let remote_nogit = temp.path().join("DriveBackup/nogit");
        let restore_nogit = temp.path().join("restored-nogit");
        let report_nogit = sync_project_bundle_with_options(&local_dir, &remote_nogit, false, false)?;
        assert_eq!(report_nogit.files_packaged, 1); // only src/main.rs

        let _ = restore_project_bundle(&remote_nogit, &restore_nogit)?;
        assert!(!restore_nogit.join(".git").exists(), "With include_git=false, .git must NOT be present");
        assert!(restore_nogit.join("src/main.rs").exists());

        Ok(())
    }
}
