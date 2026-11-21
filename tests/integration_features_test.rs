use anyhow::Result;
use devdrive::cache::CacheManager;
use devdrive::db::Database;
use devdrive::sync::{read_bundle_manifest, restore_project_bundle, sync_project_bundle, SyncEngine};
use devdrive::watcher::is_fast_ignored_path;
use devdrive::wal::{ChangeType, WalManager};
use std::path::Path;
use std::process::Command;
use tempfile::tempdir;

#[test]
fn test_bundle_sync_git_repo_roundtrip_preserves_commits_branches_and_excludes_locks() -> Result<()> {
    let temp = tempdir()?;
    let local_dir = temp.path().join("local_git_repo");
    let remote_dir = temp.path().join("DriveRemote/local_git_repo");
    let restore_dir = temp.path().join("restored_git_repo");

    std::fs::create_dir_all(&local_dir)?;

    // 1. Initialize real Git repo
    let status_init = Command::new("git")
        .arg("init")
        .current_dir(&local_dir)
        .status()?;
    assert!(status_init.success());

    let _ = Command::new("git")
        .args(["config", "user.name", "DevDrive Bot"])
        .current_dir(&local_dir)
        .status()?;
    let _ = Command::new("git")
        .args(["config", "user.email", "bot@devdrive.local"])
        .current_dir(&local_dir)
        .status()?;

    // Commit 1 on main branch
    std::fs::write(local_dir.join("main.rs"), "fn main() { println!(\"v1\"); }\n")?;
    let _ = Command::new("git").args(["add", "."]).current_dir(&local_dir).status()?;
    let c1 = Command::new("git").args(["commit", "-m", "Commit 1: Initial"]).current_dir(&local_dir).status()?;
    assert!(c1.success());

    // Create branch feature/bundle and commit 2
    let b_status = Command::new("git").args(["checkout", "-b", "feature/bundle"]).current_dir(&local_dir).status()?;
    assert!(b_status.success());
    std::fs::write(local_dir.join("bundle.rs"), "pub fn bundle() {}\n")?;
    let _ = Command::new("git").args(["add", "."]).current_dir(&local_dir).status()?;
    let c2 = Command::new("git").args(["commit", "-m", "Commit 2: Add bundle feature"]).current_dir(&local_dir).status()?;
    assert!(c2.success());

    // Switch back to master/main
    let checkout_main = Command::new("git").args(["checkout", "main"]).current_dir(&local_dir).status()
        .or_else(|_| Command::new("git").args(["checkout", "master"]).current_dir(&local_dir).status())?;
    let _ = checkout_main;

    // Simulate transient lock files in .git
    std::fs::write(local_dir.join(".git/index.lock"), "LOCK")?;
    std::fs::write(local_dir.join(".git/HEAD.lock"), "LOCK")?;

    // 2. Perform Bundle Sync (Offline, tempdir)
    let sync_report = sync_project_bundle(&local_dir, &remote_dir, false)?;
    assert!(sync_report.files_packaged > 0);
    assert!(remote_dir.join("bundle.tar.gz").exists());
    assert!(remote_dir.join("manifest.json").exists());

    // Verify manifest has git files but NO lock files
    let manifest = read_bundle_manifest(&remote_dir).expect("Manifest must be readable");
    let manifest_paths: Vec<&str> = manifest.entries.iter().map(|e| e.path.as_str()).collect();
    assert!(manifest_paths.iter().any(|p| p.starts_with(".git/")));
    assert!(!manifest_paths.iter().any(|p| p.ends_with(".lock")));

    // 3. Restore to target directory
    let restore_report = restore_project_bundle(&remote_dir, &restore_dir)?;
    assert!(restore_report.files_restored > 0);
    assert!(restore_dir.join(".git").is_dir());
    assert!(!restore_dir.join(".git/index.lock").exists());
    assert!(!restore_dir.join(".git/HEAD.lock").exists());
    assert!(restore_report.hints.iter().any(|h| h.contains("Git geçmişi")));

    // 4. Verify Git history and branches work in restored repo
    let log_output = Command::new("git")
        .args(["log", "--oneline"])
        .current_dir(&restore_dir)
        .output()?;
    assert!(log_output.status.success());
    let log_str = String::from_utf8_lossy(&log_output.stdout);
    assert!(log_str.contains("Commit 1: Initial"), "Commit 1 must be present in git log: {}", log_str);

    let branch_output = Command::new("git")
        .args(["branch", "-a"])
        .current_dir(&restore_dir)
        .output()?;
    assert!(branch_output.status.success());
    let branch_str = String::from_utf8_lossy(&branch_output.stdout);
    assert!(branch_str.contains("feature/bundle"), "Feature branch must be present in git branches: {}", branch_str);

    Ok(())
}

#[test]
fn test_sync_bundle_prunes_wal_events_and_updates_last_synced() -> Result<()> {
    let temp = tempdir()?;
    let local_dir = temp.path().join("my_project");
    let remote_dir = temp.path().join("DriveRemote/my_project");
    let cache_dir = temp.path().join("cache");

    std::fs::create_dir_all(&local_dir)?;
    std::fs::create_dir_all(&remote_dir)?;
    std::fs::create_dir_all(&cache_dir)?;

    std::fs::write(local_dir.join("src_code.rs"), "pub fn test() {}")?;

    let mut db = Database::open(":memory:")?;
    let proj = db.register_project(
        "my_project",
        &local_dir.canonicalize()?.to_string_lossy(),
        &remote_dir.to_string_lossy(),
    )?;

    // Record WAL events
    {
        let mut wal = WalManager::new(&mut db);
        wal.record_change(proj.id, ChangeType::Created, Path::new("src_code.rs"))?;
        wal.record_change(proj.id, ChangeType::Modified, Path::new("src_code.rs"))?;
    }

    let pending_before = db.get_pending_wal_events(proj.id)?;
    assert_eq!(pending_before.len(), 2);

    let cache_mgr = CacheManager::new(&cache_dir, 1024 * 1024 * 1024)?;
    let mut sync = SyncEngine::new(&mut db, &cache_mgr);

    // Run sync_bundle on SyncEngine
    let report = sync.sync_bundle(&local_dir, &remote_dir, false)?;
    assert_eq!(report.files_packaged, 1);

    // Verify WAL events are now completely pruned from the database
    let pending_after = db.get_pending_wal_events(proj.id)?;
    assert_eq!(pending_after.len(), 0, "All pending WAL events must be marked synced");

    // Total rows in wal_events must be 0 (no lingering rows with synced=1)
    let total_rows = db.get_total_wal_events_count()?;
    assert_eq!(total_rows, 0, "WAL events must be fully pruned from the database table");

    // Verify last_synced_at was updated
    let updated_proj = db.get_project_by_id(proj.id)?.unwrap();
    assert!(updated_proj.last_synced_at.is_some(), "Project last_synced_at timestamp must be updated");

    Ok(())
}

#[test]
fn test_watcher_fast_path_filtering() {
    let fast_paths = [
        "/Users/dev/workspace/node_modules/lodash/index.js",
        "/Users/dev/workspace/.git/refs/heads/main",
        "/Users/dev/workspace/target/debug/app",
        "/Users/dev/workspace/.venv/bin/python",
        "/Users/dev/workspace/.next/server/pages.js",
        "/Users/dev/workspace/DerivedData/Build/Intermediates",
        "/Users/dev/workspace/.dartServer/cache/data.bin",
    ];

    for path in fast_paths {
        assert!(
            is_fast_ignored_path(Path::new(path)),
            "Path '{}' must be immediately fast-path ignored",
            path
        );
    }

    let legitimate_paths = [
        "/Users/dev/workspace/src/lib.rs",
        "/Users/dev/workspace/target_config.json",
        "/Users/dev/workspace/components/node_modules_viewer.tsx",
        "/Users/dev/workspace/package.json",
        "/Users/dev/workspace/Cargo.toml",
    ];

    for path in legitimate_paths {
        assert!(
            !is_fast_ignored_path(Path::new(path)),
            "Legitimate path '{}' must NOT be fast-path ignored",
            path
        );
    }
}

#[test]
fn test_db_prune_synced_wal_events_explicit() -> Result<()> {
    let mut db = Database::open(":memory:")?;
    let proj = db.register_project("test_app", "/tmp/test_app", "drive://test_app")?;

    db.record_wal_event(proj.id, "created", "file_a.rs", 1000)?;
    db.record_wal_event(proj.id, "modified", "file_b.rs", 2000)?;
    db.record_wal_event(proj.id, "modified", "file_c.rs", 3000)?;

    let pending = db.get_pending_wal_events(proj.id)?;
    assert_eq!(pending.len(), 3);

    // Mark 2 events synced (which automatically executes prune_synced_wal_events)
    db.mark_wal_events_synced(&[pending[0].id, pending[1].id])?;

    // Exactly 1 unsynced row remains in table
    assert_eq!(db.get_total_wal_events_count()?, 1);
    let remaining = db.get_pending_wal_events(proj.id)?;
    assert_eq!(remaining.len(), 1);
    assert_eq!(remaining[0].id, pending[2].id);

    // Explicit manual prune returns 0 since already pruned
    let pruned_count = db.prune_synced_wal_events()?;
    assert_eq!(pruned_count, 0);

    Ok(())
}
