use anyhow::{Context, Result};
use notify_debouncer_mini::{new_debouncer, notify::RecursiveMode};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::channel;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tracing::{error, info, warn};

use crate::cache::CacheManager;
use crate::db::Database;
use crate::filter::{DevFilter, FileCategory};
use crate::sync::SyncEngine;
use crate::wal::{ChangeType, WalManager};

/// Sets the current thread and process priority to background QoS on macOS / Unix
pub fn set_background_qos() {
    #[cfg(unix)]
    unsafe {
        // Lower CPU priority to nice 19 (minimum CPU priority)
        libc::setpriority(libc::PRIO_PROCESS, 0, 19);
    }
    #[cfg(target_os = "macos")]
    unsafe {
        // Darwin QOS_CLASS_BACKGROUND = 0x09
        unsafe extern "C" {
            fn pthread_set_qos_class_self_np(qos_class: u32, relative_priority: i32) -> i32;
        }
        let _ = pthread_set_qos_class_self_np(0x09, 0);
    }
}

/// Checks if running on battery power with < 20% remaining charge on macOS.
/// If running on AC power or not on macOS, returns false (safe to sync).
pub fn is_battery_low_for_sync() -> bool {
    #[cfg(target_os = "macos")]
    {
        let output = std::process::Command::new("pmset")
            .arg("-g")
            .arg("batt")
            .output();
        if let Ok(out) = output {
            let s = String::from_utf8_lossy(&out.stdout);
            return parse_battery_low_for_sync(&s);
        }
    }
    false
}

/// Parses output of `pmset -g batt` to determine if running on battery with < 20% remaining charge.
pub fn parse_battery_low_for_sync(output: &str) -> bool {
    let lower = output.to_lowercase();
    // Only guard when actually running on battery power.
    // If running on AC Power, syncing is safe and allowed even at low percentage.
    if !lower.contains("battery power") && !lower.contains("drawing from 'battery") {
        return false;
    }

    for line in output.lines() {
        if let Some(pct_idx) = line.find('%') {
            let prefix = &line[..pct_idx];
            let digits: String = prefix
                .chars()
                .rev()
                .take_while(|c| c.is_ascii_digit())
                .collect();
            if !digits.is_empty() {
                let val_str: String = digits.chars().rev().collect();
                if let Ok(pct) = val_str.parse::<u32>()
                    && pct < 20 {
                        return true;
                    }
            }
        }
    }

    false
}

/// Ultra-fast string check (Fast Path) to immediately skip massive ephemeral build/dependency trees
/// such as node_modules, target, git, venv without running any DB query or complex path operations.
pub fn is_fast_ignored_path<P: AsRef<Path>>(path: P) -> bool {
    let s = path.as_ref().to_string_lossy();
    s.contains("/node_modules/")
        || s.contains("/.git/")
        || s.contains("/target/")
        || s.contains("/.venv/")
        || s.contains("/.next/")
        || s.contains("/DerivedData/")
        || s.contains("/.dartServer/")
        || (cfg!(windows) && (
            s.contains("\\node_modules\\")
            || s.contains("\\.git\\")
            || s.contains("\\target\\")
            || s.contains("\\.venv\\")
            || s.contains("\\.next\\")
            || s.contains("\\DerivedData\\")
            || s.contains("\\.dartServer\\")
        ))
}

/// IdleDebouncer tracks user activity and determines when the workspace has remained
/// completely quiet for the specified debounce duration (e.g. 30 minutes).
#[derive(Debug, Clone)]
pub struct IdleDebouncer {
    idle_timeout: Duration,
    last_change: Option<Instant>,
    has_pending_changes: bool,
    postpone_until: Option<Instant>,
}

impl IdleDebouncer {
    pub fn new(idle_timeout: Duration) -> Self {
        Self {
            idle_timeout,
            last_change: None,
            has_pending_changes: false,
            postpone_until: None,
        }
    }

    /// Records a file change, resetting the idle countdown window.
    pub fn record_change(&mut self) {
        self.last_change = Some(Instant::now());
        self.has_pending_changes = true;
        self.postpone_until = None;
    }

    pub fn has_pending(&self) -> bool {
        self.has_pending_changes
    }

    pub fn set_pending(&mut self, pending: bool) {
        self.has_pending_changes = pending;
    }

    pub fn last_change(&self) -> Option<Instant> {
        self.last_change
    }

    pub fn set_last_change(&mut self, time: Instant) {
        self.last_change = Some(time);
    }

    pub fn idle_timeout(&self) -> Duration {
        self.idle_timeout
    }

    pub fn set_idle_timeout(&mut self, timeout: Duration) {
        self.idle_timeout = timeout;
    }

    /// Postpones auto-sync trigger by the given duration (e.g., when battery is low)
    pub fn postpone(&mut self, duration: Duration) {
        self.postpone_until = Some(Instant::now() + duration);
    }

    pub fn is_postponed(&self) -> bool {
        if let Some(until) = self.postpone_until {
            Instant::now() < until
        } else {
            false
        }
    }

    /// Returns true if changes are pending and the quiet window has elapsed with no postponement active.
    pub fn should_trigger(&self) -> bool {
        if self.is_postponed() {
            return false;
        }
        if !self.has_pending_changes {
            return false;
        }
        if let Some(last) = self.last_change {
            last.elapsed() >= self.idle_timeout
        } else {
            false
        }
    }

    /// Returns the remaining duration until auto-sync trigger, or None if no changes are pending.
    pub fn time_until_trigger(&self) -> Option<Duration> {
        if !self.has_pending_changes {
            return None;
        }

        if let Some(until) = self.postpone_until {
            let now = Instant::now();
            if now < until {
                return Some(until - now);
            }
        }

        if let Some(last) = self.last_change {
            let elapsed = last.elapsed();
            if elapsed >= self.idle_timeout {
                Some(Duration::ZERO)
            } else {
                Some(self.idle_timeout - elapsed)
            }
        } else {
            None
        }
    }

    /// Resets pending status and idle timer after a successful sync
    pub fn mark_synced(&mut self) {
        self.has_pending_changes = false;
        self.last_change = None;
        self.postpone_until = None;
    }
}

/// Options configuring the workspace watcher
#[derive(Debug, Clone)]
pub struct WatcherOptions {
    pub debounce_ms: u64,
    pub idle_timeout: Duration,
    pub battery_guard_enabled: bool,
    pub auto_sync_enabled: bool,
    pub auto_snapshot: bool,
    pub shutdown: Option<Arc<AtomicBool>>,
}

impl Default for WatcherOptions {
    fn default() -> Self {
        Self {
            debounce_ms: 500,
            idle_timeout: Duration::from_secs(30 * 60),
            battery_guard_enabled: true,
            auto_sync_enabled: true,
            auto_snapshot: true,
            shutdown: None,
        }
    }
}

/// Processes a single path event. Returns Ok(true) if a source file change was successfully recorded into WAL.
pub fn process_event_path(
    path: &Path,
    project_dir: &Path,
    filter: &DevFilter,
    db: &mut Database,
    project_id: i64,
) -> Result<bool> {
    // Fast Path: Immediately skip high-frequency ephemeral directories (node_modules, target, .git, etc.)
    if is_fast_ignored_path(path) {
        return Ok(false);
    }

    if let Ok(rel) = path.strip_prefix(project_dir) {
        let category = filter.categorize(rel);
        if category != FileCategory::Source {
            return Ok(false);
        }

        let rel_str = rel.to_string_lossy().to_string();

        // Filter out directories: only track individual file events
        if path.exists() && !path.is_file() {
            return Ok(false);
        }

        // If path does not exist (deletion), check if it was a tracked file in files_metadata
        let change_type = if path.exists() {
            ChangeType::Modified
        } else {
            let was_tracked = db.is_tracked_file(project_id, &rel_str).unwrap_or(false);
            if !was_tracked {
                return Ok(false);
            }
            ChangeType::Deleted
        };

        let mut wal = WalManager::new(db);
        if let Ok(true) = wal.record_change(project_id, change_type.clone(), rel) {
            info!("📝 Tracked source change: {}", rel.display());

            match change_type {
                ChangeType::Created | ChangeType::Modified => {
                    if path.exists() && path.is_file()
                        && let Ok(meta) = std::fs::metadata(path) {
                            let size = meta.len() as i64;
                            let modified = meta
                                .modified()
                                .ok()
                                .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                                .map(|d| d.as_secs() as i64)
                                .unwrap_or(0);

                            let hash = CacheManager::compute_sha256(path).unwrap_or_default();
                            let now = chrono::Utc::now().timestamp();

                            let _ = db.upsert_file_metadata(
                                project_id,
                                &rel_str,
                                size,
                                &hash,
                                "source",
                                modified,
                                now,
                                false,
                            );
                        }
                }
                ChangeType::Deleted => {
                    let _ = db.delete_file_metadata(project_id, &rel_str);
                }
            }
            return Ok(true);
        }
    }

    Ok(false)
}

enum LoopAction {
    Events(Vec<notify_debouncer_mini::DebouncedEvent>),
    Error(notify_debouncer_mini::notify::Error),
    TriggerOrTimeout,
    Idle,
    Shutdown,
}

pub struct WorkspaceWatcher {
    project_dir: PathBuf,
    filter: DevFilter,
}

impl WorkspaceWatcher {
    pub fn new<P: AsRef<Path>>(project_dir: P) -> Result<Self> {
        let canonical = project_dir.as_ref().canonicalize()?;
        let filter = DevFilter::for_project(&canonical);
        Ok(Self {
            project_dir: canonical,
            filter,
        })
    }

    /// Starts an active event loop monitoring the workspace directory for changes
    pub fn watch_loop(&self, db_path: &Path, debounce_ms: u64) -> Result<()> {
        let config = crate::config::DevDriveConfig::load().unwrap_or_default();
        let options = WatcherOptions {
            debounce_ms,
            idle_timeout: Duration::from_secs(config.auto_sync_idle_minutes * 60),
            battery_guard_enabled: config.battery_guard_enabled,
            auto_sync_enabled: config.auto_sync_enabled,
            auto_snapshot: config.auto_snapshot_on_sync,
            shutdown: None,
        };
        self.watch_loop_with_options(db_path, options)
    }

    /// Starts watch loop with full configuration
    pub fn watch_loop_with_config(&self, db_path: &Path, config: &crate::config::DevDriveConfig) -> Result<()> {
        let options = WatcherOptions {
            debounce_ms: config.debounce_ms,
            idle_timeout: Duration::from_secs(config.auto_sync_idle_minutes * 60),
            battery_guard_enabled: config.battery_guard_enabled,
            auto_sync_enabled: config.auto_sync_enabled,
            auto_snapshot: config.auto_snapshot_on_sync,
            shutdown: None,
        };
        self.watch_loop_with_options(db_path, options)
    }

    /// Starts watch loop with customizable WatcherOptions (ideal for testing and custom debouncing)
    pub fn watch_loop_with_options(&self, db_path: &Path, options: WatcherOptions) -> Result<()> {
        set_background_qos();

        let (tx, rx) = channel();

        let mut debouncer = new_debouncer(Duration::from_millis(options.debounce_ms), tx)
            .context("Failed to initialize FSEvents debouncer")?;

        debouncer
            .watcher()
            .watch(&self.project_dir, RecursiveMode::Recursive)
            .context("Failed to attach watcher to workspace")?;

        info!(
            "⚡ DevDrive FSEvents Watcher active on: {} (Idle debounce: {:?}, Battery guard: {})",
            self.project_dir.display(),
            options.idle_timeout,
            options.battery_guard_enabled
        );

        let mut db = Database::open(db_path)?;
        let project_str = self.project_dir.to_string_lossy().to_string();
        let project = db
            .get_project_by_local_path(&project_str)?
            .with_context(|| format!("Project {} not registered in database", project_str))?;

        let mut idle_debouncer = IdleDebouncer::new(options.idle_timeout);

        // If there are already unsynced WAL events from earlier, activate debouncer
        if let Ok(pending) = db.get_pending_wal_events(project.id)
            && !pending.is_empty() {
                idle_debouncer.record_change();
                info!(
                    "📌 Detected {} pending WAL event(s) for {}. Auto-sync scheduled after idle window ({:?})",
                    pending.len(),
                    project.name,
                    options.idle_timeout
                );
            }

        loop {
            if let Some(ref s) = options.shutdown
                && s.load(Ordering::Relaxed) {
                    break;
                }

            let wait_time = idle_debouncer.time_until_trigger();

            let action = match wait_time {
                None => {
                    // No pending changes: Wait indefinitely until next file change (0% CPU)
                    if options.shutdown.is_some() {
                        match rx.recv_timeout(Duration::from_millis(500)) {
                            Ok(res) => match res {
                                Ok(events) => LoopAction::Events(events),
                                Err(err) => LoopAction::Error(err),
                            },
                            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => LoopAction::Idle,
                            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => LoopAction::Shutdown,
                        }
                    } else {
                        match rx.recv() {
                            Ok(Ok(events)) => LoopAction::Events(events),
                            Ok(Err(err)) => LoopAction::Error(err),
                            Err(_) => LoopAction::Shutdown,
                        }
                    }
                }
                Some(remaining) if remaining == Duration::ZERO => {
                    LoopAction::TriggerOrTimeout
                }
                Some(remaining) => {
                    let wait_slice = if options.shutdown.is_some() {
                        remaining.min(Duration::from_millis(500))
                    } else {
                        remaining
                    };

                    match rx.recv_timeout(wait_slice) {
                        Ok(Ok(events)) => LoopAction::Events(events),
                        Ok(Err(err)) => LoopAction::Error(err),
                        Err(std::sync::mpsc::RecvTimeoutError::Timeout) => LoopAction::TriggerOrTimeout,
                        Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => LoopAction::Shutdown,
                    }
                }
            };

            match action {
                LoopAction::Shutdown => break,
                LoopAction::Idle => {}
                LoopAction::Events(events) => {
                    let mut changed = false;
                    for event in events {
                        if process_event_path(&event.path, &self.project_dir, &self.filter, &mut db, project.id)? {
                            changed = true;
                        }
                    }
                    while let Ok(Ok(more_events)) = rx.try_recv() {
                        for event in more_events {
                            if process_event_path(&event.path, &self.project_dir, &self.filter, &mut db, project.id)? {
                                changed = true;
                            }
                        }
                    }
                    if changed {
                        idle_debouncer.record_change();
                        info!(
                            "⏳ User active in {}. Idle debounce window reset ({:?})",
                            project.name, options.idle_timeout
                        );
                    }
                }
                LoopAction::Error(err) => {
                    error!("Error received from file watcher: {:?}", err);
                }
                LoopAction::TriggerOrTimeout => {
                    if idle_debouncer.should_trigger() {
                        if options.auto_sync_enabled {
                            if options.battery_guard_enabled && is_battery_low_for_sync() {
                                info!(
                                    "🔋 Battery guard: battery < 20% on battery power. Auto-sync deferred for 60s on '{}'",
                                    project.name
                                );
                                idle_debouncer.postpone(Duration::from_secs(60));
                            } else {
                                let pending = db.get_pending_wal_events(project.id).unwrap_or_default();
                                if !pending.is_empty() {
                                    info!(
                                        "🚀 Idle window completed with {} pending change(s). Executing background auto-sync for '{}'...",
                                        pending.len(),
                                        project.name
                                    );
                                    let cache_dir = CacheManager::default_cache_dir();
                                    let max_cache_bytes = crate::config::DevDriveConfig::load()
                                        .map(|c| c.max_cache_bytes())
                                        .unwrap_or(20 * 1024 * 1024 * 1024);
                                    if let Ok(cache_mgr) = CacheManager::new(&cache_dir, max_cache_bytes) {
                                        let mut sync_engine = SyncEngine::new(&mut db, &cache_mgr);
                                        let remote_dest = PathBuf::from(&project.remote_path);
                                        match sync_engine.sync_bundle(&self.project_dir, &remote_dest, options.auto_snapshot) {
                                            Ok(report) => {
                                                info!(
                                                    "✅ Auto-sync completed for '{}': {} files packaged ({})", 
                                                    project.name, report.files_packaged, crate::format_bytes(report.bundle_bytes)
                                                );
                                                crate::send_system_notification(
                                                    "Otomatik Eşitleme",
                                                    &format!(
                                                        "'{}' projesi kesintisiz boşta kalma sonrası Google Drive'a eşitlendi ({} dosya, {}).", 
                                                        project.name, report.files_packaged, crate::format_bytes(report.bundle_bytes)
                                                    )
                                                );
                                            }
                                            Err(err) => {
                                                error!("❌ Auto-sync failed for '{}': {:?}", project.name, err);
                                            }
                                        }
                                    }
                                }
                                idle_debouncer.mark_synced();
                            }
                        } else {
                            idle_debouncer.mark_synced();
                        }
                    }
                }
            }
        }

        Ok(())
    }
}

pub fn expand_tilde(path_str: &str) -> PathBuf {
    if let Some(stripped) = path_str.strip_prefix("~/") {
        dirs::home_dir().unwrap_or_else(|| PathBuf::from(".")).join(stripped)
    } else if path_str == "~" {
        dirs::home_dir().unwrap_or_else(|| PathBuf::from("."))
    } else {
        PathBuf::from(path_str)
    }
}

pub fn discover_projects_once(
    db_path: &Path,
    config: &crate::config::DevDriveConfig,
) -> Result<usize> {
    if !config.auto_discover_enabled {
        return Ok(0);
    }

    let mut db = Database::open(db_path)?;
    let mut discovered = 0;

    let expanded_roots: Vec<PathBuf> = config
        .auto_discover_paths
        .iter()
        .map(|p| expand_tilde(p))
        .collect();

    let canonical_roots: Vec<PathBuf> = expanded_roots
        .iter()
        .filter_map(|p| p.canonicalize().ok())
        .collect();

    let canonical_remote = expand_tilde(&config.remote_drive_base_path)
        .canonicalize()
        .ok();

    for root in &expanded_roots {
        if !root.exists() || !root.is_dir() {
            continue;
        }

        let entries = match std::fs::read_dir(root) {
            Ok(e) => e,
            Err(_) => continue,
        };

        for entry in entries.filter_map(|e| e.ok()) {
            let path = entry.path();
            let ft = match entry.file_type() {
                Ok(ft) => ft,
                Err(_) => continue,
            };

            if !ft.is_dir() || ft.is_symlink() {
                continue;
            }

            let folder_name = entry.file_name().to_string_lossy().to_string();

            // 1. Skip dotfiles/hidden dirs, .app, .framework
            if folder_name.starts_with('.')
                || folder_name.ends_with(".app")
                || folder_name.ends_with(".framework")
            {
                continue;
            }

            // 2. Skip well-known build/ephemeral dirs
            let ephemeral_names = [
                "node_modules", "target", "build", "dist", "out", "vendor", "venv", ".venv",
                "DerivedData", "Pods",
            ];
            if ephemeral_names.contains(&folder_name.as_str()) {
                continue;
            }

            let canonical = match path.canonicalize() {
                Ok(c) => c,
                Err(_) => continue,
            };

            // 3. Skip if this directory itself is another root directory (e.g. ~/Documents/antigravity when scanning ~/Documents)
            if canonical_roots.contains(&canonical) {
                continue;
            }

            // 4. Skip if inside remote Google Drive path
            if let Some(ref rem) = canonical_remote
                && (canonical == *rem || canonical.starts_with(rem)) {
                    continue;
                }

            let canonical_str = canonical.to_string_lossy().to_string();

            // 5. Skip if already registered in DB
            if let Some(existing) = db.get_project_by_local_path(&canonical_str)? {
                if folder_name == "adventurous-hypatia" && !existing.pinned {
                    let _ = db.set_project_pinned(existing.id, true);
                }
                continue;
            }

            // 6. Check if directory is in showcase mode
            let is_showcase = crate::offload::is_showcase_dir(&canonical);
            let meta_path = canonical.join(crate::offload::SHOWCASE_META_FILE);
            let showcase_meta: Option<crate::offload::ShowcaseMeta> = if is_showcase {
                std::fs::read_to_string(&meta_path)
                    .ok()
                    .and_then(|s| serde_json::from_str(&s).ok())
            } else {
                None
            };

            // 7. Resolve name collisions
            let parent_dir_name = canonical
                .parent()
                .and_then(|p| p.file_name())
                .map(|n| n.to_string_lossy().to_string())
                .unwrap_or_else(|| "dir".to_string());

            let mut unique_name = folder_name.clone();
            if let Ok(Some(existing)) = db.get_project_by_name(&unique_name)
                && existing.local_path != canonical_str {
                    unique_name = format!("{} ({})", folder_name, parent_dir_name);
                    let mut counter = 2;
                    while let Ok(Some(collision)) = db.get_project_by_name(&unique_name) {
                        if collision.local_path == canonical_str {
                            break;
                        }
                        unique_name = format!("{} ({}) ({})", folder_name, parent_dir_name, counter);
                        counter += 1;
                    }
                }

            let remote_dest = Path::new(&config.remote_drive_base_path)
                .join(&unique_name)
                .to_string_lossy()
                .to_string();

            let proj = db.register_project(&unique_name, &canonical_str, &remote_dest)?;

            if folder_name == "adventurous-hypatia" {
                let _ = db.set_project_pinned(proj.id, true);
            }

            if let Some(meta) = showcase_meta {
                let _ = db.update_project_offload_info(
                    proj.id,
                    "archived",
                    Some(&meta.archived_at),
                    Some(&meta.archive_remote_path),
                    None,
                    None,
                    None,
                    meta.total_bytes as i64,
                );
            }

            discovered += 1;
            info!(
                "✨ Otomatik keşfedilen yeni proje DevDrive'a kaydedildi: '{}' ({})",
                unique_name, canonical_str
            );
        }
    }

    Ok(discovered)
}

pub fn run_lifecycle_maintenance(
    db_path: &Path,
    config: &crate::config::DevDriveConfig,
) -> Result<()> {
    let mut db = Database::open(db_path)?;
    let projects = db.list_projects()?;

    for proj in projects {
        if proj.status != "active" || proj.pinned || proj.name == "adventurous-hypatia" {
            continue;
        }

        let local_path = PathBuf::from(&proj.local_path);
        if !local_path.exists() || !local_path.is_dir() {
            continue;
        }

        // Fast scan for latest source mtime
        let filter = crate::filter::DevFilter::for_project(&local_path);
        let mut latest_mtime = None;
        for entry in walkdir::WalkDir::new(&local_path)
            .min_depth(1)
            .into_iter()
            .filter_entry(|e| {
                if e.file_type().is_dir() {
                    let is_root = e.depth() == 1;
                    if filter.should_skip_dir_entry(e.path(), is_root) {
                        return false;
                    }
                }
                !is_fast_ignored_path(e.path())
            })
            .filter_map(|e| e.ok())
        {
            if entry.file_type().is_file()
                && let Ok(m) = entry.metadata()
                && let Ok(modified) = m.modified()
                && latest_mtime.map(|prev| modified > prev).unwrap_or(true) {
                    latest_mtime = Some(modified);
                }
        }

        let latest = latest_mtime
            .or_else(|| std::fs::metadata(&local_path).ok().and_then(|m| m.modified().ok()))
            .unwrap_or_else(std::time::SystemTime::now);

        let elapsed_secs = std::time::SystemTime::now()
            .duration_since(latest)
            .unwrap_or_default()
            .as_secs();
        let inactive_days = elapsed_secs / (24 * 3600);

        // Stage 3: Showcase Mode (default 30 days)
        if config.auto_showcase_days > 0 && inactive_days >= config.auto_showcase_days {
            info!(
                "👻 Proje '{}' {} gündür inaktif. Vitrin Moduna geçiriliyor...",
                proj.name, inactive_days
            );
            match crate::offload::OffloadEngine::offload(&mut db, proj.id, config, false, true) {
                Ok(_) => {
                    crate::send_system_notification(
                        "Vitrin Moduna Alındı",
                        &format!(
                            "'{}' projesi {} gün inaktif kaldığı için Google Drive'a arşivlendi ve vitrin moduna alındı.",
                            proj.name, inactive_days
                        ),
                    );
                }
                Err(e) => {
                    tracing::warn!("Vitrin moduna geçiş başarısız '{}': {:?}", proj.name, e);
                }
            }
            continue;
        }

        // Stage 2: Slim Mode (default 7 days)
        if config.auto_slim_days > 0 && inactive_days >= config.auto_slim_days
            && proj.last_synced_at.is_some() {
                info!(
                    "🟡 Proje '{}' {} gündür inaktif. Sınıf A artifaktları temizleniyor...",
                    proj.name, inactive_days
                );
                match crate::offload::OffloadEngine::clean_artifacts(&local_path, false) {
                    Ok(rep) if rep.total_freed_bytes > 0 => {
                        crate::send_system_notification(
                            "Otomatik Temizlik (Slim)",
                            &format!(
                                "'{}' projesinin {} boyutundaki geçici artifaktları temizlendi.",
                                proj.name,
                                crate::format_bytes(rep.total_freed_bytes)
                            ),
                        );
                    }
                    Ok(_) => {}
                    Err(e) => {
                        tracing::warn!("Slim temizliği başarısız '{}': {:?}", proj.name, e);
                    }
                }
            }
    }

    Ok(())
}

/// Automatically backs up unsynced active projects to Google Drive in the background.
pub fn auto_sync_unsynced_projects(
    db_path: &Path,
    config: &crate::config::DevDriveConfig,
) {
    if !config.auto_sync_enabled {
        return;
    }
    if let Ok(mut db) = Database::open(db_path)
        && let Ok(projects) = db.list_projects() {
            let unsynced: Vec<_> = projects
                .into_iter()
                .filter(|p| {
                    p.status == "active"
                        && p.last_synced_at.is_none()
                        && !crate::offload::is_showcase_dir(Path::new(&p.local_path))
                        && Path::new(&p.local_path).exists()
                })
                .collect();

            if !unsynced.is_empty() {
                let cache_dir = CacheManager::default_cache_dir();
                if let Ok(cache_mgr) = CacheManager::new(&cache_dir, config.max_cache_bytes()) {
                    for project in unsynced {
                        let local_p = PathBuf::from(&project.local_path);
                        let remote_dest = PathBuf::from(&project.remote_path);
                        info!(
                            "🚀 Otomatik İlk Yedekleme başlatılıyor: '{}' -> '{}'",
                            project.name,
                            remote_dest.display()
                        );
                        let mut sync_engine = SyncEngine::new(&mut db, &cache_mgr);
                        match sync_engine.sync_bundle(&local_p, &remote_dest, config.auto_snapshot_on_sync) {
                            Ok(report) => {
                                info!(
                                    "✅ İlk otomatik yedekleme tamamlandı: '{}' ({} dosya, {})",
                                    project.name,
                                    report.files_packaged,
                                    crate::format_bytes(report.bundle_bytes)
                                );
                                crate::send_system_notification(
                                    "Otomatik İlk Yedekleme",
                                    &format!(
                                        "'{}' projesi başarıyla Google Drive'a yedeklendi ({} dosya).",
                                        project.name, report.files_packaged
                                    ),
                                );
                            }
                            Err(e) => {
                                warn!("⚠️ İlk otomatik yedekleme başarısız ('{}'): {:?}", project.name, e);
                            }
                        }
                    }
                }
            }
        }
}

/// Spawns background watcher threads for all registered projects in the database.
/// Periodically checks for newly registered projects and respects the shutdown flag.
pub fn run_multi_project_watcher(
    db_path: &Path,
    config: &crate::config::DevDriveConfig,
    shutdown: Option<Arc<AtomicBool>>,
) -> Result<()> {
    set_background_qos();
    info!("⚡ DevDrive Multi-Project Background Watcher started");

    // 1. Initial discovery & maintenance
    let _ = discover_projects_once(db_path, config);
    let _ = run_lifecycle_maintenance(db_path, config);

    // 2. Setup 0% CPU Non-Recursive event watcher on auto_discover_paths
    let (disc_tx, disc_rx) = channel();
    let mut root_debouncer = new_debouncer(Duration::from_millis(500), disc_tx).ok();
    if let Some(ref mut deb) = root_debouncer {
        for root_str in &config.auto_discover_paths {
            let root_p = expand_tilde(root_str);
            if root_p.exists() && root_p.is_dir() {
                let _ = deb
                    .watcher()
                    .watch(&root_p, notify_debouncer_mini::notify::RecursiveMode::NonRecursive);
            }
        }
    }

    let mut active_watchers: HashMap<i64, Arc<AtomicBool>> = HashMap::new();
    let mut last_lifecycle = Instant::now();

    loop {
        if let Some(ref s) = shutdown
            && s.load(Ordering::Relaxed) {
                for (_, stop) in active_watchers {
                    stop.store(true, Ordering::Relaxed);
                }
                break;
            }

        // Check if root directories had new folders added
        let mut trigger_discovery = false;
        while disc_rx.try_recv().is_ok() {
            trigger_discovery = true;
        }
        if trigger_discovery {
            let _ = discover_projects_once(db_path, config);
        }

        // Daily lifecycle maintenance
        if last_lifecycle.elapsed() >= Duration::from_secs(24 * 3600) {
            let _ = run_lifecycle_maintenance(db_path, config);
            last_lifecycle = Instant::now();
        }

        if let Ok(db) = Database::open(db_path)
            && let Ok(projects) = db.list_projects() {
                for project in projects {
                    let local_path = PathBuf::from(&project.local_path);
                    let is_showcase = project.status == "archived" || crate::offload::is_showcase_dir(&local_path);

                    if is_showcase {
                        if let Some(stop) = active_watchers.remove(&project.id) {
                            stop.store(true, Ordering::Relaxed);
                        }
                        continue;
                    }

                    if !local_path.exists() {
                        continue;
                    }

                    if let std::collections::hash_map::Entry::Vacant(e) = active_watchers.entry(project.id) {
                        let watcher_stop = Arc::new(AtomicBool::new(false));
                        e.insert(watcher_stop.clone());

                        let p_dir = local_path.clone();
                        let p_name = project.name.clone();
                        let db_p = db_path.to_path_buf();
                        let cfg = config.clone();

                        std::thread::spawn(move || {
                            set_background_qos();
                            info!("👀 Starting watcher for '{}' ({})", p_name, p_dir.display());
                            if let Ok(watcher) = WorkspaceWatcher::new(&p_dir) {
                                let opts = WatcherOptions {
                                    debounce_ms: cfg.debounce_ms,
                                    idle_timeout: Duration::from_secs(cfg.auto_sync_idle_minutes * 60),
                                    battery_guard_enabled: cfg.battery_guard_enabled,
                                    auto_sync_enabled: cfg.auto_sync_enabled,
                                    auto_snapshot: cfg.auto_snapshot_on_sync,
                                    shutdown: Some(watcher_stop),
                                };
                                let _ = watcher.watch_loop_with_options(&db_p, opts);
                            }
                        });
                    }
                }
            }

        for _ in 0..15 {
            if let Some(ref s) = shutdown
                && s.load(Ordering::Relaxed) {
                    break;
                }
            std::thread::sleep(Duration::from_secs(1));
        }
    }

    Ok(())
}

/// Spawns multi-project watcher in an independent background thread and returns the shutdown handle
pub fn start_all_projects_watcher_background(
    db_path: &Path,
    config: &crate::config::DevDriveConfig,
) -> Arc<AtomicBool> {
    let shutdown = Arc::new(AtomicBool::new(false));
    let shutdown_clone = shutdown.clone();
    let db_p = db_path.to_path_buf();
    let cfg = config.clone();

    std::thread::spawn(move || {
        let _ = run_multi_project_watcher(&db_p, &cfg, Some(shutdown_clone));
    });

    shutdown
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_fast_path_ignored_directories() {
        // Must immediately ignore all 7 specified high-frequency ephemeral directories
        assert!(is_fast_ignored_path(Path::new("/workspace/project/node_modules/react/index.js")));
        assert!(is_fast_ignored_path(Path::new("/workspace/project/.git/objects/ab/123456")));
        assert!(is_fast_ignored_path(Path::new("/workspace/project/target/debug/build.log")));
        assert!(is_fast_ignored_path(Path::new("/workspace/project/.venv/lib/python3.11/site.py")));
        assert!(is_fast_ignored_path(Path::new("/workspace/project/.next/cache/webpack.json")));
        assert!(is_fast_ignored_path(Path::new("/workspace/project/DerivedData/App/Build/Products")));
        assert!(is_fast_ignored_path(Path::new("/workspace/project/.dartServer/cache.db")));

        // Must NOT ignore regular source files
        assert!(!is_fast_ignored_path(Path::new("/workspace/project/src/main.rs")));
        assert!(!is_fast_ignored_path(Path::new("/workspace/project/Cargo.toml")));
        assert!(!is_fast_ignored_path(Path::new("/workspace/project/lib/target_utils.rs")));
        assert!(!is_fast_ignored_path(Path::new("/workspace/project/app/components/navbar.tsx")));
    }

    #[test]
    fn test_parse_battery_low_for_sync() {
        // AC Power should never trigger battery guard, even with low percentage
        let ac_low = "Now drawing from 'AC Power'\n -InternalBattery-0 (id=23003235)\t10%; charging; 3:37 remaining present: true";
        assert!(!parse_battery_low_for_sync(ac_low));

        // Battery Power with < 20% MUST trigger guard
        let batt_low_19 = "Now drawing from 'Battery Power'\n -InternalBattery-0 (id=23003235)\t19%; discharging; 1:45 remaining present: true";
        assert!(parse_battery_low_for_sync(batt_low_19));

        let batt_low_5 = "Now drawing from 'Battery Power'\n -InternalBattery-0\t5%; discharging;";
        assert!(parse_battery_low_for_sync(batt_low_5));

        let batt_low_0 = "Now drawing from 'Battery Power'\n -InternalBattery-0\t0%; discharging;";
        assert!(parse_battery_low_for_sync(batt_low_0));

        // Battery Power with >= 20% should NOT trigger guard
        let batt_20 = "Now drawing from 'Battery Power'\n -InternalBattery-0\t20%; discharging;";
        assert!(!parse_battery_low_for_sync(batt_20));

        let batt_85 = "Now drawing from 'Battery Power'\n -InternalBattery-0 (id=23003235)\t85%; discharging; 6:30 remaining present: true";
        assert!(!parse_battery_low_for_sync(batt_85));

        // Desktops without battery or malformed output
        let desktop = "Now drawing from 'AC Power'\n";
        assert!(!parse_battery_low_for_sync(desktop));

        let empty = "";
        assert!(!parse_battery_low_for_sync(empty));
    }

    #[test]
    fn test_idle_debouncer_behavior() {
        let timeout = Duration::from_millis(60);
        let mut debouncer = IdleDebouncer::new(timeout);

        // Initially no changes
        assert!(!debouncer.has_pending());
        assert!(!debouncer.should_trigger());
        assert_eq!(debouncer.time_until_trigger(), None);

        // Record a change
        debouncer.record_change();
        assert!(debouncer.has_pending());
        assert!(!debouncer.should_trigger());
        let wait = debouncer.time_until_trigger().unwrap();
        assert!(wait <= timeout && wait > Duration::ZERO);

        // Record another change (resetting countdown)
        std::thread::sleep(Duration::from_millis(30));
        debouncer.record_change();
        assert!(!debouncer.should_trigger());

        // Wait past the timeout
        std::thread::sleep(Duration::from_millis(70));
        assert!(debouncer.should_trigger());
        assert_eq!(debouncer.time_until_trigger(), Some(Duration::ZERO));

        // Mark synced
        debouncer.mark_synced();
        assert!(!debouncer.has_pending());
        assert!(!debouncer.should_trigger());
        assert_eq!(debouncer.time_until_trigger(), None);
    }

    #[test]
    fn test_idle_debouncer_postpone() {
        let timeout = Duration::from_millis(40);
        let mut debouncer = IdleDebouncer::new(timeout);

        debouncer.record_change();
        std::thread::sleep(Duration::from_millis(50));
        assert!(debouncer.should_trigger());

        // Postpone by 50ms (e.g. low battery)
        debouncer.postpone(Duration::from_millis(50));
        assert!(debouncer.is_postponed());
        assert!(!debouncer.should_trigger());

        // After postpone duration elapses
        std::thread::sleep(Duration::from_millis(60));
        assert!(!debouncer.is_postponed());
        assert!(debouncer.should_trigger());
    }

    #[test]
    fn test_set_background_qos() {
        // Ensure function executes without crashing
        set_background_qos();
    }

    #[test]
    fn test_discover_projects_once() -> Result<()> {
        let temp = tempfile::tempdir()?;
        let desktop = temp.path().join("Desktop");
        let documents = temp.path().join("Documents");
        let antigravity = documents.join("antigravity");
        let remote_drive = temp.path().join("DevDrive");
        let db_file = temp.path().join("test.db");

        std::fs::create_dir_all(&desktop)?;
        std::fs::create_dir_all(&documents)?;
        std::fs::create_dir_all(&antigravity)?;
        std::fs::create_dir_all(&remote_drive)?;

        // Normal projects
        std::fs::create_dir_all(desktop.join("app1"))?;
        std::fs::create_dir_all(antigravity.join("app2"))?;
        std::fs::create_dir_all(antigravity.join("adventurous-hypatia"))?;

        // Collision: 'nofa' on Desktop and 'nofa' in antigravity
        std::fs::create_dir_all(desktop.join("nofa"))?;
        std::fs::create_dir_all(antigravity.join("nofa"))?;

        // Ignored items
        std::fs::create_dir_all(desktop.join(".hidden_project"))?;
        std::fs::create_dir_all(desktop.join("DevDrive.app"))?;
        std::fs::create_dir_all(desktop.join("node_modules"))?;

        let config = crate::config::DevDriveConfig {
            remote_drive_base_path: remote_drive.to_string_lossy().to_string(),
            max_cache_gb: 5,
            debounce_ms: 100,
            auto_snapshot_on_sync: false,
            custom_ephemeral_dirs: vec![],
            backup_env_files: true,
            quarantine_days: 7,
            quarantine_max_gb: 5,
            include_git: true,
            quarantine_base_path: None,
            auto_sync_idle_minutes: 30,
            battery_guard_enabled: true,
            auto_sync_enabled: true,
            auto_discover_enabled: true,
            auto_discover_paths: vec![
                desktop.to_string_lossy().to_string(),
                documents.to_string_lossy().to_string(),
                antigravity.to_string_lossy().to_string(),
            ],
            auto_slim_days: 7,
            auto_showcase_days: 30,
            use_direct_showcase: true,
        };

        let count = discover_projects_once(&db_file, &config)?;
        assert_eq!(count, 5);

        let db = Database::open(&db_file)?;
        let projects = db.list_projects()?;
        assert_eq!(projects.len(), 5);

        let hypatia = db.get_project_by_name("adventurous-hypatia")?.unwrap();
        assert!(hypatia.pinned, "adventurous-hypatia must be automatically pinned");

        // Verify collision handled
        let nofa1 = db.get_project_by_name("nofa")?.unwrap();
        let nofa2 = db.get_project_by_name("nofa (antigravity)")?.unwrap();
        assert_ne!(nofa1.local_path, nofa2.local_path);

        // Run again -> 0 new projects
        let count_second = discover_projects_once(&db_file, &config)?;
        assert_eq!(count_second, 0);

        Ok(())
    }
}
