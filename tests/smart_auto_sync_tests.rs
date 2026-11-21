use anyhow::Result;
use devdrive::cache::CacheManager;
use devdrive::db::Database;
use devdrive::filter::DevFilter;
use devdrive::sync::SyncEngine;
use devdrive::watcher::{
    is_battery_low_for_sync, parse_battery_low_for_sync, process_event_path, set_background_qos,
    IdleDebouncer,
};
use std::fs;
use std::path::Path;
use std::time::Duration;
use tempfile::tempdir;

#[test]
fn test_battery_guard_ac_power_never_blocks() {
    // When connected to AC power, sync must never be blocked even if battery level is 5% or 10%
    let ac_low = "Now drawing from 'AC Power'\n -InternalBattery-0 (id=23003235)\t10%; charging; 3:37 remaining present: true";
    assert!(!parse_battery_low_for_sync(ac_low));

    let ac_crit = "Now drawing from 'AC Power'\n -InternalBattery-0 (id=23003235)\t2%; charging; 4:10 remaining present: true";
    assert!(!parse_battery_low_for_sync(ac_crit));

    let ac_full = "Now drawing from 'AC Power'\n -InternalBattery-0 (id=23003235)\t100%; charged; 0:00 remaining present: true";
    assert!(!parse_battery_low_for_sync(ac_full));
}

#[test]
fn test_battery_guard_battery_thresholds() {
    // Discharging on battery power below 20% MUST trigger guard
    let batt_19 = "Now drawing from 'Battery Power'\n -InternalBattery-0 (id=23003235)\t19%; discharging; 1:45 remaining present: true";
    assert!(parse_battery_low_for_sync(batt_19));

    let batt_15 = "Now drawing from 'Battery Power'\n -InternalBattery-0 (id=23003235)\t15%; discharging; 1:12 remaining present: true";
    assert!(parse_battery_low_for_sync(batt_15));

    let batt_5 = "Now drawing from 'Battery Power'\n -InternalBattery-0\t5%; discharging;";
    assert!(parse_battery_low_for_sync(batt_5));

    let batt_0 = "Now drawing from 'Battery Power'\n -InternalBattery-0\t0%; discharging;";
    assert!(parse_battery_low_for_sync(batt_0));

    // Discharging on battery power at or above 20% must NOT trigger guard
    let batt_20 = "Now drawing from 'Battery Power'\n -InternalBattery-0\t20%; discharging;";
    assert!(!parse_battery_low_for_sync(batt_20));

    let batt_25 = "Now drawing from 'Battery Power'\n -InternalBattery-0 (id=23003235)\t25%; discharging; 2:10 remaining present: true";
    assert!(!parse_battery_low_for_sync(batt_25));

    let batt_80 = "Now drawing from 'Battery Power'\n -InternalBattery-0 (id=23003235)\t80%; discharging; 6:00 remaining present: true";
    assert!(!parse_battery_low_for_sync(batt_80));

    let batt_100 = "Now drawing from 'Battery Power'\n -InternalBattery-0 (id=23003235)\t100%; discharging; 8:00 remaining present: true";
    assert!(!parse_battery_low_for_sync(batt_100));

    // Malformed or desktop outputs must default to false
    assert!(!parse_battery_low_for_sync("Now drawing from 'AC Power'\n"));
    assert!(!parse_battery_low_for_sync("No battery detected"));
    assert!(!parse_battery_low_for_sync(""));
}

#[test]
fn test_native_battery_guard_safe_execution() {
    // Calling the native battery check function must execute safely without panicking
    let _ = is_battery_low_for_sync();
}

#[test]
fn test_idle_debouncer_lifecycle_and_reset() {
    let timeout = Duration::from_millis(80);
    let mut debouncer = IdleDebouncer::new(timeout);

    // Initial state: quiet, no pending syncs, zero CPU wait
    assert!(!debouncer.has_pending());
    assert!(!debouncer.should_trigger());
    assert_eq!(debouncer.time_until_trigger(), None);

    // First change: debouncer starts counting
    debouncer.record_change();
    assert!(debouncer.has_pending());
    assert!(!debouncer.should_trigger());
    let wait1 = debouncer.time_until_trigger().unwrap();
    assert!(wait1 <= timeout);

    // Second change at 40ms: user continues typing, timer resets
    std::thread::sleep(Duration::from_millis(40));
    debouncer.record_change();
    assert!(!debouncer.should_trigger());

    // At 70ms total (only 30ms since 2nd change): still within debounce window
    std::thread::sleep(Duration::from_millis(30));
    assert!(!debouncer.should_trigger());

    // Wait another 60ms (90ms since 2nd change, > 80ms timeout): debouncer triggers!
    std::thread::sleep(Duration::from_millis(60));
    assert!(debouncer.should_trigger());
    assert_eq!(debouncer.time_until_trigger(), Some(Duration::ZERO));

    // Once sync finishes, mark synced
    debouncer.mark_synced();
    assert!(!debouncer.has_pending());
    assert!(!debouncer.should_trigger());
    assert_eq!(debouncer.time_until_trigger(), None);
}

#[test]
fn test_idle_debouncer_postpone_and_resume() {
    let timeout = Duration::from_millis(50);
    let mut debouncer = IdleDebouncer::new(timeout);

    debouncer.record_change();
    std::thread::sleep(Duration::from_millis(60));
    assert!(debouncer.should_trigger());

    // Postpone auto-sync for 60ms (e.g. low battery condition detected)
    debouncer.postpone(Duration::from_millis(60));
    assert!(debouncer.is_postponed());
    assert!(!debouncer.should_trigger());
    let wait = debouncer.time_until_trigger().unwrap();
    assert!(wait <= Duration::from_millis(60));

    // After postpone interval passes
    std::thread::sleep(Duration::from_millis(70));
    assert!(!debouncer.is_postponed());
    assert!(debouncer.should_trigger());
    assert_eq!(debouncer.time_until_trigger(), Some(Duration::ZERO));
}

#[test]
fn test_offline_smart_auto_sync_end_to_end() -> Result<()> {
    // Isolated offline environment using tempdir
    let temp_root = tempdir()?;
    let local_project_raw = temp_root.path().join("my_rust_project");
    let remote_drive = temp_root.path().join("GoogleDrive_DevDrive");
    let db_path = temp_root.path().join("devdrive.db");
    let cache_path = temp_root.path().join("cache");

    fs::create_dir_all(local_project_raw.join("src"))?;
    fs::create_dir_all(&remote_drive)?;
    fs::create_dir_all(&cache_path)?;

    let local_project = local_project_raw.canonicalize()?;

    // Create source files
    let main_rs = local_project.join("src/main.rs");
    let lib_rs = local_project.join("src/lib.rs");
    fs::write(&main_rs, "fn main() { println!(\"Hello\"); }")?;
    fs::write(&lib_rs, "pub fn add(a: i32, b: i32) -> i32 { a + b }")?;

    // Register project in SQLite
    let mut db = Database::open(&db_path)?;
    let proj = db.register_project(
        "my_rust_project",
        &local_project.to_string_lossy(),
        &remote_drive.join("my_rust_project").to_string_lossy(),
    )?;

    let filter = DevFilter::for_project(&local_project);

    // Initial files recorded
    assert!(process_event_path(&main_rs, &local_project, &filter, &mut db, proj.id)?);
    assert!(process_event_path(&lib_rs, &local_project, &filter, &mut db, proj.id)?);

    // Verify WAL has 2 pending events
    let pending_before = db.get_pending_wal_events(proj.id)?;
    assert_eq!(pending_before.len(), 2);

    // Initialize debouncer with short offline timeout (50ms)
    let mut debouncer = IdleDebouncer::new(Duration::from_millis(50));
    debouncer.record_change();

    // User is active: modifies main.rs after 20ms
    std::thread::sleep(Duration::from_millis(20));
    fs::write(&main_rs, "fn main() { println!(\"Updated v2\"); }")?;
    assert!(process_event_path(&main_rs, &local_project, &filter, &mut db, proj.id)?);
    debouncer.record_change();

    // At 30ms after update: still quiet period, shouldn't trigger
    std::thread::sleep(Duration::from_millis(30));
    assert!(!debouncer.should_trigger());

    // 40ms more (70ms total since last change): quiet period elapsed!
    std::thread::sleep(Duration::from_millis(40));
    assert!(debouncer.should_trigger());

    // Trigger auto-sync bundle
    let cache_mgr = CacheManager::new(&cache_path, 1024 * 1024 * 1024)?;
    let mut sync_engine = SyncEngine::new(&mut db, &cache_mgr);
    let remote_dest = Path::new(&proj.remote_path);

    let report = sync_engine.sync_bundle(&local_project, remote_dest, false)?;
    assert_eq!(report.files_packaged, 2);
    assert!(report.bundle_bytes > 0);

    // Verify bundle and manifest exist in offline remote directory
    let bundle_file = remote_dest.join("bundle.tar.gz");
    let manifest_file = remote_dest.join("manifest.json");
    assert!(bundle_file.exists());
    assert!(manifest_file.exists());

    // Verify WAL events are marked synced and pruned
    let pending_after = db.get_pending_wal_events(proj.id)?;
    assert_eq!(pending_after.len(), 0);

    // Verify last_synced_at timestamp in database
    let updated_proj = db.get_project_by_id(proj.id)?.unwrap();
    assert!(updated_proj.last_synced_at.is_some());

    debouncer.mark_synced();
    assert!(!debouncer.has_pending());
    assert!(!debouncer.should_trigger());

    Ok(())
}

#[test]
fn test_offline_smart_auto_sync_ignores_ephemeral_changes() -> Result<()> {
    let temp_root = tempdir()?;
    let local_project_raw = temp_root.path().join("web_project");
    let remote_drive = temp_root.path().join("remote");
    let db_path = temp_root.path().join("test.db");

    fs::create_dir_all(local_project_raw.join("node_modules/react"))?;
    fs::create_dir_all(local_project_raw.join("target/debug"))?;
    fs::create_dir_all(&remote_drive)?;

    let local_project = local_project_raw.canonicalize()?;

    let mut db = Database::open(&db_path)?;
    let proj = db.register_project(
        "web_project",
        &local_project.to_string_lossy(),
        &remote_drive.join("web_project").to_string_lossy(),
    )?;

    let filter = DevFilter::for_project(&local_project);

    // Ephemeral node_modules file change
    let nm_file = local_project.join("node_modules/react/index.js");
    fs::write(&nm_file, "// react bundle")?;
    let handled_nm = process_event_path(&nm_file, &local_project, &filter, &mut db, proj.id)?;
    assert!(!handled_nm, "node_modules changes must be ignored by fast path and filter");

    // Ephemeral build target change
    let target_file = local_project.join("target/debug/app.d");
    fs::write(&target_file, "app.o: src/main.rs")?;
    let handled_target = process_event_path(&target_file, &local_project, &filter, &mut db, proj.id)?;
    assert!(!handled_target, "target/ build artifacts must be ignored");

    // WAL should remain completely empty
    let pending = db.get_pending_wal_events(proj.id)?;
    assert_eq!(pending.len(), 0);

    // Debouncer stays idle
    let debouncer = IdleDebouncer::new(Duration::from_millis(50));
    assert!(!debouncer.has_pending());
    assert!(!debouncer.should_trigger());

    Ok(())
}

#[test]
fn test_mac_background_qos() {
    set_background_qos();
}
