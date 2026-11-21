use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::io::Write;
use std::path::Path;
use tracing::{info, warn};
use walkdir::WalkDir;

use crate::cache::CacheManager;
use crate::db::Database;
use crate::filter::{DevFilter, FileCategory};

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct SnapshotFileEntry {
    pub relative_path: String,
    pub sha256: String,
    pub size_bytes: u64,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct SnapshotManifest {
    pub id: String,
    pub project_name: String,
    pub message: String,
    pub created_at: String,
    pub files: HashMap<String, SnapshotFileEntry>,
    pub total_size_bytes: u64,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct DiffEntry {
    pub path: String,
    pub status: String, // "modified", "added_locally", "missing_locally", "identical"
    pub local_sha256: Option<String>,
    pub snapshot_sha256: Option<String>,
    pub local_size: Option<u64>,
    pub snapshot_size: Option<u64>,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct SnapshotDiffReport {
    pub snapshot_id: String,
    pub message: String,
    pub created_at: String,
    pub entries: Vec<DiffEntry>,
    pub modified_count: usize,
    pub added_locally_count: usize,
    pub missing_locally_count: usize,
    pub identical_count: usize,
}

pub struct SnapshotEngine<'a> {
    db: &'a mut Database,
    cache_mgr: &'a CacheManager,
    filter: DevFilter,
}

impl<'a> SnapshotEngine<'a> {
    pub fn new(db: &'a mut Database, cache_mgr: &'a CacheManager) -> Self {
        Self {
            db,
            cache_mgr,
            filter: DevFilter::new(),
        }
    }

    /// Captures a point-in-time micro-snapshot of the project
    pub fn create_snapshot<P: AsRef<Path>>(
        &mut self,
        project_path: P,
        message: &str,
    ) -> Result<SnapshotManifest> {
        let project_path = project_path.as_ref().canonicalize()?;
        self.filter = DevFilter::for_project(&project_path);
        let path_str = project_path.to_string_lossy().to_string();

        let project = self
            .db
            .get_project_by_local_path(&path_str)?
            .with_context(|| format!("Project at {} is not registered", path_str))?;

        let timestamp_now = chrono::Utc::now();
        let nanos = timestamp_now.timestamp_nanos_opt().unwrap_or(0);
        let snap_id = format!(
            "snap_{}_{:08x}",
            timestamp_now.format("%Y%m%d_%H%M%S"),
            (nanos & 0xFFFFFFFF) as u32
        );

        let mut manifest_files = HashMap::new();
        let mut total_size = 0u64;

        for entry in WalkDir::new(&project_path)
            .into_iter()
            .filter_entry(|e| {
                if e.file_type().is_dir() && e.depth() > 0 {
                    !self.filter.should_skip_dir_entry(e.path(), e.depth() == 1)
                } else {
                    true
                }
            })
            .filter_map(|e| e.ok())
        {
            let file_type = entry.file_type();
            if !file_type.is_file() {
                continue;
            }

            let full_path = entry.path();
            let relative = full_path
                .strip_prefix(&project_path)?
                .to_path_buf();

            if self.filter.categorize(&relative) != FileCategory::Source {
                continue;
            }

            let (hash, size) = self.cache_mgr.store_file(full_path)?;

            total_size += size;
            manifest_files.insert(
                relative.to_string_lossy().to_string(),
                SnapshotFileEntry {
                    relative_path: relative.to_string_lossy().to_string(),
                    sha256: hash,
                    size_bytes: size,
                },
            );
        }

        let manifest = SnapshotManifest {
            id: snap_id.clone(),
            project_name: project.name,
            message: message.to_string(),
            created_at: timestamp_now.to_rfc3339(),
            files: manifest_files,
            total_size_bytes: total_size,
        };

        let json = serde_json::to_string(&manifest)?;
        self.db.save_snapshot(&snap_id, project.id, message, &json)?;

        info!(
            "Captured micro-snapshot: {} ({} files, {} bytes)",
            snap_id,
            manifest.files.len(),
            total_size
        );

        Ok(manifest)
    }

    /// Rolls back the project to the exact state recorded in the snapshot
    pub fn rollback_to_snapshot<P: AsRef<Path>>(
        &mut self,
        project_path: P,
        snapshot_id: &str,
    ) -> Result<()> {
        let project_path = project_path.as_ref().canonicalize()?;
        self.filter = DevFilter::for_project(&project_path);
        let snapshot_record = self
            .db
            .get_snapshot(snapshot_id)?
            .with_context(|| format!("Snapshot {} not found", snapshot_id))?;

        let manifest: SnapshotManifest = serde_json::from_str(&snapshot_record.manifest_json)?;

        // 1. Automatic pre-rollback safety snapshot to prevent accidental loss
        match self.create_snapshot(
            &project_path,
            &format!("Safety auto-snapshot before rollback to {}", snapshot_id),
        ) {
            Ok(safety_snap) => {
                info!("📸 Pre-rollback auto-snapshot secured: {}", safety_snap.id);
            }
            Err(e) => {
                warn!("Could not create pre-rollback auto-snapshot: {}", e);
            }
        }

        info!(
            "Restoring snapshot {} ({}) to {}",
            manifest.id,
            manifest.message,
            project_path.display()
        );

        // 1.5. Pre-flight verification: Ensure all CAS objects exist BEFORE removing any local files!
        for (rel_path, file_entry) in &manifest.files {
            if self.cache_mgr.get_content(&file_entry.sha256)?.is_none() {
                bail!(
                    "Geri yükleme iptal edildi: '{}' dosyasına ait içerik ({}) CAS önbelleğinde bulunamadı. Hiçbir yerel dosya silinmedi.",
                    rel_path,
                    file_entry.sha256
                );
            }
        }

        // 2. Detect and remove extraneous files added locally after the snapshot
        for entry in WalkDir::new(&project_path)
            .into_iter()
            .filter_entry(|e| {
                if e.file_type().is_dir() && e.depth() > 0 {
                    !self.filter.should_skip_dir_entry(e.path(), e.depth() == 1)
                } else {
                    true
                }
            })
            .filter_map(|e| e.ok())
        {
            if entry.file_type().is_file() {
                let full_path = entry.path();
                if let Ok(relative) = full_path.strip_prefix(&project_path)
                    && self.filter.categorize(relative) == FileCategory::Source {
                        let rel_str = relative.to_string_lossy().to_string();
                        if !manifest.files.contains_key(&rel_str) {
                            if let Err(e) = std::fs::remove_file(full_path) {
                                warn!("Could not remove post-snapshot extraneous file {}: {}", rel_str, e);
                            } else {
                                info!("Rollback cleanup: removed extraneous file {}", rel_str);
                            }
                        }
                    }
            }
        }

        // 3. Atomically restore all files from CAS
        for (rel_path, file_entry) in &manifest.files {
            let target_file = project_path.join(rel_path);
            let parent = target_file.parent().context("Target file has no parent")?;
            std::fs::create_dir_all(parent)?;

            let content = self
                .cache_mgr
                .get_content(&file_entry.sha256)?
                .with_context(|| {
                    format!(
                        "Content for {} (hash {}) missing from CAS cache",
                        rel_path, file_entry.sha256
                    )
                })?;

            let tmp_name = format!(
                ".restore_tmp_{}_{}",
                std::process::id(),
                chrono::Utc::now().timestamp_nanos_opt().unwrap_or(0)
            );
            let tmp_path = parent.join(tmp_name);
            {
                let mut f = std::fs::File::create(&tmp_path)?;
                f.write_all(&content)?;
                f.sync_all()?;
            }
            std::fs::rename(&tmp_path, &target_file)?;
        }

        Ok(())
    }

    /// Compares the current local project state against a snapshot without modifying any files
    pub fn diff_snapshot<P: AsRef<Path>>(
        &mut self,
        project_path: P,
        snapshot_id: &str,
    ) -> Result<SnapshotDiffReport> {
        let project_path = project_path.as_ref().canonicalize()?;
        let snapshot_record = self
            .db
            .get_snapshot(snapshot_id)?
            .with_context(|| format!("Snapshot {} not found", snapshot_id))?;

        let manifest: SnapshotManifest = serde_json::from_str(&snapshot_record.manifest_json)?;
        let filter = DevFilter::for_project(&project_path);

        // Collect current local source files
        let mut local_files: HashMap<String, (String, u64)> = HashMap::new();
        for entry in WalkDir::new(&project_path)
            .into_iter()
            .filter_entry(|e| {
                if e.file_type().is_dir() && e.depth() > 0 {
                    !filter.should_skip_dir_entry(e.path(), e.depth() == 1)
                } else {
                    true
                }
            })
            .filter_map(|e| e.ok()) {
            if entry.file_type().is_file() {
                let full_path = entry.path();
                if let Ok(rel) = full_path.strip_prefix(&project_path)
                    && filter.categorize(rel) == FileCategory::Source {
                        let rel_str = rel.to_string_lossy().to_string();
                        let size = entry.metadata().map(|m| m.len()).unwrap_or(0);
                        let hash = CacheManager::compute_sha256(full_path).unwrap_or_default();
                        local_files.insert(rel_str, (hash, size));
                    }
            }
        }

        let mut entries = Vec::new();
        let mut modified_count = 0;
        let mut added_locally_count = 0;
        let mut missing_locally_count = 0;
        let mut identical_count = 0;

        for (snap_path, snap_entry) in &manifest.files {
            if let Some((loc_hash, loc_size)) = local_files.remove(snap_path) {
                if loc_hash == snap_entry.sha256 {
                    identical_count += 1;
                    entries.push(DiffEntry {
                        path: snap_path.clone(),
                        status: "identical".to_string(),
                        local_sha256: Some(loc_hash),
                        snapshot_sha256: Some(snap_entry.sha256.clone()),
                        local_size: Some(loc_size),
                        snapshot_size: Some(snap_entry.size_bytes),
                    });
                } else {
                    modified_count += 1;
                    entries.push(DiffEntry {
                        path: snap_path.clone(),
                        status: "modified".to_string(),
                        local_sha256: Some(loc_hash),
                        snapshot_sha256: Some(snap_entry.sha256.clone()),
                        local_size: Some(loc_size),
                        snapshot_size: Some(snap_entry.size_bytes),
                    });
                }
            } else {
                missing_locally_count += 1;
                entries.push(DiffEntry {
                    path: snap_path.clone(),
                    status: "missing_locally".to_string(),
                    local_sha256: None,
                    snapshot_sha256: Some(snap_entry.sha256.clone()),
                    local_size: None,
                    snapshot_size: Some(snap_entry.size_bytes),
                });
            }
        }

        for (loc_path, (loc_hash, loc_size)) in local_files {
            added_locally_count += 1;
            entries.push(DiffEntry {
                path: loc_path,
                status: "added_locally".to_string(),
                local_sha256: Some(loc_hash),
                snapshot_sha256: None,
                local_size: Some(loc_size),
                snapshot_size: None,
            });
        }

        entries.sort_by(|a, b| a.path.cmp(&b.path));

        Ok(SnapshotDiffReport {
            snapshot_id: manifest.id,
            message: manifest.message,
            created_at: manifest.created_at,
            entries,
            modified_count,
            added_locally_count,
            missing_locally_count,
            identical_count,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn test_snapshot_create_and_rollback() -> Result<()> {
        let temp = tempdir()?;
        let cache_dir = temp.path().join(".cache");
        let project_dir = temp.path().join("my-project");
        std::fs::create_dir_all(&project_dir)?;

        let mut db = Database::open(":memory:")?;
        db.register_project(
            "my-project",
            &project_dir.canonicalize()?.to_string_lossy(),
            "drive://my-project",
        )?;

        let cache_mgr = CacheManager::new(&cache_dir, 1024 * 1024 * 1024)?;

        // Create some initial files
        let file1 = project_dir.join("main.py");
        let node_modules_file = project_dir.join("node_modules").join("pkg.js");
        std::fs::create_dir_all(project_dir.join("node_modules"))?;

        std::fs::write(&file1, "print('Version 1')")?;
        std::fs::write(&node_modules_file, "// should be ignored")?;

        let mut engine = SnapshotEngine::new(&mut db, &cache_mgr);
        let snap1 = engine.create_snapshot(&project_dir, "Initial commit snapshot")?;
        assert_eq!(snap1.files.len(), 1); // Only main.py, node_modules ignored!

        // Modify file1
        std::fs::write(&file1, "print('Version 2 - Broken Code')")?;

        // Rollback
        engine.rollback_to_snapshot(&project_dir, &snap1.id)?;
        let restored_content = std::fs::read_to_string(&file1)?;
        assert_eq!(restored_content, "print('Version 1')");

        Ok(())
    }

    #[test]
    fn test_snapshot_diff() -> Result<()> {
        let temp = tempdir()?;
        let cache_dir = temp.path().join(".cache");
        let project_dir = temp.path().join("diff-project");
        std::fs::create_dir_all(&project_dir)?;

        let mut db = Database::open(":memory:")?;
        db.register_project(
            "diff-project",
            &project_dir.canonicalize()?.to_string_lossy(),
            "drive://diff-project",
        )?;

        let cache_mgr = CacheManager::new(&cache_dir, 1024 * 1024 * 1024)?;
        let mut engine = SnapshotEngine::new(&mut db, &cache_mgr);

        // Initial files: main.py, utils.py
        std::fs::write(project_dir.join("main.py"), "code v1")?;
        std::fs::write(project_dir.join("utils.py"), "utils v1")?;

        let snap = engine.create_snapshot(&project_dir, "base snapshot")?;

        // State change:
        // 1. main.py modified
        std::fs::write(project_dir.join("main.py"), "code v2 - modified")?;
        // 2. utils.py deleted
        std::fs::remove_file(project_dir.join("utils.py"))?;
        // 3. extra.py added
        std::fs::write(project_dir.join("extra.py"), "extra locally added")?;

        let diff = engine.diff_snapshot(&project_dir, &snap.id)?;

        assert_eq!(diff.modified_count, 1);
        assert_eq!(diff.missing_locally_count, 1);
        assert_eq!(diff.added_locally_count, 1);
        assert_eq!(diff.identical_count, 0);

        let main_diff = diff.entries.iter().find(|e| e.path == "main.py").unwrap();
        assert_eq!(main_diff.status, "modified");

        let utils_diff = diff.entries.iter().find(|e| e.path == "utils.py").unwrap();
        assert_eq!(utils_diff.status, "missing_locally");

        let extra_diff = diff.entries.iter().find(|e| e.path == "extra.py").unwrap();
        assert_eq!(extra_diff.status, "added_locally");

        Ok(())
    }
}
