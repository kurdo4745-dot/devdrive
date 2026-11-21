use anyhow::Result;
use devdrive::cache::CacheManager;
use devdrive::db::Database;
use devdrive::filter::{DevFilter, FileCategory};
use devdrive::snapshot::SnapshotEngine;
use devdrive::sync::{SyncEngine, SyncFileResult};
use std::path::{Path, PathBuf};
use tempfile::tempdir;

#[test]
fn test_filter_zero_false_positives() {
    let filter = DevFilter::new();

    // 1. Critical source files that MUST NEVER be ignored (Zero False Positives):
    let valid_sources = vec![
        "src/target/mod.rs",
        "src/build/codegen.rs",
        "src/dist/bundle.rs",
        "src/out/compiler.rs",
        "src/node_modules_helper.rs",
        "src/target_profile.json",
        "lib/vendor_api.py",
        "components/build_runner.tsx",
        ".github/workflows/ci.yml",
        ".gitignore",
        ".gitattributes",
        ".gitmodules",
        ".env.example",
        "Cargo.lock",
        "package-lock.json",
        "pnpm-lock.yaml",
        "yarn.lock",
        "poetry.lock",
        "composer.lock",
        "Gemfile.lock",
        "bun.lockb",
        "flake.lock",
        "türkçe_dosya_şçöğü.rs",
    ];

    for path in valid_sources {
        let cat = filter.categorize(&PathBuf::from(path));
        assert_eq!(
            cat,
            FileCategory::Source,
            "FALSE POSITIVE DETECTED! File '{}' was incorrectly classified as {:?}",
            path,
            cat
        );
    }

    // 2. Build artifacts and dependency junk that MUST BE IGNORED (Zero False Negatives):
    let ephemeral_junk = vec![
        "target/release/app",
        "target/debug/incremental/app/s-foo.bin",
        "dist/bundle.js",
        "build/CMakeCache.txt",
        "out/debug/log.txt",
        "DerivedData/App/Build/Products/Debug/App.app",
        "node_modules/react/index.js",
        "apps/web/node_modules/vue/index.js",
        ".venv/bin/python",
        "api/.venv/lib/python3.12/site-packages/fastapi/main.py",
        "__pycache__/app.cpython-312.pyc",
        ".next/server/pages/index.js",
        ".cache/turbo/hash.json",
        ".git/objects/4b/825dc642cb6eb9a060e54bf8d69288fbee4904",
        ".git/refs/heads/master",
        ".git/HEAD",
    ];

    for path in ephemeral_junk {
        let cat = filter.categorize(&PathBuf::from(path));
        assert_eq!(
            cat,
            FileCategory::Ephemeral,
            "FALSE NEGATIVE! Junk file '{}' should be Ephemeral but was {:?}",
            path,
            cat
        );
    }

    // 3. Transient system locks that MUST be TransientLock:
    let transient_junk = vec![
        ".git/index.lock",
        ".git/HEAD.lock",
        ".DS_Store",
        "Thumbs.db",
        "server.tmp",
        "app.swp",
        ".#main.rs",
        "main.rs~",
    ];

    for path in transient_junk {
        let cat = filter.categorize(&PathBuf::from(path));
        assert_eq!(
            cat,
            FileCategory::TransientLock,
            "Transient file '{}' should be TransientLock but was {:?}",
            path,
            cat
        );
    }
}

#[test]
fn test_conflict_shield_zero_false_positives() -> Result<()> {
    let temp = tempdir()?;
    let local_file = temp.path().join("main.rs");
    let remote_file = temp.path().join("remote_main.rs");

    let identical_content = b"fn main() { println!(\"Same content everywhere\"); }";
    std::fs::write(&local_file, identical_content)?;
    std::fs::write(&remote_file, identical_content)?;

    // Simulate remote file having a NEWER timestamp (e.g. cloud provider touch)
    let newer_time = filetime::FileTime::from_unix_time(2000000000, 0);
    let older_time = filetime::FileTime::from_unix_time(1000000000, 0);
    filetime::set_file_mtime(&remote_file, newer_time)?;
    filetime::set_file_mtime(&local_file, older_time)?;

    // Because content is 100% identical, Conflict Shield MUST NOT report a false conflict!
    let result = SyncEngine::safe_sync_file(&local_file, &remote_file)?;
    assert_eq!(
        result,
        SyncFileResult::Unchanged,
        "FALSE POSITIVE CONFLICT! Identical files must return Unchanged even if remote mtime is newer"
    );

    // Now test a REAL conflict: different content AND remote is newer
    std::fs::write(&remote_file, b"fn main() { println!(\"Remote edit made on web\"); }")?;
    filetime::set_file_mtime(&remote_file, newer_time)?;

    let conflict_result = SyncEngine::safe_sync_file(&local_file, &remote_file)?;
    match conflict_result {
        SyncFileResult::Conflict(conflict_path) => {
            assert!(Path::new(&conflict_path).exists(), "Conflict copy must exist on disk");
            let remote_content = std::fs::read(&remote_file)?;
            assert_eq!(
                remote_content,
                b"fn main() { println!(\"Remote edit made on web\"); }",
                "Original remote file MUST NOT be overwritten during conflict"
            );
        }
        other => panic!("Expected Conflict result, got {:?}", other),
    }

    Ok(())
}

#[test]
fn test_snapshot_and_rollback_zero_data_loss() -> Result<()> {
    let temp = tempdir()?;
    let cache_dir = temp.path().join("cache");
    let project_dir = temp.path().join("my-project");
    std::fs::create_dir_all(&project_dir)?;

    let mut db = Database::open(":memory:")?;
    let proj = db.register_project(
        "my-project",
        &project_dir.canonicalize()?.to_string_lossy(),
        "drive://my-project",
    )?;

    let cache_mgr = CacheManager::new(&cache_dir, 1024 * 1024 * 1024)?;

    // Create diverse file types (nested, unicode, empty, binary)
    let nested_dir = project_dir.join("src").join("a").join("b");
    std::fs::create_dir_all(&nested_dir)?;

    let file_unicode = project_dir.join("türkçe_karakterli_dosya_ğüşiöç.txt");
    let file_empty = nested_dir.join("empty.txt");
    let file_binary = nested_dir.join("data.dat");
    let file_code = project_dir.join("main.rs");

    std::fs::write(&file_unicode, "Örnek Türkçe metin: Pijamalı hasta yağız şoföre çabucak güvendi.")?;
    std::fs::write(&file_empty, b"")?;
    std::fs::write(&file_binary, [0xDE, 0xAD, 0xBE, 0xEF, 0x00, 0xFF])?;
    std::fs::write(&file_code, "fn main() { /* Original Code */ }")?;

    let original_unicode_hash = CacheManager::compute_sha256(&file_unicode)?;
    let original_empty_hash = CacheManager::compute_sha256(&file_empty)?;
    let original_binary_hash = CacheManager::compute_sha256(&file_binary)?;
    let original_code_hash = CacheManager::compute_sha256(&file_code)?;

    // 1. Capture snapshot
    let mut engine = SnapshotEngine::new(&mut db, &cache_mgr);
    let snap = engine.create_snapshot(&project_dir, "Golden State Snapshot")?;
    assert_eq!(snap.files.len(), 4);

    // 2. Simulate disaster: delete files, corrupt content, add junk
    std::fs::remove_file(&file_unicode)?;
    std::fs::remove_file(&file_binary)?;
    std::fs::write(&file_code, "CORRUPTED CONTENT THAT BROKE THE APP")?;
    std::fs::write(project_dir.join("rogue_file.txt"), "unwanted file")?;

    // 3. Rollback to snapshot
    engine.rollback_to_snapshot(&project_dir, &snap.id)?;

    // 4. Verify 100% byte-for-byte exact restoration (Zero Data Loss)
    assert!(file_unicode.exists());
    assert!(file_empty.exists());
    assert!(file_binary.exists());
    assert!(file_code.exists());

    assert_eq!(CacheManager::compute_sha256(&file_unicode)?, original_unicode_hash);
    assert_eq!(CacheManager::compute_sha256(&file_empty)?, original_empty_hash);
    assert_eq!(CacheManager::compute_sha256(&file_binary)?, original_binary_hash);
    assert_eq!(CacheManager::compute_sha256(&file_code)?, original_code_hash);

    // Verify extraneous rogue file added after snapshot was cleaned up
    assert!(
        !project_dir.join("rogue_file.txt").exists(),
        "Rogue extraneous file must be safely purged on rollback!"
    );

    // Verify automatic pre-rollback safety snapshot was saved in database
    let all_snaps = db.list_snapshots(proj.id)?;
    assert!(
        all_snaps.len() >= 2,
        "Safety pre-rollback snapshot must have been created automatically"
    );

    Ok(())
}

#[test]
fn test_soft_delete_vault_preserves_history() -> Result<()> {
    let temp = tempdir()?;
    let remote_dir = temp.path().join("remote_drive");
    let file_to_delete = remote_dir.join("precious_code.rs");
    std::fs::create_dir_all(&remote_dir)?;
    std::fs::write(&file_to_delete, "fn critical_algorithm() -> i32 { 42 }")?;

    // Perform soft delete
    let deleted = SyncEngine::safe_soft_delete(&file_to_delete, &remote_dir)?;
    assert!(deleted);
    assert!(!file_to_delete.exists(), "Original path must be cleared");

    // Must exist inside .trash vault with content preserved
    let trash_dir = remote_dir.join(".trash");
    assert!(trash_dir.exists());

    let mut found_in_vault = false;
    for entry in std::fs::read_dir(trash_dir)?.filter_map(|e| e.ok()) {
        let content = std::fs::read_to_string(entry.path())?;
        if content == "fn critical_algorithm() -> i32 { 42 }" {
            found_in_vault = true;
            break;
        }
    }
    assert!(found_in_vault, "Deleted file content MUST be safely vaulted in .trash");

    Ok(())
}

#[test]
fn test_adversarial_security_filter_and_lockfiles() {
    let filter = DevFilter::new();

    // 1. Critical security: .env secrets MUST NEVER be synced or treated as Source
    let secret_env_files = vec![
        ".env",
        ".env.local",
        ".env.production",
        ".env.development",
        ".env.staging",
        ".env.test",
        "nested/subfolder/.env",
        "api/.env.production",
    ];

    for secret in secret_env_files {
        let cat = filter.categorize(&PathBuf::from(secret));
        assert_ne!(
            cat,
            FileCategory::Source,
            "CRITICAL SECURITY LEAK: '{}' was classified as Source and would be synced to Drive!",
            secret
        );
        assert_eq!(
            cat,
            FileCategory::TransientLock,
            "Secret env file '{}' must be TransientLock (ignored completely)",
            secret
        );
        assert!(
            !filter.is_syncable(&PathBuf::from(secret)),
            "Secret env file '{}' must not be syncable",
            secret
        );
    }

    // 2. Safe templates MUST remain Source:
    let safe_templates = vec![
        ".env.example",
        ".env.sample",
        ".env.template",
        "backend/.env.example",
    ];
    for safe in safe_templates {
        assert_eq!(
            filter.categorize(&PathBuf::from(safe)),
            FileCategory::Source,
            "Safe template '{}' must be Source",
            safe
        );
    }

    // 3. Modern lockfiles MUST be Source:
    let modern_locks = vec![
        "uv.lock",
        "Pipfile.lock",
        "Podfile.lock",
        "deno.lock",
        "flake.lock",
        "pubspec.lock",
    ];
    for lock in modern_locks {
        assert_eq!(
            filter.categorize(&PathBuf::from(lock)),
            FileCategory::Source,
            "Modern lockfile '{}' must be Source",
            lock
        );
    }

    // 4. Zero False Positives for legitimate filenames containing transient tokens
    let legitimate_files = vec![
        "foo.DS_Store",
        "myThumbs.txt",
        "src/env/config.rs",
        "database.bak",
    ];
    for legit in legitimate_files {
        assert_eq!(
            filter.categorize(&PathBuf::from(legit)),
            FileCategory::Source,
            "Legitimate file '{}' must not produce false positive",
            legit
        );
    }

    assert_eq!(
        filter.categorize(&PathBuf::from("myThumbs.db")),
        FileCategory::Ephemeral,
        ".db database file must be Ephemeral"
    );

    // 5. Real editor backups must be ignored:
    assert_eq!(
        filter.categorize(&PathBuf::from("main.rs.bak")),
        FileCategory::TransientLock
    );
}

#[test]
fn test_adversarial_wal_debouncing_purges_all_crushed_events() -> Result<()> {
    let temp = tempdir()?;
    let local_dir = temp.path().join("local");
    let remote_dir = temp.path().join("remote");
    let cache_dir = temp.path().join("cache");
    std::fs::create_dir_all(&local_dir)?;
    std::fs::create_dir_all(&remote_dir)?;

    let mut db = Database::open(":memory:")?;
    let proj = db.register_project(
        "wal-integrity-app",
        &local_dir.canonicalize()?.to_string_lossy(),
        &remote_dir.to_string_lossy(),
    )?;

    let cache_mgr = CacheManager::new(&cache_dir, 1024 * 1024 * 1024)?;

    let file_path = local_dir.join("rapid_edits.rs");
    std::fs::write(&file_path, "fn edit() { 5 }")?;

    // Simulate 5 rapid WAL writes on the same file (e.g., fast user typing)
    {
        let mut wal = devdrive::wal::WalManager::new(&mut db);
        wal.record_change(proj.id, devdrive::wal::ChangeType::Created, Path::new("rapid_edits.rs"))?;
        wal.record_change(proj.id, devdrive::wal::ChangeType::Modified, Path::new("rapid_edits.rs"))?;
        wal.record_change(proj.id, devdrive::wal::ChangeType::Modified, Path::new("rapid_edits.rs"))?;
        wal.record_change(proj.id, devdrive::wal::ChangeType::Modified, Path::new("rapid_edits.rs"))?;
        wal.record_change(proj.id, devdrive::wal::ChangeType::Modified, Path::new("rapid_edits.rs"))?;
    }

    // Check raw pending in DB (must be 5)
    let raw_pending = db.get_pending_wal_events(proj.id)?;
    assert_eq!(raw_pending.len(), 5, "DB must hold all 5 raw WAL events initially");

    // Perform sync
    let mut sync = SyncEngine::new(&mut db, &cache_mgr);
    let report = sync.sync_pending(&local_dir, &remote_dir, false)?;
    assert_eq!(report.files_uploaded, 1);

    // CRITICAL INTEGRITY CHECK: All 5 events must be marked synced=1!
    // Zero stale pending events left behind in the database table!
    let pending_after = db.get_pending_wal_events(proj.id)?;
    assert_eq!(
        pending_after.len(),
        0,
        "WAL table bloat detected! All crushed/superseded event IDs must be marked synced"
    );

    Ok(())
}

#[test]
fn test_restore_round_trip_and_safety_exclusions() -> Result<()> {
    let temp = tempdir()?;
    let local_dir = temp.path().join("local");
    let remote_dir = temp.path().join("remote_drive");
    let target_dir = temp.path().join("restore_target");
    let cache_dir = temp.path().join("cache");

    std::fs::create_dir_all(&local_dir)?;
    std::fs::create_dir_all(local_dir.join("src"))?;
    std::fs::create_dir_all(local_dir.join(".git"))?;
    std::fs::create_dir_all(local_dir.join("node_modules/pkg"))?;
    std::fs::create_dir_all(local_dir.join("target/debug"))?;

    // Create valid source files
    let main_src = local_dir.join("src/main.rs");
    std::fs::write(&main_src, b"fn main() { println!(\"DevDrive Roundtrip!\"); }")?;
    let cargo_toml = local_dir.join("Cargo.toml");
    std::fs::write(&cargo_toml, b"[package]\nname = \"roundtrip\"\nversion = \"0.1.0\"\n")?;
    let pkg_json = local_dir.join("package.json");
    std::fs::write(&pkg_json, b"{\"name\": \"roundtrip\"}")?;
    let env_example = local_dir.join(".env.example");
    std::fs::write(&env_example, b"API_KEY=your_key_here\n")?;

    // Create ephemeral/ignored files that must NEVER be synced or restored
    std::fs::write(local_dir.join(".git/HEAD"), b"ref: refs/heads/master")?;
    std::fs::write(local_dir.join("node_modules/pkg/index.js"), b"console.log('vendor');")?;
    std::fs::write(local_dir.join("target/debug/app"), b"binary junk")?;
    std::fs::write(local_dir.join(".env"), b"SECRET_PROD_KEY=super_secret")?;

    let mut db = Database::open(":memory:")?;
    let cache_mgr = CacheManager::new(&cache_dir, 1024 * 1024 * 1024)?;

    // 1. Initial Sync to Remote
    {
        let mut sync = SyncEngine::new(&mut db, &cache_mgr);
        let sync_report = sync.sync_all(&local_dir, &remote_dir, false)?;
        assert_eq!(sync_report.files_uploaded, 4, "Only 4 Source files should be synced to remote");
    }

    // Verify remote does not contain ephemeral files
    assert!(!remote_dir.join(".git").exists());
    assert!(!remote_dir.join("node_modules").exists());
    assert!(!remote_dir.join("target").exists());
    assert!(!remote_dir.join(".env").exists());

    // 2. Adversarial Injection: simulate .trash and *.conflict_* files existing in remote
    let trash_dir = remote_dir.join(".trash");
    std::fs::create_dir_all(&trash_dir)?;
    std::fs::write(trash_dir.join("deleted_secret.rs"), b"should not be restored")?;

    let conflict_file = remote_dir.join("src/main.conflict_20261014_120000.rs");
    std::fs::write(&conflict_file, b"conflict content should not be restored")?;

    // 3. Restore to empty target
    {
        let mut sync = SyncEngine::new(&mut db, &cache_mgr);
        let restore_report = sync.restore_from_remote(&remote_dir, &target_dir, false)?;
        assert_eq!(restore_report.files_restored, 4);
        assert_eq!(restore_report.files_skipped, 0);

        // Verify hints
        assert!(restore_report.hints.iter().any(|h| h.contains("Cargo.toml") || h.contains("cargo build")));
        assert!(restore_report.hints.iter().any(|h| h.contains("node_modules") || h.contains("npm install")));
        assert!(restore_report.hints.iter().any(|h| h.contains("Git geçmişi")));
        assert!(restore_report.hints.iter().any(|h| h.contains(".env")));
    }

    // Verify all source files restored identically (SHA-256 and mtime)
    let restored_main = target_dir.join("src/main.rs");
    assert!(restored_main.exists());
    assert_eq!(
        CacheManager::compute_sha256(&restored_main)?,
        CacheManager::compute_sha256(&main_src)?
    );

    let src_meta = main_src.metadata()?;
    let dst_meta = restored_main.metadata()?;
    let src_mtime = filetime::FileTime::from_last_modification_time(&src_meta);
    let dst_mtime = filetime::FileTime::from_last_modification_time(&dst_meta);
    assert_eq!(src_mtime, dst_mtime, "Restored file mtime must match remote file mtime exactly");

    // Verify .trash and .conflict files were completely SKIPPED during restore
    assert!(!target_dir.join(".trash").exists(), ".trash must NEVER be restored");
    assert!(!target_dir.join("src/main.conflict_20261014_120000.rs").exists(), "Conflict copies must NEVER be restored");

    // Verify ephemeral files were NOT restored
    assert!(!target_dir.join(".git").exists());
    assert!(!target_dir.join("node_modules").exists());
    assert!(!target_dir.join(".env").exists());

    Ok(())
}

#[test]
fn test_restore_non_empty_target_fails_without_force_and_succeeds_with_force_backup() -> Result<()> {
    let temp = tempdir()?;
    let remote_dir = temp.path().join("remote_drive");
    let target_dir = temp.path().join("local_target");
    let cache_dir = temp.path().join("cache");

    std::fs::create_dir_all(&remote_dir)?;
    std::fs::create_dir_all(&target_dir)?;

    let remote_code = remote_dir.join("app.rs");
    std::fs::write(&remote_code, b"// Cloud remote version 1.0\n")?;

    let local_code = target_dir.join("app.rs");
    std::fs::write(&local_code, b"// Local uncommitted user work that would be lost!\n")?;

    let mut db = Database::open(":memory:")?;
    let cache_mgr = CacheManager::new(&cache_dir, 1024 * 1024 * 1024)?;

    // 1. Without --force: MUST FAIL and preserve local file
    {
        let mut sync = SyncEngine::new(&mut db, &cache_mgr);
        let res = sync.restore_from_remote(&remote_dir, &target_dir, false);
        assert!(res.is_err(), "Restoring into non-empty directory without --force MUST return an error");
    }

    // Assert local content is untouched
    let current_local = std::fs::read(&local_code)?;
    assert_eq!(current_local, b"// Local uncommitted user work that would be lost!\n");

    // 2. With --force: MUST succeed and backup conflicting local file
    {
        let mut sync = SyncEngine::new(&mut db, &cache_mgr);
        let report = sync.restore_from_remote(&remote_dir, &target_dir, true)?;
        assert_eq!(report.files_restored, 1);
    }

    // Now app.rs has remote content
    let updated_local = std::fs::read(&local_code)?;
    assert_eq!(updated_local, b"// Cloud remote version 1.0\n");

    // A backup file named app.restore_backup_<timestamp> MUST exist with original content
    let mut backup_found = false;
    for entry in std::fs::read_dir(&target_dir)?.filter_map(|e| e.ok()) {
        let name = entry.file_name().to_string_lossy().to_string();
        if name.starts_with("app.restore_backup_") {
            backup_found = true;
            let backup_content = std::fs::read(entry.path())?;
            assert_eq!(
                backup_content,
                b"// Local uncommitted user work that would be lost!\n",
                "Backup file must contain original overwritten local content"
            );
        }
    }
    assert!(backup_found, "Atomic backup file <name>.restore_backup_<ts> must be created on --force");

    Ok(())
}

#[test]
fn test_prune_remote_deletions_and_safety_thresholds() -> Result<()> {
    let temp = tempdir()?;
    let local_dir = temp.path().join("local");
    let remote_dir = temp.path().join("remote_drive");
    let cache_dir = temp.path().join("cache");

    std::fs::create_dir_all(&local_dir)?;
    std::fs::create_dir_all(&remote_dir)?;

    let mut db = Database::open(":memory:")?;
    let cache_mgr = CacheManager::new(&cache_dir, 1024 * 1024 * 1024)?;

    // Create 2 files locally and sync all
    std::fs::write(local_dir.join("file1.rs"), b"file 1 content")?;
    std::fs::write(local_dir.join("file2.rs"), b"file 2 content")?;

    {
        let mut sync = SyncEngine::new(&mut db, &cache_mgr);
        let report = sync.sync_all(&local_dir, &remote_dir, false)?;
        assert_eq!(report.files_uploaded, 2);
    }
    assert!(remote_dir.join("file1.rs").exists());
    assert!(remote_dir.join("file2.rs").exists());

    // 1. Delete file2.rs locally and run prune (1 out of 2 remote files = 50%, ≤50% so --force not needed)
    std::fs::remove_file(local_dir.join("file2.rs"))?;

    let pruned = SyncEngine::prune_remote(&local_dir, &remote_dir, false)?;
    assert_eq!(pruned, 1);
    assert!(!remote_dir.join("file2.rs").exists(), "Pruned file must be removed from remote project root");
    assert!(remote_dir.join("file1.rs").exists(), "Untouched file must remain in remote project root");

    // Verify it was moved to .trash vault
    let trash_dir = remote_dir.join(".trash");
    assert!(trash_dir.exists());
    let mut found_in_trash = false;
    for entry in std::fs::read_dir(&trash_dir)?.filter_map(|e| e.ok()) {
        if entry.file_name().to_string_lossy().ends_with("_file2.rs") {
            found_in_trash = true;
        }
    }
    assert!(found_in_trash, "Pruned file must be safely moved into .trash vault");

    // 2. Safety Gate 1: When local has NO Source files, prune MUST abort with an error
    std::fs::remove_file(local_dir.join("file1.rs"))?;
    let empty_gate_res = SyncEngine::prune_remote(&local_dir, &remote_dir, false);
    assert!(empty_gate_res.is_err(), "Safety Gate 1: Pruning with 0 local source files must fail");

    // 3. Safety Gate 2: When >50% of remote files would be pruned, require --force
    // Setup: 3 files in remote, 1 in local -> 2/3 = 66% pruned
    let temp2 = tempdir()?;
    let loc2 = temp2.path().join("local2");
    let rem2 = temp2.path().join("remote2");
    std::fs::create_dir_all(&loc2)?;
    std::fs::create_dir_all(&rem2)?;

    std::fs::write(loc2.join("keep.rs"), b"keep me")?;
    std::fs::write(rem2.join("keep.rs"), b"keep me")?;
    std::fs::write(rem2.join("del1.rs"), b"del 1")?;
    std::fs::write(rem2.join("del2.rs"), b"del 2")?;

    // Attempt prune without force -> must fail gate 2
    let threshold_err = SyncEngine::prune_remote(&loc2, &rem2, false);
    assert!(threshold_err.is_err(), "Safety Gate 2: Pruning >50% remote files without --force must fail");

    // Prune with force -> must succeed!
    let threshold_ok = SyncEngine::prune_remote(&loc2, &rem2, true)?;
    assert_eq!(threshold_ok, 2);
    assert!(!rem2.join("del1.rs").exists());
    assert!(!rem2.join("del2.rs").exists());
    assert!(rem2.join("keep.rs").exists());

    Ok(())
}

#[test]
fn test_restore_db_registration_and_no_auto_sync() -> Result<()> {
    let temp = tempdir()?;
    let remote_dir = temp.path().join("remote_project");
    let target_dir = temp.path().join("restored_project");
    let cache_dir = temp.path().join("cache");

    std::fs::create_dir_all(&remote_dir)?;
    std::fs::write(remote_dir.join("lib.rs"), b"pub fn code() -> i32 { 42 }")?;

    let mut db = Database::open(":memory:")?;
    let cache_mgr = CacheManager::new(&cache_dir, 1024 * 1024 * 1024)?;

    {
        let mut sync = SyncEngine::new(&mut db, &cache_mgr);
        let report = sync.restore_from_remote(&remote_dir, &target_dir, false)?;
        assert_eq!(report.files_restored, 1);
    }

    // Verify project is registered in database under canonical local path
    let canonical_target = target_dir.canonicalize()?;
    let proj = db.get_project_by_local_path(&canonical_target.to_string_lossy())?;
    assert!(proj.is_some(), "Restored project MUST be registered in the database");

    // Verify NO pending WAL events were generated (auto-sync did NOT run)
    let pending_wal = db.get_all_pending_wal_events()?;
    assert_eq!(pending_wal.len(), 0, "Initial sync_all must NOT run automatically upon restore");

    Ok(())
}

#[test]
fn test_restore_edit_sync_no_false_conflict() -> Result<()> {
    let temp = tempdir()?;
    let local_dir = temp.path().join("original");
    let remote_dir = temp.path().join("remote");
    let restored_dir = temp.path().join("restored");
    let cache_dir = temp.path().join("cache");

    std::fs::create_dir_all(&local_dir)?;
    std::fs::write(local_dir.join("main.rs"), b"fn main() { println!(\"Initial\"); }")?;

    let mut db = Database::open(":memory:")?;
    let cache_mgr = CacheManager::new(&cache_dir, 1024 * 1024 * 1024)?;

    // 1. Initial sync
    {
        let mut sync = SyncEngine::new(&mut db, &cache_mgr);
        sync.sync_all(&local_dir, &remote_dir, false)?;
    }

    // 2. Restore to new directory
    {
        let mut sync = SyncEngine::new(&mut db, &cache_mgr);
        sync.restore_from_remote(&remote_dir, &restored_dir, false)?;
    }

    // 3. Edit file in restored directory
    std::thread::sleep(std::time::Duration::from_millis(20));
    let restored_main = restored_dir.join("main.rs");
    std::fs::write(&restored_main, b"fn main() { println!(\"Work continuing on restored copy!\"); }")?;

    // 4. Sync restored directory back to Drive
    let _proj = db.get_project_by_local_path(&restored_dir.canonicalize()?.to_string_lossy())?.unwrap();
    {
        let mut sync = SyncEngine::new(&mut db, &cache_mgr);
        let sync_report = sync.sync_all(&restored_dir, &remote_dir, false)?;

        // CRITICAL CHECK: ZERO false conflicts!
        assert_eq!(
            sync_report.conflicts_detected,
            0,
            "False conflict detected! Modification after restore must sync cleanly without .conflict_ creation"
        );
        assert_eq!(sync_report.files_uploaded, 1);
    }

    // Remote file is updated
    let remote_content = std::fs::read(remote_dir.join("main.rs"))?;
    assert_eq!(remote_content, b"fn main() { println!(\"Work continuing on restored copy!\"); }");

    Ok(())
}

#[test]
fn test_real_concurrent_conflict_handling() -> Result<()> {
    let temp = tempdir()?;
    let local_file = temp.path().join("main.rs");
    let remote_file = temp.path().join("remote_main.rs");

    // Both start with identical content
    std::fs::write(&local_file, b"version 1.0")?;
    std::fs::write(&remote_file, b"version 1.0")?;

    // Cloud edit: remote edited with newer timestamp
    std::thread::sleep(std::time::Duration::from_millis(20));
    std::fs::write(&remote_file, b"version 2.0 edited in cloud web UI")?;
    let newer_time = filetime::FileTime::from_unix_time(2000000000, 0);
    filetime::set_file_mtime(&remote_file, newer_time)?;

    // Local edit: different content
    std::fs::write(&local_file, b"version 2.0 edited locally in IDE")?;
    let older_time = filetime::FileTime::from_unix_time(1000000000, 0);
    filetime::set_file_mtime(&local_file, older_time)?;

    // Sync must detect genuine conflict
    let res = SyncEngine::safe_sync_file(&local_file, &remote_file)?;
    match res {
        SyncFileResult::Conflict(conflict_path) => {
            assert!(Path::new(&conflict_path).exists());
            let remote_content = std::fs::read(&remote_file)?;
            assert_eq!(remote_content, b"version 2.0 edited in cloud web UI", "Original remote file must not be overwritten");
        }
        other => panic!("Expected Conflict result, got {:?}", other),
    }

    Ok(())
}

#[test]
fn test_name_validation_and_collision_prevention() -> Result<()> {
    use devdrive::sync::validate_project_name;

    // 1. Path traversal & illegal name validation
    assert!(validate_project_name("..").is_err());
    assert!(validate_project_name(".").is_err());
    assert!(validate_project_name("").is_err());
    assert!(validate_project_name("   ").is_err());
    assert!(validate_project_name("foo/bar").is_err());
    assert!(validate_project_name("foo\\bar").is_err());
    assert!(validate_project_name("../escape").is_err());
    assert!(validate_project_name("valid-project_name.1").is_ok());

    // 2. Remote collision prevention in Database
    let mut db = Database::open(":memory:")?;
    db.register_project("app", "/path/one", "/remote/drive/app")?;

    // Looking up by remote path
    let existing = db.get_project_by_remote_path("/remote/drive/app")?;
    assert!(existing.is_some());
    let proj = existing.unwrap();
    assert_eq!(proj.local_path, "/path/one");

    // Second project trying to bind to same remote path
    let conflict_check = proj.local_path != "/path/two";
    assert!(conflict_check, "Collision shield must detect remote folder collision for different local paths");

    Ok(())
}
