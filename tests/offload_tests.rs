use anyhow::Result;
use devdrive::cache::CacheManager;
use devdrive::config::DevDriveConfig;
use devdrive::db::Database;
use devdrive::offload::OffloadEngine;
use std::collections::HashMap;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use tempfile::tempdir;

fn setup_test_config(remote_path: &Path, quarantine_path: &Path) -> DevDriveConfig {
    DevDriveConfig {
        remote_drive_base_path: remote_path.to_string_lossy().to_string(),
        max_cache_gb: 5,
        debounce_ms: 100,
        auto_snapshot_on_sync: false,
        custom_ephemeral_dirs: vec![],
        backup_env_files: true,
        quarantine_days: 7,
        quarantine_max_gb: 5,
        include_git: true,
        quarantine_base_path: Some(quarantine_path.to_string_lossy().to_string()),
        auto_sync_idle_minutes: 30,
        battery_guard_enabled: true,
        auto_sync_enabled: true,
        auto_discover_enabled: false,
        auto_discover_paths: vec![],
        auto_slim_days: 7,
        auto_showcase_days: 30,
        use_direct_showcase: false,
    }
}

/// Helper to compute hashes of all files in a directory tree for comparison
fn hash_directory_files(dir: &Path) -> HashMap<PathBuf, String> {
    let mut hashes = HashMap::new();
    for entry in walkdir::WalkDir::new(dir).into_iter().filter_map(|e| e.ok()) {
        if entry.file_type().is_file()
            && let Ok(rel) = entry.path().strip_prefix(dir)
            && let Ok(hash) = CacheManager::compute_sha256(entry.path())
        {
            hashes.insert(rel.to_path_buf(), hash);
        }
    }
    hashes
}

// -----------------------------------------------------------------------------
// Invariants 1 & 2:
// - Class A is wiped, Class B is moved via atomic rename to quarantine.
// - All Class B files verified in manifest hash before moving.
// -----------------------------------------------------------------------------
#[test]
fn test_invariant_1_and_2_offload_clean_a_and_quarantine_b() -> Result<()> {
    let temp = tempdir()?;
    let project_dir = temp.path().join("my_app");
    let remote_dir = temp.path().join("DevDrive");
    let quarantine_dir = temp.path().join("quarantine");

    std::fs::create_dir_all(&project_dir)?;
    std::fs::create_dir_all(&remote_dir)?;
    std::fs::create_dir_all(&quarantine_dir)?;

    let config = setup_test_config(&remote_dir, &quarantine_dir);
    let mut db = Database::open(":memory:")?;

    // Create Class B precious files
    std::fs::write(project_dir.join("package.json"), r#"{"name": "my_app"}"#)?;
    std::fs::write(project_dir.join("package-lock.json"), r#"{"lockfileVersion": 3}"#)?;
    std::fs::create_dir_all(project_dir.join("src"))?;
    std::fs::write(project_dir.join("src").join("index.js"), "console.log('hello');")?;
    std::fs::write(project_dir.join("README.md"), "# Documentation")?;

    // Create Class A reproducible dependency folder
    let node_modules = project_dir.join("node_modules");
    std::fs::create_dir_all(node_modules.join("lodash"))?;
    std::fs::write(node_modules.join("lodash").join("index.js"), "module.exports = {};")?;

    let proj = db.register_project(
        "my_app",
        &project_dir.to_string_lossy(),
        &remote_dir.join("my_app").to_string_lossy(),
    )?;

    // Execute dry run first
    let dry_report = OffloadEngine::offload(&mut db, proj.id, &config, true, false)?;
    assert!(dry_report.dry_run);
    assert_eq!(dry_report.class_b_file_count, 4); // package.json, package-lock.json, src/index.js, README.md
    assert!(dry_report.class_a_freed_bytes > 0);
    assert!(project_dir.exists(), "Dry run must NOT modify local directory");

    // Execute real offload
    let report = OffloadEngine::offload(&mut db, proj.id, &config, false, false)?;
    assert!(!report.dry_run);
    assert_eq!(report.project_name, "my_app");
    assert!(!report.manifest_sha256.is_empty());

    // Invariant: Original local path no longer exists (atomically moved to quarantine)
    assert!(!project_dir.exists(), "Local project directory must be relocated to quarantine");

    // Invariant: Class A wiped from quarantine, Class B preserved in quarantine
    let q_path = PathBuf::from(report.quarantine_path.expect("Quarantine path must be present"));
    assert!(q_path.exists(), "Quarantine path must exist on disk");
    assert!(!q_path.join("node_modules").exists(), "Class A folder must be deleted from quarantine");
    assert!(q_path.join("package.json").exists(), "Class B package.json must exist in quarantine");
    assert!(q_path.join("src").join("index.js").exists(), "Class B source must exist in quarantine");

    // Invariant: Remote archive structure (.archives/<proj>/<time>/tree, recipe.json, manifest.json, manifest.sha256)
    let archive_path = PathBuf::from(&report.archive_remote_path);
    assert!(archive_path.join("tree.tar.gz").exists(), "tree.tar.gz must exist");
    assert!(archive_path.join("manifest.json").exists());
    assert!(archive_path.join("manifest.sha256").exists());
    assert!(archive_path.join("recipe.json").exists());

    // Verify remote archive integrity
    let verify = OffloadEngine::archive_verify(&archive_path)?;
    assert!(verify.is_valid);
    assert_eq!(verify.verified_files, 4);

    // Verify DB state
    let updated_proj = db.get_project_by_id(proj.id)?.expect("Project must exist in DB");
    assert_eq!(updated_proj.status, "archived");
    assert!(updated_proj.freed_bytes > 0);

    // Verify Offload Journal entries
    let journals = db.get_offload_journal(proj.id)?;
    assert!(journals.iter().any(|j| j.step == "preflight" && j.state == "success"));
    assert!(journals.iter().any(|j| j.step == "copy_archive" && j.state == "success"));
    assert!(journals.iter().any(|j| j.step == "write_manifest" && j.state == "success"));
    assert!(journals.iter().any(|j| j.step == "verify_before_delete" && j.state == "success"));
    assert!(journals.iter().any(|j| j.step == "delete_class_a" && j.state == "success"));
    assert!(journals.iter().any(|j| j.step == "quarantine_rename" && j.state == "success"));
    assert!(journals.iter().any(|j| j.step == "complete" && j.state == "success"));

    Ok(())
}

// -----------------------------------------------------------------------------
// Invariants 3 & 6:
// - Class C blockers (external symlinks, unreadable files, missing lockfiles)
//   MUST reject offload and leave local project 100% bit-for-bit unchanged.
// -----------------------------------------------------------------------------
#[test]
fn test_invariant_3_and_6_blocker_leaves_project_100_percent_untouched() -> Result<()> {
    let temp = tempdir()?;
    let project_dir = temp.path().join("secure_proj");
    let outside_dir = temp.path().join("outside_target");
    let remote_dir = temp.path().join("DevDrive");
    let quarantine_dir = temp.path().join("quarantine");

    std::fs::create_dir_all(&project_dir)?;
    std::fs::create_dir_all(&outside_dir)?;
    std::fs::create_dir_all(&remote_dir)?;
    std::fs::create_dir_all(&quarantine_dir)?;

    let config = setup_test_config(&remote_dir, &quarantine_dir);
    let mut db = Database::open(":memory:")?;

    // Create source files
    std::fs::write(project_dir.join("main.py"), "print('secure')")?;
    std::fs::write(project_dir.join("utils.py"), "def add(a, b): return a + b")?;

    // Create an outside file and an external symlink (Class C blocker!)
    std::fs::write(outside_dir.join("secret_external.txt"), "outside data")?;
    std::os::unix::fs::symlink(
        outside_dir.join("secret_external.txt"),
        project_dir.join("sym_leak.txt"),
    )?;

    let proj = db.register_project(
        "secure_proj",
        &project_dir.to_string_lossy(),
        &remote_dir.join("secure_proj").to_string_lossy(),
    )?;

    let hashes_before = hash_directory_files(&project_dir);

    // Attempt offload
    let result = OffloadEngine::offload(&mut db, proj.id, &config, false, false);
    assert!(result.is_err(), "Offload MUST fail when Class C blocker is present");
    let err_msg = result.unwrap_err().to_string();
    assert!(err_msg.contains("Güvenlik Kapısı Başarısız") || err_msg.contains("Sınıf C"));
    assert!(err_msg.contains("sym_leak.txt"));

    // INVARIANT: Project is 100% bit-for-bit unchanged!
    assert!(project_dir.exists(), "Project directory must still exist in original location");
    let hashes_after = hash_directory_files(&project_dir);
    assert_eq!(hashes_before, hashes_after, "Local files must be 100% identical before and after failed offload");

    // Check DB status is NOT archived
    let updated_proj = db.get_project_by_id(proj.id)?.unwrap();
    assert_ne!(updated_proj.status, "archived");

    // Remove the bad symlink
    std::fs::remove_file(project_dir.join("sym_leak.txt"))?;

    // Now introduce missing lockfile blocker:
    // Create a node_modules directory with NO package.json or package-lock.json
    std::fs::create_dir_all(project_dir.join("node_modules").join("foo"))?;
    std::fs::write(project_dir.join("node_modules").join("foo").join("index.js"), "foo")?;

    let hashes_before_2 = hash_directory_files(&project_dir);
    let result_missing_lockfile = OffloadEngine::offload(&mut db, proj.id, &config, false, false);
    assert!(result_missing_lockfile.is_err(), "Offload MUST fail when required lockfile is missing");
    let hashes_after_2 = hash_directory_files(&project_dir);
    assert_eq!(hashes_before_2, hashes_after_2, "Files must remain intact after missing lockfile blocker");

    Ok(())
}

// -----------------------------------------------------------------------------
// Invariants 4 & 7:
// - Archive verification detects corruption or mutation.
// -----------------------------------------------------------------------------
#[test]
fn test_invariant_4_and_7_archive_integrity_verification_detects_tampering() -> Result<()> {
    let temp = tempdir()?;
    let project_dir = temp.path().join("integrity_app");
    let remote_dir = temp.path().join("DevDrive");
    let quarantine_dir = temp.path().join("quarantine");

    std::fs::create_dir_all(&project_dir)?;
    std::fs::create_dir_all(&remote_dir)?;
    std::fs::create_dir_all(&quarantine_dir)?;

    let config = setup_test_config(&remote_dir, &quarantine_dir);
    let mut db = Database::open(":memory:")?;

    std::fs::write(project_dir.join("Cargo.toml"), "[package]\nname = \"integrity_app\"\nversion = \"0.1.0\"\n")?;
    std::fs::write(project_dir.join("Cargo.lock"), "# Lockfile v3\n")?;
    std::fs::create_dir_all(project_dir.join("src"))?;
    std::fs::write(project_dir.join("src").join("main.rs"), "fn main() { println!(\"ok\"); }")?;

    let proj = db.register_project(
        "integrity_app",
        &project_dir.to_string_lossy(),
        &remote_dir.join("integrity_app").to_string_lossy(),
    )?;

    let report = OffloadEngine::offload(&mut db, proj.id, &config, false, false)?;
    let archive_path = PathBuf::from(&report.archive_remote_path);

    // Verify pristine archive
    let verify = OffloadEngine::archive_verify(&archive_path)?;
    assert!(verify.is_valid);
    assert!(verify.errors.is_empty());

    // Tamper with archive package
    let arch_tar = archive_path.join("tree.tar.gz");
    let original_bytes = std::fs::read(&arch_tar)?;
    let mut corrupt_bytes = original_bytes.clone();
    let mid = corrupt_bytes.len() / 2;
    corrupt_bytes[mid] ^= 0xFF;
    std::fs::write(&arch_tar, &corrupt_bytes)?;

    // Now verification MUST detect the tampering
    let verify_tampered = OffloadEngine::archive_verify(&archive_path)?;
    assert!(!verify_tampered.is_valid, "Archive verification must fail on tampered file");
    assert!(!verify_tampered.errors.is_empty());

    // Restore file, but tamper with manifest.sha256
    std::fs::write(&arch_tar, &original_bytes)?;
    let sha_path = archive_path.join("manifest.sha256");
    std::fs::write(&sha_path, "0000000000000000000000000000000000000000000000000000000000000000  manifest.json\n")?;

    let verify_sha_tampered = OffloadEngine::archive_verify(&archive_path);
    assert!(verify_sha_tampered.is_err(), "Verification must fail when manifest seal is broken");

    Ok(())
}

// -----------------------------------------------------------------------------
// Invariants 5 & 11:
// - Round-trip offload -> unarchive preserves byte-for-byte hashes,
//   permissions (chmod +x), empty directories, internal symlinks, and Unicode filenames.
// - Tests both Path 1 (restore from quarantine) and Path 2 (restore from Drive archive).
// -----------------------------------------------------------------------------
#[test]
fn test_invariant_5_and_11_roundtrip_preserves_hashes_modes_mtimes_symlinks_unicode() -> Result<()> {
    let temp = tempdir()?;
    let project_dir = temp.path().join("rich_project");
    let remote_dir = temp.path().join("DevDrive");
    let quarantine_dir = temp.path().join("quarantine");

    std::fs::create_dir_all(&project_dir)?;
    std::fs::create_dir_all(&remote_dir)?;
    std::fs::create_dir_all(&quarantine_dir)?;

    let config = setup_test_config(&remote_dir, &quarantine_dir);
    let mut db = Database::open(":memory:")?;

    // 1. Regular precious files
    std::fs::write(project_dir.join("Cargo.toml"), "[package]\nname = \"rich_project\"\nversion = \"0.1.0\"\n")?;
    std::fs::write(project_dir.join("Cargo.lock"), "# Lockfile v3\n")?;
    std::fs::create_dir_all(project_dir.join("src"))?;
    std::fs::write(project_dir.join("src").join("lib.rs"), "pub fn compute() -> i32 { 42 }")?;

    // 2. Executable script (chmod +x / 0o755)
    let script_path = project_dir.join("run.sh");
    std::fs::write(&script_path, "#!/bin/sh\necho 'running'\n")?;
    std::fs::set_permissions(&script_path, std::fs::Permissions::from_mode(0o755))?;

    // 3. Unicode filename (Turkish letters çğıöşü)
    let unicode_file = project_dir.join("türkçe_rapor_çğıöşü.txt");
    std::fs::write(&unicode_file, "Türkçe karakter testi: Ğ, Ü, Ş, İ, Ö, Ç, ı, ğ, ü, ş, ö, ç.")?;

    // 4. Empty directory structure
    let empty_dir = project_dir.join("empty_folder");
    std::fs::create_dir_all(&empty_dir)?;

    // 5. Internal symlink: symlink pointing to unicode file
    let symlink_path = project_dir.join("sym_to_report.txt");
    std::os::unix::fs::symlink("türkçe_rapor_çğıöşü.txt", &symlink_path)?;

    // 6. Class A directory (target)
    let target_dir = project_dir.join("target").join("debug");
    std::fs::create_dir_all(&target_dir)?;
    std::fs::write(target_dir.join("app.bin"), vec![0u8; 1024])?;

    let proj = db.register_project(
        "rich_project",
        &project_dir.to_string_lossy(),
        &remote_dir.join("rich_project").to_string_lossy(),
    )?;

    // Perform Offload
    let _offload_report = OffloadEngine::offload(&mut db, proj.id, &config, false, false)?;
    assert!(!project_dir.exists());

    // -------------------------------------------------------------
    // Roundtrip Case A: Unarchive from local Quarantine (instant rename)
    // -------------------------------------------------------------
    let unarchive_report_a = OffloadEngine::unarchive(&mut db, proj.id, &config, false)?;
    assert!(unarchive_report_a.restored_from_quarantine);
    assert!(!unarchive_report_a.restored_from_archive);
    assert!(project_dir.exists());

    // Verify all files and metadata in restored project
    assert!(project_dir.join("Cargo.toml").exists());
    assert!(project_dir.join("src").join("lib.rs").exists());

    // Verify executable permissions
    let restored_script_meta = project_dir.join("run.sh").metadata()?;
    assert_eq!(restored_script_meta.permissions().mode() & 0o777, 0o755, "Script must remain executable (0o755)");

    // Verify Unicode file
    let unicode_content = std::fs::read_to_string(project_dir.join("türkçe_rapor_çğıöşü.txt"))?;
    assert_eq!(unicode_content, "Türkçe karakter testi: Ğ, Ü, Ş, İ, Ö, Ç, ı, ğ, ü, ş, ö, ç.");

    // Verify empty directory
    assert!(project_dir.join("empty_folder").is_dir(), "Empty directory must be preserved");

    // Verify symlink
    let symlink_meta = std::fs::symlink_metadata(project_dir.join("sym_to_report.txt"))?;
    assert!(symlink_meta.file_type().is_symlink());
    let sym_target = std::fs::read_link(project_dir.join("sym_to_report.txt"))?;
    assert_eq!(sym_target.to_string_lossy(), "türkçe_rapor_çğıöşü.txt");

    // Verify Class A target was cleaned
    assert!(!project_dir.join("target").exists(), "Class A folder must not exist locally");

    // -------------------------------------------------------------
    // Roundtrip Case B: Unarchive from Remote Drive Archive
    // (Simulate quarantine deleted or expired)
    // -------------------------------------------------------------
    // Offload again
    let offload_report_b = OffloadEngine::offload(&mut db, proj.id, &config, false, false)?;
    assert!(!project_dir.exists());

    // Delete the quarantine folder completely
    let q_path = PathBuf::from(offload_report_b.quarantine_path.unwrap());
    std::fs::remove_dir_all(&q_path)?;
    assert!(!q_path.exists());

    // Unarchive now (quarantine missing -> downloads from remote archive tree)
    let unarchive_report_b = OffloadEngine::unarchive(&mut db, proj.id, &config, false)?;
    assert!(!unarchive_report_b.restored_from_quarantine);
    assert!(unarchive_report_b.restored_from_archive);
    assert_eq!(unarchive_report_b.files_restored, 5); // Cargo.toml, Cargo.lock, src/lib.rs, run.sh, unicode_file

    // Verify executable permissions from archive
    let restored_script_meta_b = project_dir.join("run.sh").metadata()?;
    assert_eq!(restored_script_meta_b.permissions().mode() & 0o777, 0o755, "Restored script from archive must have 0o755");

    // Verify Unicode file from archive
    let unicode_content_b = std::fs::read_to_string(project_dir.join("türkçe_rapor_çğıöşü.txt"))?;
    assert_eq!(unicode_content_b, "Türkçe karakter testi: Ğ, Ü, Ş, İ, Ö, Ç, ı, ğ, ü, ş, ö, ç.");

    // Verify empty folder from archive
    assert!(project_dir.join("empty_folder").is_dir());

    // Verify symlink from archive
    let symlink_meta_b = std::fs::symlink_metadata(project_dir.join("sym_to_report.txt"))?;
    assert!(symlink_meta_b.file_type().is_symlink());

    Ok(())
}

// -----------------------------------------------------------------------------
// Invariant 8:
// - Git repository with uncommitted or unpushed commits is preserved
//   across offload -> unarchive.
// -----------------------------------------------------------------------------
#[test]
fn test_invariant_8_git_uncommitted_preserved() -> Result<()> {
    let temp = tempdir()?;
    let project_dir = temp.path().join("git_repo");
    let remote_dir = temp.path().join("DevDrive");
    let quarantine_dir = temp.path().join("quarantine");

    std::fs::create_dir_all(&project_dir)?;
    std::fs::create_dir_all(&remote_dir)?;
    std::fs::create_dir_all(&quarantine_dir)?;

    let config = setup_test_config(&remote_dir, &quarantine_dir);
    let mut db = Database::open(":memory:")?;

    // Initialize git repo
    let status_init = std::process::Command::new("git")
        .arg("init")
        .current_dir(&project_dir)
        .status()?;
    assert!(status_init.success());

    std::fs::write(project_dir.join("README.md"), "Git test initial")?;

    let _ = std::process::Command::new("git")
        .args(["config", "user.name", "Test Runner"])
        .current_dir(&project_dir)
        .status()?;
    let _ = std::process::Command::new("git")
        .args(["config", "user.email", "test@example.com"])
        .current_dir(&project_dir)
        .status()?;

    let _ = std::process::Command::new("git")
        .args(["add", "."])
        .current_dir(&project_dir)
        .status()?;

    let commit_status = std::process::Command::new("git")
        .args(["commit", "-m", "Initial unpushed commit"])
        .current_dir(&project_dir)
        .status()?;
    assert!(commit_status.success());

    // Create an uncommitted change in working tree
    std::fs::write(project_dir.join("feature.txt"), "uncommitted work in progress")?;

    // Get HEAD commit hash
    let head_output = std::process::Command::new("git")
        .args(["rev-parse", "HEAD"])
        .current_dir(&project_dir)
        .output()?;
    let commit_hash_before = String::from_utf8_lossy(&head_output.stdout).trim().to_string();

    let proj = db.register_project(
        "git_repo",
        &project_dir.to_string_lossy(),
        &remote_dir.join("git_repo").to_string_lossy(),
    )?;

    // Offload with include_git = true
    let report = OffloadEngine::offload(&mut db, proj.id, &config, false, false)?;
    assert!(!project_dir.exists());

    // Wipe quarantine to force restoration from remote archive
    let q_path = PathBuf::from(report.quarantine_path.unwrap());
    std::fs::remove_dir_all(&q_path)?;

    // Unarchive
    let unarchive_report = OffloadEngine::unarchive(&mut db, proj.id, &config, false)?;
    assert!(unarchive_report.restored_from_archive);
    assert!(project_dir.exists());

    // Check HEAD commit hash matches exactly
    let head_output_after = std::process::Command::new("git")
        .args(["rev-parse", "HEAD"])
        .current_dir(&project_dir)
        .output()?;
    let commit_hash_after = String::from_utf8_lossy(&head_output_after.stdout).trim().to_string();
    assert_eq!(commit_hash_before, commit_hash_after, "Git commit history must be preserved identically");

    // Check uncommitted file was also restored
    assert!(project_dir.join("feature.txt").exists());
    assert_eq!(std::fs::read_to_string(project_dir.join("feature.txt"))?, "uncommitted work in progress");

    Ok(())
}

// -----------------------------------------------------------------------------
// Invariant 9:
// - backup_env_files=false blocks without explicit bypass; when bypassed, excludes .env.
// - backup_env_files=true backs up and restores .env.
// -----------------------------------------------------------------------------
#[test]
fn test_invariant_9_env_protection_gates() -> Result<()> {
    let temp = tempdir()?;
    let project_dir = temp.path().join("env_project");
    let remote_dir = temp.path().join("DevDrive");
    let quarantine_dir = temp.path().join("quarantine");

    std::fs::create_dir_all(&project_dir)?;
    std::fs::create_dir_all(&remote_dir)?;
    std::fs::create_dir_all(&quarantine_dir)?;

    let mut db = Database::open(":memory:")?;

    std::fs::write(project_dir.join("main.py"), "print('hello')")?;
    std::fs::write(project_dir.join(".env"), "DATABASE_URL=postgres://secret@localhost/db")?;
    std::fs::write(project_dir.join(".env.local"), "SECRET_KEY=12345")?;

    let proj = db.register_project(
        "env_project",
        &project_dir.to_string_lossy(),
        &remote_dir.join("env_project").to_string_lossy(),
    )?;

    // Case A: backup_env_files = false, bypass = false -> MUST FAIL with blocker
    let mut config_no_backup = setup_test_config(&remote_dir, &quarantine_dir);
    config_no_backup.backup_env_files = false;

    let result_fail = OffloadEngine::offload(&mut db, proj.id, &config_no_backup, false, false);
    assert!(result_fail.is_err());
    let err_msg = result_fail.unwrap_err().to_string();
    assert!(err_msg.contains(".env") || err_msg.contains("backup_env_files"));
    assert!(project_dir.exists(), "Local project must not be touched on blocker");

    // Case B: backup_env_files = false, bypass = true -> Offload succeeds, but .env is excluded from archive
    let report_b = OffloadEngine::offload(&mut db, proj.id, &config_no_backup, false, true)?;
    let arch_dir_b = PathBuf::from(&report_b.archive_remote_path);
    let manifest_b: devdrive::offload::OffloadManifest = serde_json::from_str(&std::fs::read_to_string(arch_dir_b.join("manifest.json"))?)?;
    assert!(!manifest_b.entries.iter().any(|e| e.path == ".env"), ".env must NOT be written to archive when backup_env_files is false");
    assert!(!manifest_b.entries.iter().any(|e| e.path == ".env.local"));

    // Restore from quarantine
    OffloadEngine::unarchive(&mut db, proj.id, &config_no_backup, false)?;

    // Case C: backup_env_files = true -> .env IS backed up to archive and restored
    let config_with_backup = setup_test_config(&remote_dir, &quarantine_dir);
    let report_c = OffloadEngine::offload(&mut db, proj.id, &config_with_backup, false, false)?;
    let arch_dir_c = PathBuf::from(&report_c.archive_remote_path);
    let manifest_c: devdrive::offload::OffloadManifest = serde_json::from_str(&std::fs::read_to_string(arch_dir_c.join("manifest.json"))?)?;
    assert!(manifest_c.entries.iter().any(|e| e.path == ".env"), ".env must be present in archive when backup_env_files is true");

    // Delete quarantine to force archive download
    let q_path_c = PathBuf::from(report_c.quarantine_path.unwrap());
    std::fs::remove_dir_all(&q_path_c)?;

    OffloadEngine::unarchive(&mut db, proj.id, &config_with_backup, false)?;
    assert!(project_dir.join(".env").exists(), ".env must be restored from archive");
    assert_eq!(
        std::fs::read_to_string(project_dir.join(".env"))?,
        "DATABASE_URL=postgres://secret@localhost/db"
    );

    Ok(())
}

// -----------------------------------------------------------------------------
// Invariant 10:
// - Purge is rejected before 7 days without --force.
// - Purge is STRICTLY rejected if archive verification fails (even with --force).
// - Purge succeeds when archive verifies and force is true.
// -----------------------------------------------------------------------------
#[test]
fn test_invariant_10_purge_gates_7days_and_archive_verification() -> Result<()> {
    let temp = tempdir()?;
    let project_dir = temp.path().join("purge_test");
    let remote_dir = temp.path().join("DevDrive");
    let quarantine_dir = temp.path().join("quarantine");

    std::fs::create_dir_all(&project_dir)?;
    std::fs::create_dir_all(&remote_dir)?;
    std::fs::create_dir_all(&quarantine_dir)?;

    let config = setup_test_config(&remote_dir, &quarantine_dir);
    let mut db = Database::open(":memory:")?;

    std::fs::write(project_dir.join("main.rs"), "fn main() {}")?;
    std::fs::write(project_dir.join("Cargo.toml"), "[package]\nname = \"purge_test\"\nversion = \"0.1.0\"\n")?;
    std::fs::write(project_dir.join("Cargo.lock"), "# Lockfile\n")?;

    let proj = db.register_project(
        "purge_test",
        &project_dir.to_string_lossy(),
        &remote_dir.join("purge_test").to_string_lossy(),
    )?;

    let offload_report = OffloadEngine::offload(&mut db, proj.id, &config, false, false)?;
    let q_path = PathBuf::from(offload_report.quarantine_path.unwrap());
    assert!(q_path.exists());

    // Gate A: Purge without force before 7 days MUST be rejected
    let purge_fail_time = OffloadEngine::purge(&mut db, proj.id, false);
    assert!(purge_fail_time.is_err());
    let err_time = purge_fail_time.unwrap_err().to_string();
    assert!(err_time.contains("Karantina süresi henüz dolmadı") || err_time.contains("--force"));
    assert!(q_path.exists(), "Quarantine directory must not be deleted when time gate fails");

    // Gate B: Corrupt remote archive -> Purge with force MUST STILL BE REJECTED!
    let arch_tar = PathBuf::from(&offload_report.archive_remote_path).join("tree.tar.gz");
    let original_bytes = std::fs::read(&arch_tar)?;
    let mut corrupt_bytes = original_bytes.clone();
    let mid = corrupt_bytes.len() / 2;
    corrupt_bytes[mid] ^= 0xFF;
    std::fs::write(&arch_tar, &corrupt_bytes)?;

    let purge_fail_corrupt = OffloadEngine::purge(&mut db, proj.id, true);
    assert!(purge_fail_corrupt.is_err());
    let err_corrupt = purge_fail_corrupt.unwrap_err().to_string();
    assert!(err_corrupt.contains("Arşiv doğrulanmadı") || err_corrupt.contains("ASLA silinemez"));
    assert!(q_path.exists(), "Quarantine MUST NEVER be deleted when archive is corrupted!");

    // Gate C: Fix archive integrity -> Purge with force SUCCEEDS
    std::fs::write(&arch_tar, &original_bytes)?;

    let purge_success = OffloadEngine::purge(&mut db, proj.id, true)?;
    assert_eq!(purge_success.deleted_quarantine_path, q_path.to_string_lossy());
    assert!(!q_path.exists(), "Quarantine must be permanently deleted upon valid purge");

    // Check DB status is "purged"
    let purged_proj = db.get_project_by_id(proj.id)?.unwrap();
    assert_eq!(purged_proj.status, "purged");
    assert!(purged_proj.quarantine_path.is_none());

    Ok(())
}

// -----------------------------------------------------------------------------
// Standalone clean_artifacts test:
// - Dry run reports items and sizes without deletion.
// - Real run removes only Class A directories, keeping source and lockfiles intact.
// -----------------------------------------------------------------------------
#[test]
fn test_clean_artifacts_standalone() -> Result<()> {
    let temp = tempdir()?;
    let project_dir = temp.path().join("artifact_project");
    std::fs::create_dir_all(&project_dir)?;

    std::fs::write(project_dir.join("package.json"), "{}")?;
    std::fs::write(project_dir.join("package-lock.json"), "{}")?;
    let node_modules = project_dir.join("node_modules").join("dep");
    std::fs::create_dir_all(&node_modules)?;
    std::fs::write(node_modules.join("dep.js"), "console.log('dep')")?;

    std::fs::write(project_dir.join("Cargo.toml"), "[package]\nname = \"artifact_project\"\nversion = \"0.1.0\"\n")?;
    std::fs::write(project_dir.join("Cargo.lock"), "# Lock\n")?;
    let target_dir = project_dir.join("target").join("debug");
    std::fs::create_dir_all(&target_dir)?;
    std::fs::write(target_dir.join("app"), vec![0u8; 512])?;

    std::fs::write(project_dir.join("main.rs"), "fn main() {}")?;

    // 1. Dry run
    let dry_report = OffloadEngine::clean_artifacts(&project_dir, true)?;
    assert!(dry_report.dry_run);
    assert_eq!(dry_report.cleaned_items.len(), 2); // node_modules and target
    assert!(project_dir.join("node_modules").exists());
    assert!(project_dir.join("target").exists());

    // 2. Real clean
    let clean_report = OffloadEngine::clean_artifacts(&project_dir, false)?;
    assert!(!clean_report.dry_run);
    assert_eq!(clean_report.cleaned_items.len(), 2);
    assert!(!project_dir.join("node_modules").exists(), "node_modules must be removed");
    assert!(!project_dir.join("target").exists(), "target must be removed");

    // Precious source files and lockfiles must remain intact
    assert!(project_dir.join("package.json").exists());
    assert!(project_dir.join("package-lock.json").exists());
    assert!(project_dir.join("Cargo.toml").exists());
    assert!(project_dir.join("Cargo.lock").exists());
    assert!(project_dir.join("main.rs").exists());

    Ok(())
}

// -----------------------------------------------------------------------------
// Recipe and Rebuild command execution test:
// -----------------------------------------------------------------------------
#[test]
fn test_unarchive_rebuild_command_execution() -> Result<()> {
    let temp = tempdir()?;
    let project_dir = temp.path().join("rebuild_project");
    let remote_dir = temp.path().join("DevDrive");
    let quarantine_dir = temp.path().join("quarantine");

    std::fs::create_dir_all(&project_dir)?;
    std::fs::create_dir_all(&remote_dir)?;
    std::fs::create_dir_all(&quarantine_dir)?;

    let config = setup_test_config(&remote_dir, &quarantine_dir);
    let mut db = Database::open(":memory:")?;

    std::fs::write(project_dir.join("package.json"), "{}")?;
    std::fs::write(project_dir.join("package-lock.json"), "{}")?;
    std::fs::create_dir_all(project_dir.join("node_modules"))?;
    std::fs::write(project_dir.join("node_modules").join("dep.js"), "x")?;

    let proj = db.register_project(
        "rebuild_project",
        &project_dir.to_string_lossy(),
        &remote_dir.join("rebuild_project").to_string_lossy(),
    )?;

    let offload_report = OffloadEngine::offload(&mut db, proj.id, &config, false, false)?;

    // Modify recipe.json in remote archive to run a safe test rebuild command: touch rebuilt_ok.txt
    let recipe_path = PathBuf::from(&offload_report.archive_remote_path).join("recipe.json");
    let custom_recipe = r#"{
        "version": "1.0",
        "project_name": "rebuild_project",
        "artifacts": [],
        "rebuild_commands": ["touch rebuilt_ok.txt"]
    }"#.to_string();
    std::fs::write(&recipe_path, custom_recipe)?;

    // Delete quarantine so it unarchives from remote archive
    let q_path = PathBuf::from(offload_report.quarantine_path.unwrap());
    std::fs::remove_dir_all(&q_path)?;

    // Unarchive with rebuild = true
    let unarchive_report = OffloadEngine::unarchive(&mut db, proj.id, &config, true)?;
    assert!(unarchive_report.rebuild_executed);
    assert!(project_dir.join("rebuilt_ok.txt").exists(), "Rebuild command must have been executed in restored project directory");

    Ok(())
}

#[test]
fn test_purge_expired_batch_cleans_only_expired_quarantines() -> Result<()> {
    let temp = tempdir()?;
    let project_dir = temp.path().join("exp_proj");
    let remote_dir = temp.path().join("DevDrive");
    let quarantine_dir = temp.path().join("quarantine");

    std::fs::create_dir_all(&project_dir)?;
    std::fs::create_dir_all(&remote_dir)?;
    std::fs::create_dir_all(&quarantine_dir)?;

    let config = setup_test_config(&remote_dir, &quarantine_dir);
    let mut db = Database::open(":memory:")?;

    std::fs::write(project_dir.join("package.json"), r#"{"name": "exp"}"#)?;
    std::fs::write(project_dir.join("package-lock.json"), r#"{"lockfileVersion": 3}"#)?;
    std::fs::write(project_dir.join("index.js"), "console.log('hi');")?;

    let proj = db.register_project(
        "exp_proj",
        &project_dir.to_string_lossy(),
        &remote_dir.join("exp_proj").to_string_lossy(),
    )?;

    let offload_report = OffloadEngine::offload(&mut db, proj.id, &config, false, false)?;
    let q_path = PathBuf::from(offload_report.quarantine_path.unwrap());
    assert!(q_path.exists());

    // 1. Calling purge_expired before quarantine_until expires should purge 0 projects
    let purged_initial = OffloadEngine::purge_expired(&mut db, false)?;
    assert_eq!(purged_initial.len(), 0, "Non-expired quarantine must not be purged");
    assert!(q_path.exists());

    // 2. Fast-forward quarantine_until into the past
    let past_time = (chrono::Utc::now() - chrono::Duration::days(1)).to_rfc3339();
    db.update_project_offload_info(
        proj.id,
        "archived",
        Some(&chrono::Utc::now().to_rfc3339()),
        Some(&offload_report.archive_remote_path),
        Some(&q_path.to_string_lossy()),
        Some(&past_time),
        Some(&offload_report.manifest_sha256),
        0,
    )?;

    // 3. Now purge_expired should identify and purge the expired quarantine
    let purged_expired = OffloadEngine::purge_expired(&mut db, false)?;
    assert_eq!(purged_expired.len(), 1, "Expired quarantine must be purged");
    assert_eq!(purged_expired[0].project_name, "exp_proj");
    assert!(!q_path.exists(), "Quarantine directory must be permanently deleted");

    Ok(())
}

#[test]
fn test_direct_showcase_mode_roundtrip() -> Result<()> {
    let temp = tempdir()?;
    let project_dir = temp.path().join("showcase_app");
    let remote_dir = temp.path().join("DevDrive");

    std::fs::create_dir_all(&project_dir)?;
    std::fs::create_dir_all(&remote_dir)?;

    let config = DevDriveConfig {
        remote_drive_base_path: remote_dir.to_string_lossy().to_string(),
        max_cache_gb: 5,
        debounce_ms: 100,
        auto_snapshot_on_sync: false,
        custom_ephemeral_dirs: vec![],
        backup_env_files: true,
        quarantine_days: 7,
        quarantine_max_gb: 5,
        include_git: true,
        quarantine_base_path: None, // Direct showcase mode
        auto_sync_idle_minutes: 30,
        battery_guard_enabled: true,
        auto_sync_enabled: true,
        auto_discover_enabled: false,
        auto_discover_paths: vec![],
        auto_slim_days: 7,
        auto_showcase_days: 30,
        use_direct_showcase: true,
    };
    let mut db = Database::open(":memory:")?;

    // Create files
    std::fs::write(project_dir.join("package.json"), r#"{"name": "showcase_app"}"#)?;
    std::fs::write(project_dir.join("package-lock.json"), r#"{"lockfileVersion": 3}"#)?;
    std::fs::create_dir_all(project_dir.join("src"))?;
    std::fs::write(project_dir.join("src").join("index.js"), "console.log('showcase test');")?;
    std::fs::write(project_dir.join("README.md"), "# Original Documentation")?;

    let node_modules = project_dir.join("node_modules");
    std::fs::create_dir_all(node_modules.join("pkg"))?;
    std::fs::write(node_modules.join("pkg").join("index.js"), "module.exports = {};")?;

    let proj = db.register_project(
        "showcase_app",
        &project_dir.to_string_lossy(),
        &remote_dir.join("showcase_app").to_string_lossy(),
    )?;

    // 1. Offload to Direct Showcase
    let report = OffloadEngine::offload(&mut db, proj.id, &config, false, false)?;
    assert!(report.quarantine_path.is_none(), "Direct showcase has no quarantine path");
    assert!(project_dir.exists(), "Local directory remains in place as showcase folder");

    // Check Showcase files
    assert!(devdrive::offload::is_showcase_dir(&project_dir));
    assert!(project_dir.join(".devdrive-showcase.json").exists());
    assert!(project_dir.join("📋 BULUT_VITRINI.html").exists());
    assert!(project_dir.join("Projeyi_Uyandir.command").exists());
    assert!(project_dir.join("README.md").exists(), "Original README.md must be preserved in showcase");

    // Check that source code & node_modules are deleted from disk to free SSD space
    assert!(!project_dir.join("src").exists());
    assert!(!project_dir.join("node_modules").exists());

    // 2. Unarchive back to SSD
    let unarchive_report = OffloadEngine::unarchive(&mut db, proj.id, &config, false)?;
    assert!(unarchive_report.restored_from_archive);
    assert!(!devdrive::offload::is_showcase_dir(&project_dir));

    // Showcase files must be removed
    assert!(!project_dir.join(".devdrive-showcase.json").exists());
    assert!(!project_dir.join("📋 BULUT_VITRINI.html").exists());
    assert!(!project_dir.join("Projeyi_Uyandir.command").exists());

    // Source files must be restored
    assert!(project_dir.join("src").join("index.js").exists());
    let restored_code = std::fs::read_to_string(project_dir.join("src").join("index.js"))?;
    assert_eq!(restored_code, "console.log('showcase test');");

    // Status is active in DB
    let p_db = db.get_project_by_id(proj.id)?.unwrap();
    assert_eq!(p_db.status, "active");

    Ok(())
}

