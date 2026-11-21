use anyhow::Result;
use axum::{
    extract::{Query, State},
    http::StatusCode,
    response::{Html, IntoResponse, Json},
    routing::{get, post},
    Router,
};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use crate::cache::CacheManager;
use crate::config::DevDriveConfig;
use crate::db::Database;
use crate::format_bytes;
use crate::snapshot::SnapshotEngine;
use crate::sync::SyncEngine;

#[derive(Clone, Debug, Serialize)]
pub struct SyncProgressTracker {
    pub processed_files: usize,
    pub total_files: usize,
    pub percent: u32,
}

#[derive(Clone, Debug)]
pub struct CachedProjectMetrics {
    pub local_file_count: usize,
    pub remote_file_count: usize,
    pub remote_used_bytes: u64,
    pub reclaimable_bytes: u64,
    pub modified_files_count: usize,
    pub inactive_days: u64,
    pub is_showcase: bool,
    pub tech_badges: Vec<String>,
    pub updated_at: std::time::Instant,
}

#[derive(Clone, Debug, Serialize)]
pub struct BulkJobDto {
    pub active: bool,
    pub total_projects: usize,
    pub completed_projects: usize,
    pub current_project_id: Option<i64>,
    pub current_project_name: String,
    pub current_file: String,
    pub current_files_done: usize,
    pub current_files_total: usize,
    pub overall_percent: u8,
    pub status_text: String,
}

#[derive(Clone)]
pub struct AppState {
    pub db_path: PathBuf,
    pub config: Arc<std::sync::RwLock<DevDriveConfig>>,
    pub sync_progress: Arc<Mutex<HashMap<i64, SyncProgressTracker>>>,
    pub metrics_cache: Arc<Mutex<HashMap<i64, CachedProjectMetrics>>>,
    pub metrics_refreshing: Arc<std::sync::atomic::AtomicBool>,
    pub bulk_job: Arc<Mutex<Option<BulkJobDto>>>,
    pub cancel_flag: Arc<std::sync::atomic::AtomicBool>,
    pub cached_cache_usage: Arc<std::sync::atomic::AtomicU64>,
}

impl AppState {
    pub fn new(
        db_path: PathBuf,
        config: Arc<std::sync::RwLock<DevDriveConfig>>,
        sync_progress: Arc<Mutex<HashMap<i64, SyncProgressTracker>>>,
    ) -> Self {
        Self {
            db_path,
            config,
            sync_progress,
            metrics_cache: Arc::new(Mutex::new(HashMap::new())),
            metrics_refreshing: Arc::new(std::sync::atomic::AtomicBool::new(false)),
            bulk_job: Arc::new(Mutex::new(None)),
            cancel_flag: Arc::new(std::sync::atomic::AtomicBool::new(false)),
            cached_cache_usage: Arc::new(std::sync::atomic::AtomicU64::new(0)),
        }
    }

    pub fn get_config(&self) -> DevDriveConfig {
        self.config.read().map(|g| g.clone()).unwrap_or_else(|_| DevDriveConfig::default())
    }
}

#[derive(Serialize)]
pub struct StatusResponse {
    pub current_cache_bytes: u64,
    pub max_cache_bytes: u64,
    pub remote_drive_path: String,
    pub projects: Vec<ProjectDto>,
    pub total_space_saved_bytes: u64,
    pub total_reclaimable_bytes: u64,
    pub remote_used_bytes: u64,
    pub bulk_job: Option<BulkJobDto>,
}

#[derive(Serialize)]
pub struct ProjectDto {
    pub id: i64,
    pub name: String,
    pub local_path: String,
    pub remote_path: String,
    pub created_at: String,
    pub last_synced_at: Option<String>,
    pub sync_status: String,
    pub status_message: String,
    pub remote_file_count: usize,
    pub local_file_count: usize,
    pub progress: Option<SyncProgressTracker>,
    pub project_status: String,
    pub archived_at: Option<String>,
    pub archive_remote_path: Option<String>,
    pub quarantine_path: Option<String>,
    pub quarantine_until: Option<String>,
    pub quarantine_days_left: Option<i64>,
    pub freed_bytes: i64,
    pub reclaimable_bytes: u64,
    pub tech_badges: Vec<String>,
    pub pinned: bool,
    pub is_showcase: bool,
    pub inactive_days: u64,
    pub lifecycle_badge: String,
}


#[derive(Deserialize)]
pub struct InitProjectReq {
    pub path: String,
    pub name: Option<String>,
}

#[derive(Deserialize)]
pub struct SyncProjectReq {
    pub path: String,
    pub all: Option<bool>,
    pub prune: Option<bool>,
    pub force: Option<bool>,
}

#[derive(Deserialize)]
pub struct RestoreReq {
    pub remote_name: String,
    pub target_path: String,
    pub force: Option<bool>,
}

#[derive(Deserialize)]
pub struct SnapshotReq {
    pub path: String,
    pub message: String,
}

#[derive(Deserialize)]
pub struct RollbackReq {
    pub path: String,
    pub snapshot_id: String,
}

#[derive(Deserialize)]
pub struct SnapshotsQuery {
    pub path: String,
}

#[derive(Serialize)]
pub struct ApiResponse<T> {
    pub success: bool,
    pub message: String,
    pub data: Option<T>,
}

#[derive(Deserialize)]
pub struct SimulateReq {
    pub path: String,
}

#[derive(Serialize)]
pub struct SimulateResult {
    pub total_files: usize,
    pub source_files: usize,
    pub source_bytes: u64,
    pub ephemeral_files: usize,
    pub ephemeral_bytes: u64,
    pub transient_files: usize,
    pub sample_sources: Vec<String>,
    pub sample_ephemerals: Vec<String>,
}

#[derive(Deserialize)]
pub struct VaultQuery {
    pub project_path: Option<String>,
}

#[derive(Serialize)]
pub struct VaultItem {
    pub file_name: String,
    pub original_name: String,
    pub deleted_at: String,
    pub size_bytes: u64,
    pub full_path: String,
    pub project_path: String,
}

#[derive(Deserialize)]
pub struct VaultRestoreReq {
    pub file_path: String,
    pub target_project_path: String,
}

#[derive(Deserialize)]
pub struct UpdateConfigReq {
    pub max_cache_gb: u64,
    pub debounce_ms: u64,
    pub auto_snapshot_on_sync: bool,
    pub remote_drive_base_path: Option<String>,
    pub backup_env_files: Option<bool>,
    pub quarantine_days: Option<u64>,
    pub include_git: Option<bool>,
    pub auto_sync_idle_minutes: Option<u64>,
    pub battery_guard_enabled: Option<bool>,
    pub auto_sync_enabled: Option<bool>,
    pub auto_discover_enabled: Option<bool>,
    pub auto_slim_days: Option<u64>,
    pub auto_showcase_days: Option<u64>,
}

#[derive(Deserialize)]
pub struct PinProjectReq {
    pub id: i64,
    pub pinned: bool,
}

#[derive(Deserialize)]
pub struct CleanArtifactsReq {
    pub id: i64,
    pub dry_run: Option<bool>,
}

#[derive(Deserialize)]
pub struct OffloadProjectReq {
    pub id: i64,
    pub dry_run: Option<bool>,
    pub yes: Option<bool>,
    pub backup_env: Option<bool>,
    pub ignore_env_warning: Option<bool>,
}

#[derive(Deserialize)]
pub struct UnarchiveProjectReq {
    pub id: i64,
    pub rebuild: Option<bool>,
}

#[derive(Deserialize)]
pub struct ArchiveVerifyReq {
    pub id: i64,
}

#[derive(Deserialize)]
pub struct PurgeQuarantineReq {
    pub id: i64,
    pub force: Option<bool>,
}

#[derive(Deserialize)]
pub struct RevealReq {
    pub path: String,
}

#[derive(Deserialize)]
pub struct PickFolderReq {
    pub prompt: Option<String>,
}

#[derive(Serialize)]
pub struct PickFolderResp {
    pub path: String,
    pub name: String,
}

#[derive(Deserialize)]
pub struct TestDriveReq {
    pub path: Option<String>,
}

#[derive(Serialize)]
pub struct TestDriveResp {
    pub latency_ms: u128,
    pub path: String,
    pub writable: bool,
}

#[derive(Deserialize)]
pub struct RemoveProjectReq {
    pub id: i64,
}

const DASHBOARD_HTML: &str = include_str!("../assets/dashboard.html");

pub async fn start_ui_server(port: u16, auto_open: bool) -> Result<()> {
    let db_path = Database::default_path();
    let config = Arc::new(std::sync::RwLock::new(DevDriveConfig::load()?));
    let sync_progress = Arc::new(Mutex::new(HashMap::new()));
    let state = AppState::new(
        db_path,
        config,
        sync_progress,
    );

    let app = Router::new()
        .route("/", get(|| async { Html(DASHBOARD_HTML) }))
        .route("/logo.svg", get(|| async { ([(axum::http::header::CONTENT_TYPE, "image/svg+xml")], include_str!("../assets/logo.svg")) }))
        .route("/api/status", get(get_status))
        .route("/api/init", post(init_project_handler))
        .route("/api/sync", post(sync_project_handler))
        .route("/api/sync-all", post(sync_all_projects_handler))
        .route("/api/sync-cancel", post(cancel_sync_handler))
        .route("/api/snapshot", post(snapshot_handler))
        .route("/api/snapshots", get(get_snapshots_handler))
        .route("/api/rollback", post(rollback_handler))
        .route("/api/cache-clean", post(cache_clean_handler))
        .route("/api/simulate", post(simulate_handler))
        .route("/api/vault", get(get_vault_handler))
        .route("/api/vault/restore", post(restore_vault_handler))
        .route("/api/config", get(get_config_handler).post(update_config_handler))
        .route("/api/reveal", post(reveal_in_finder_handler))
        .route("/api/pick-folder", post(pick_folder_handler))
        .route("/api/drive/accounts", get(get_drive_accounts_handler))
        .route("/api/drive/test", post(test_drive_handler))
        .route("/api/project/remove", post(remove_project_handler))
        .route("/api/project/remove-all", post(remove_all_projects_handler))
        .route("/api/project/clean-remote", post(clean_remote_project_handler))
        .route("/api/project/clean-artifacts", post(clean_artifacts_handler))
        .route("/api/project/offload", post(offload_project_handler))
        .route("/api/project/unarchive", post(unarchive_project_handler))
        .route("/api/project/archive-verify", post(archive_verify_handler))
        .route("/api/project/purge", post(purge_quarantine_handler))
        .route("/api/project/purge-expired", post(purge_expired_quarantine_handler))
        .route("/api/project/pin", post(pin_project_handler))
        .route("/api/discover", post(auto_discover_handler))
        .route("/api/snapshots/diff", get(get_snapshot_diff_handler))
        .route("/api/remote-projects", get(get_remote_projects_handler))
        .route("/api/restore", post(restore_project_handler))
        .route("/api/project/tree", get(get_project_tree_handler))
        .route("/api/project/export-subfolder", post(export_subfolder_handler))
        .with_state(state.clone());


    let addr = SocketAddr::from(([127, 0, 0, 1], port));
    let listener = match tokio::net::TcpListener::bind(addr).await {
        Ok(l) => l,
        Err(e) if e.kind() == std::io::ErrorKind::AddrInUse => {
            println!("\nℹ️ DevDrive UI zaten {} portunda çalışıyor. Tarayıcı açılıyor...", port);
            if auto_open {
                #[cfg(target_os = "macos")]
                {
                    let url = format!("http://127.0.0.1:{}", port);
                    let _ = std::process::Command::new("open").arg(&url).spawn();
                }
            }
            return Ok(());
        }
        Err(e) => return Err(e.into()),
    };
    let url = format!("http://127.0.0.1:{}", port);

    println!("\n🚀 DevDrive Dashboard başlatıldı: {}", url);

    if auto_open {
        #[cfg(target_os = "macos")]
        {
            let _ = std::process::Command::new("open").arg(&url).spawn();
        }
    }

    // Pre-warm metrics cache in the background on startup
    let bg_state = state.clone();
    std::thread::spawn(move || {
        let cfg = bg_state.get_config();
        let cache_dir = CacheManager::default_cache_dir();
        if let Ok(cache_mgr) = CacheManager::new(&cache_dir, cfg.max_cache_bytes())
            && let Ok(usage) = cache_mgr.current_cache_usage() {
                bg_state.cached_cache_usage.store(usage, std::sync::atomic::Ordering::Relaxed);
            }
        if let Ok(db) = Database::open(&bg_state.db_path)
            && let Ok(projects) = db.list_projects() {
                for p in projects {
                    let m = compute_project_metrics(&p, &cfg);
                    bg_state.metrics_cache.lock().unwrap().insert(p.id, m);
                }
            }
    });

    axum::serve(listener, app).await?;
    Ok(())
}

pub fn compute_project_metrics(p: &crate::db::ProjectRecord, _cfg: &DevDriveConfig) -> CachedProjectMetrics {
    let p_path = PathBuf::from(&p.local_path);
    let remote_path = PathBuf::from(&p.remote_path);
    let filter = crate::filter::DevFilter::for_project(&p_path);

    let mut reclaimable = 0u64;
    if p.status == "active" && p_path.exists()
        && let Ok(bytes) = crate::offload::OffloadEngine::measure_reclaimable_bytes(&p_path) {
            reclaimable = bytes;
        }

    let mut remote_c = 0;
    let mut remote_bytes = 0u64;
    let mut remote_files: HashMap<PathBuf, (u64, Option<std::time::SystemTime>)> = HashMap::new();

    if let Some(manifest) = crate::sync::read_bundle_manifest(&remote_path) {
        remote_c = manifest.total_files;
        remote_bytes = manifest.total_bytes;
        for entry in manifest.entries {
            let rel = PathBuf::from(&entry.path);
            let mtime = std::time::UNIX_EPOCH + std::time::Duration::from_secs(entry.mtime as u64);
            remote_files.insert(rel, (entry.size_bytes, Some(mtime)));
        }
    } else if remote_path.exists() {
        for entry in walkdir::WalkDir::new(&remote_path)
            .into_iter()
            .filter_entry(|e| {
                if e.file_type().is_dir() {
                    let is_root = e.depth() == 1;
                    if e.depth() > 0 && filter.should_skip_dir_entry(e.path(), is_root) {
                        return false;
                    }
                    if let Ok(rel) = e.path().strip_prefix(&remote_path)
                        && filter.categorize(rel) == crate::filter::FileCategory::Ephemeral {
                            return false;
                        }
                    true
                } else {
                    true
                }
            })
            .filter_map(|e| e.ok())
        {
            if entry.file_type().is_file()
                && let Ok(rel) = entry.path().strip_prefix(&remote_path) {
                    let name = entry.file_name().to_string_lossy();
                    if name != ".devdrive-meta.json"
                        && !name.starts_with(".conflict_")
                        && !entry.path().components().any(|c| c.as_os_str() == ".trash")
                        && filter.categorize(rel) == crate::filter::FileCategory::Source
                    {
                        remote_c += 1;
                        if let Ok(m) = entry.metadata() {
                            remote_bytes += m.len();
                            remote_files.insert(rel.to_path_buf(), (m.len(), m.modified().ok()));
                        } else {
                            remote_files.insert(rel.to_path_buf(), (0, None));
                        }
                    }
                }
        }
    }

    let is_showcase = p.status == "archived" || p.status == "purged" || crate::offload::is_showcase_dir(&p_path);
    let mut local_c = 0;
    let mut modified_files_count = 0;
    let mut latest_project_mtime: Option<std::time::SystemTime> = std::fs::metadata(&p_path).ok().and_then(|m| m.modified().ok());

    if !is_showcase && p_path.exists() {
        for entry in walkdir::WalkDir::new(&p_path)
            .into_iter()
            .filter_entry(|e| {
                if e.file_type().is_dir() {
                    let is_root = e.depth() == 1;
                    if e.depth() > 0 && filter.should_skip_dir_entry(e.path(), is_root) {
                        return false;
                    }
                    if let Ok(rel) = e.path().strip_prefix(&p_path)
                        && filter.categorize(rel) == crate::filter::FileCategory::Ephemeral {
                            return false;
                        }
                    true
                } else {
                    true
                }
            })
            .filter_map(|e| e.ok())
        {
            if entry.file_type().is_file()
                && let Ok(rel) = entry.path().strip_prefix(&p_path)
                && filter.categorize(rel) == crate::filter::FileCategory::Source
            {
                local_c += 1;
                match remote_files.get(rel) {
                    None => {
                        modified_files_count += 1;
                    }
                    Some(&(remote_len, remote_mtime)) => {
                        if let Ok(meta_local) = entry.metadata()
                            && let Ok(mtime_local) = meta_local.modified()
                        {
                            if latest_project_mtime.map(|prev| mtime_local > prev).unwrap_or(true) {
                                latest_project_mtime = Some(mtime_local);
                            }
                            if meta_local.len() != remote_len {
                                modified_files_count += 1;
                            } else if let Some(mtime_remote) = remote_mtime
                                && mtime_local > mtime_remote
                                && mtime_local.duration_since(mtime_remote).map(|d| d.as_secs() > 1).unwrap_or(false)
                            {
                                modified_files_count += 1;
                            }
                        }
                    }
                }
            }
        }
    }

    let now_utc = chrono::Utc::now();
    let inactive_days: u64 = if let Some(latest) = latest_project_mtime {
        std::time::SystemTime::now()
            .duration_since(latest)
            .unwrap_or_default()
            .as_secs() / 86400
    } else if let Some(ref dt_str) = p.last_synced_at
        && let Ok(parsed) = chrono::DateTime::parse_from_rfc3339(dt_str) {
            (now_utc - parsed.with_timezone(&chrono::Utc)).num_days().max(0) as u64
    } else if let Ok(parsed) = chrono::DateTime::parse_from_rfc3339(&p.created_at) {
        (now_utc - parsed.with_timezone(&chrono::Utc)).num_days().max(0) as u64
    } else {
        0
    };

    let tech_badges = detect_tech_badges(&p_path);

    CachedProjectMetrics {
        local_file_count: local_c,
        remote_file_count: remote_c,
        remote_used_bytes: remote_bytes,
        reclaimable_bytes: reclaimable,
        modified_files_count,
        inactive_days,
        is_showcase,
        tech_badges,
        updated_at: std::time::Instant::now(),
    }
}

async fn get_status(State(state): State<AppState>) -> impl IntoResponse {
    let cfg = state.get_config();
    let current_cache = state.cached_cache_usage.load(std::sync::atomic::Ordering::Relaxed);

    let mut projects_dto = Vec::new();
    let mut total_space_saved: u64 = 0;
    let mut total_reclaimable: u64 = 0;
    let mut remote_used_bytes: u64 = 0;

    let now_utc = chrono::Utc::now();
    let mut need_refresh = false;
    let remote_base_exists = Path::new(&cfg.remote_drive_base_path).exists();

    if let Ok(db) = Database::open(&state.db_path) {
        let mut projects = db.list_projects().unwrap_or_default();
        if cfg.auto_discover_enabled && projects.is_empty() {
            let _ = crate::watcher::discover_projects_once(&state.db_path, &cfg);
            projects = db.list_projects().unwrap_or_default();
        }

        let mut wal_counts: HashMap<i64, usize> = HashMap::new();
        if let Ok(events) = db.get_all_pending_wal_events() {
            for ev in events {
                *wal_counts.entry(ev.project_id).or_insert(0) += 1;
            }
        }

        let cache_guard = state.metrics_cache.lock().unwrap();

        for p in &projects {
            let p_path = PathBuf::from(&p.local_path);

            total_space_saved += p.freed_bytes as u64;

            let quarantine_days_left = p.quarantine_until.as_ref().and_then(|u| {
                chrono::DateTime::parse_from_rfc3339(u).ok().map(|until| {
                    let diff = until.with_timezone(&chrono::Utc) - now_utc;
                    diff.num_days().max(0)
                })
            });

            let cached = cache_guard.get(&p.id).cloned();
            if cached.is_none() || cached.as_ref().map(|c| c.updated_at.elapsed().as_secs() > 60).unwrap_or(false) {
                need_refresh = true;
            }

            let (local_c, remote_c, r_bytes, proj_reclaimable, mod_count, inact_days, is_showcase, tech_badges) = match cached {
                Some(ref m) => (
                    m.local_file_count,
                    m.remote_file_count,
                    m.remote_used_bytes,
                    m.reclaimable_bytes,
                    m.modified_files_count,
                    m.inactive_days,
                    m.is_showcase,
                    m.tech_badges.clone(),
                ),
                None => {
                    let is_sc = p.status == "archived" || p.status == "purged";
                    (0, 0, 0, 0, 0, 0, is_sc, Vec::new())
                }
            };

            total_reclaimable += proj_reclaimable;
            remote_used_bytes += r_bytes;

            let pending_wal_count = wal_counts.get(&p.id).copied().unwrap_or(0);

            let (sync_status, status_message) = if is_showcase {
                let msg = if p.quarantine_path.is_some() {
                    format!("Karantinada ({} gün kaldı)", quarantine_days_left.unwrap_or(7))
                } else {
                    "👻 Vitrin Modunda (Bulutta Uyuyor)".to_string()
                };
                ("archived".to_string(), msg)
            } else if p.status == "archiving" {
                ("archiving".to_string(), "Arşivleniyor...".to_string())
            } else if local_c == 0 && !p_path.exists() {
                ("missing_local".to_string(), "Yerel proje dizini bulunamadı".to_string())
            } else if !remote_base_exists {
                ("missing_remote".to_string(), "Google Drive bağlantısı koptu veya erişilemiyor".to_string())
            } else if remote_c == 0 {
                ("needs_sync".to_string(), "Drive'da yedek bulunamadı (Klasör boş) — Eşitleyin".to_string())
            } else if local_c > remote_c {
                ("needs_sync".to_string(), format!("Eşitleme gerekiyor (Yerel: {} / Drive: {} dosya)", local_c, remote_c))
            } else if mod_count > 0 || pending_wal_count > 0 {
                let count = mod_count.max(pending_wal_count);
                ("needs_sync".to_string(), format!("Değişiklik var ({} dosya eşitlenmeli)", count))
            } else {
                let sync_time_str = if let Some(ref dt) = p.last_synced_at {
                    if let Ok(parsed) = chrono::DateTime::parse_from_rfc3339(dt) {
                        format!(" • Son eşitleme: {}", parsed.with_timezone(&chrono::Local).format("%H:%M"))
                    } else {
                        String::new()
                    }
                } else {
                    String::new()
                };
                ("synced".to_string(), format!("Tam korumada (Drive'da {} dosya güncel{})", remote_c, sync_time_str))
            };

            let lifecycle_badge = if is_showcase {
                "👻 Bulut Vitrininde".to_string()
            } else if p.pinned {
                "📌 Sabitli (Muaf)".to_string()
            } else if inact_days >= cfg.auto_showcase_days {
                "⏳ Vitrine Alınacak".to_string()
            } else if inact_days >= cfg.auto_slim_days {
                format!("🧹 Çöp Temizliği Yapıldı • Vitrine {}g", cfg.auto_showcase_days.saturating_sub(inact_days))
            } else {
                format!("⚡ Aktif ({}g önce) • Vitrine {}g", inact_days, cfg.auto_showcase_days.saturating_sub(inact_days))
            };

            let active_prog = state.sync_progress.lock().unwrap().get(&p.id).cloned();
            let (sync_status, status_message) = if let Some(ref prog) = active_prog {
                (
                    "syncing".to_string(),
                    format!("Drive'a aktarılıyor: {} / {} dosya (%{})", prog.processed_files, prog.total_files, prog.percent),
                )
            } else {
                (sync_status, status_message)
            };

            projects_dto.push(ProjectDto {
                id: p.id,
                name: p.name.clone(),
                local_path: p.local_path.clone(),
                remote_path: p.remote_path.clone(),
                created_at: p.created_at.clone(),
                last_synced_at: p.last_synced_at.clone(),
                sync_status,
                status_message,
                remote_file_count: remote_c,
                local_file_count: local_c,
                progress: active_prog,
                project_status: p.status.clone(),
                archived_at: p.archived_at.clone(),
                archive_remote_path: p.archive_remote_path.clone(),
                quarantine_path: p.quarantine_path.clone(),
                quarantine_until: p.quarantine_until.clone(),
                quarantine_days_left,
                freed_bytes: p.freed_bytes,
                reclaimable_bytes: proj_reclaimable,
                tech_badges,
                pinned: p.pinned,
                is_showcase,
                inactive_days: inact_days,
                lifecycle_badge,
            });
        }
    }

    if need_refresh && !state.metrics_refreshing.swap(true, std::sync::atomic::Ordering::SeqCst) {
        let state_clone = state.clone();
        std::thread::spawn(move || {
            let cfg = state_clone.get_config();
            let cache_dir = CacheManager::default_cache_dir();
            if let Ok(cache_mgr) = CacheManager::new(&cache_dir, cfg.max_cache_bytes())
                && let Ok(usage) = cache_mgr.current_cache_usage() {
                    state_clone.cached_cache_usage.store(usage, std::sync::atomic::Ordering::Relaxed);
                }
            if let Ok(db) = Database::open(&state_clone.db_path)
                && let Ok(projects) = db.list_projects() {
                    for p in projects {
                        let should_calc = {
                            let cache = state_clone.metrics_cache.lock().unwrap();
                            match cache.get(&p.id) {
                                Some(m) => m.updated_at.elapsed().as_secs() > 60,
                                None => true,
                            }
                        };
                        if should_calc {
                            let m = compute_project_metrics(&p, &cfg);
                            state_clone.metrics_cache.lock().unwrap().insert(p.id, m);
                        }
                    }
                }
            state_clone.metrics_refreshing.store(false, std::sync::atomic::Ordering::SeqCst);
        });
    }

    let bulk_job = state.bulk_job.lock().unwrap().clone();

    Json(StatusResponse {
        current_cache_bytes: current_cache,
        max_cache_bytes: cfg.max_cache_bytes(),
        remote_drive_path: cfg.remote_drive_base_path,
        projects: projects_dto,
        total_space_saved_bytes: total_space_saved,
        total_reclaimable_bytes: total_reclaimable,
        remote_used_bytes,
        bulk_job,
    })
}

async fn cancel_sync_handler(State(state): State<AppState>) -> impl IntoResponse {
    state.cancel_flag.store(true, std::sync::atomic::Ordering::SeqCst);
    *state.bulk_job.lock().unwrap() = None;
    state.sync_progress.lock().unwrap().clear();
    (
        StatusCode::OK,
        Json(ApiResponse::<()> {
            success: true,
            message: "İşlem kullanıcı tarafından durduruldu".to_string(),
            data: None,
        }),
    )
}

async fn init_project_handler(
    State(state): State<AppState>,
    Json(payload): Json<InitProjectReq>,
) -> impl IntoResponse {
    let cfg = state.get_config();
    let local_path = PathBuf::from(&payload.path);
    let canonical = match local_path.canonicalize() {
        Ok(c) => c,
        Err(_) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(ApiResponse::<()> {
                    success: false,
                    message: "Geçersiz dizin yolu".into(),
                    data: None,
                }),
            )
        }
    };

    let name = payload.name.unwrap_or_else(|| {
        canonical
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_else(|| "project".to_string())
    });

    // Project name validation
    if let Err(e) = crate::sync::validate_project_name(&name) {
        return (
            StatusCode::BAD_REQUEST,
            Json(ApiResponse::<()> {
                success: false,
                message: format!("Geçersiz veya güvensiz proje adı: {}", e),
                data: None,
            }),
        );
    }

    let remote_dest = PathBuf::from(&cfg.remote_drive_base_path).join(&name);
    match Database::open(&state.db_path) {
        Ok(mut db) => {
            // Prevent collision: check if remote_dest is already registered to a different local path
            if let Ok(Some(existing)) = db.get_project_by_remote_path(&remote_dest.to_string_lossy())
                && existing.local_path != canonical.to_string_lossy()
            {
                return (
                    StatusCode::BAD_REQUEST,
                    Json(ApiResponse::<()> {
                        success: false,
                        message: format!(
                            "Hedef uzak klasör ('{}') zaten başka bir projeye ('{}' -> '{}') atanmış! Lütfen farklı bir isim seçin.",
                            remote_dest.display(),
                            existing.name,
                            existing.local_path
                        ),
                        data: None,
                    }),
                );
            }

            // Check if remote folder already exists on disk and belongs to a different project
            if remote_dest.exists()
                && let Some(meta) = crate::sync::read_devdrive_meta(&remote_dest)
                && meta.original_local_path != canonical.to_string_lossy()
            {
                let is_same = db.get_project_by_local_path(&canonical.to_string_lossy())
                    .ok()
                    .flatten()
                    .map(|p| p.remote_path == remote_dest.to_string_lossy())
                    .unwrap_or(false);
                if !is_same {
                    return (
                        StatusCode::BAD_REQUEST,
                        Json(ApiResponse::<()> {
                            success: false,
                            message: format!(
                                "Hedef uzak klasör ('{}') diskte mevcut ve başka bir projeye ('{}' -> '{}') ait! Lütfen farklı bir isim seçin.",
                                remote_dest.display(),
                                meta.project_name,
                                meta.original_local_path
                            ),
                            data: None,
                        }),
                    );
                }
            }

            match db.register_project(
                &name,
                &canonical.to_string_lossy(),
                &remote_dest.to_string_lossy(),
            ) {
                Ok(_) => {
                    let _ = crate::sync::write_devdrive_meta(
                        &remote_dest,
                        &name,
                        &canonical.to_string_lossy(),
                    );
                    let db_path = state.db_path.clone();
                    let config = cfg.clone();
                    let canonical_clone = canonical.clone();
                    let remote_dest_clone = remote_dest.clone();

                    let sync_prog_arc = state.sync_progress.clone();
                    let project_id = match db.get_project_by_local_path(&canonical.to_string_lossy()) {
                        Ok(Some(p)) => p.id,
                        _ => 0,
                    };

                    tokio::task::spawn_blocking(move || {
                        let cache_dir = CacheManager::default_cache_dir();
                        if let (Ok(mut db), Ok(cache_mgr)) = (Database::open(&db_path), CacheManager::new(&cache_dir, config.max_cache_bytes())) {
                            let mut sync = SyncEngine::new(&mut db, &cache_mgr);
                            if let Ok(mut map) = sync_prog_arc.lock() {
                                map.insert(project_id, SyncProgressTracker {
                                    processed_files: 0,
                                    total_files: 1,
                                    percent: 50,
                                });
                            }
                            let sync_res = sync.sync_bundle(&canonical_clone, &remote_dest_clone, config.auto_snapshot_on_sync);
                            if let Err(e) = sync_res {
                                tracing::error!("Initial bundle sync failed for project {}: {}", project_id, e);
                            }
                            if let Ok(mut map) = sync_prog_arc.lock() {
                                map.remove(&project_id);
                            }
                        }
                    });

                    (
                        StatusCode::OK,
                        Json(ApiResponse::<()> {
                            success: true,
                            message: format!("'{}' projesi korumaya alındı!", name),
                            data: None,
                        }),
                    )
                }
                Err(e) => (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    Json(ApiResponse::<()> {
                        success: false,
                        message: e.to_string(),
                        data: None,
                    }),
                ),
            }
        }
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(ApiResponse::<()> {
                success: false,
                message: e.to_string(),
                data: None,
            }),
        ),
    }
}

async fn sync_project_handler(
    State(state): State<AppState>,
    Json(payload): Json<SyncProjectReq>,
) -> impl IntoResponse {
    let cfg = state.get_config();
    let local_path = PathBuf::from(&payload.path);
    let canonical = match local_path.canonicalize() {
        Ok(c) => c,
        Err(_) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(ApiResponse::<()> {
                    success: false,
                    message: "Geçersiz dizin yolu".into(),
                    data: None,
                }),
            )
        }
    };

    let cache_dir = CacheManager::default_cache_dir();
    let cache_mgr = match CacheManager::new(&cache_dir, cfg.max_cache_bytes()) {
        Ok(m) => m,
        Err(e) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(ApiResponse::<()> {
                    success: false,
                    message: format!("Önbellek yöneticisi başlatılamadı: {}", e),
                    data: None,
                }),
            );
        }
    };

    let mut db = match Database::open(&state.db_path) {
        Ok(d) => d,
        Err(e) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(ApiResponse::<()> {
                    success: false,
                    message: e.to_string(),
                    data: None,
                }),
            )
        }
    };

    let proj = match db.get_project_by_local_path(&canonical.to_string_lossy()) {
        Ok(Some(p)) => p,
        _ => {
            return (
                StatusCode::NOT_FOUND,
                Json(ApiResponse::<()> {
                    success: false,
                    message: "Proje bulunamadı, önce ekleyin".into(),
                    data: None,
                }),
            )
        }
    };

    let remote_path = PathBuf::from(&proj.remote_path);
    let project_id = proj.id;
    let proj_name_clone = proj.name.clone();
    let proj_clone = proj.clone();
    let sync_prog_arc = state.sync_progress.clone();
    let bulk_job_arc = state.bulk_job.clone();
    let cancel_flag = state.cancel_flag.clone();
    let metrics_cache_arc = state.metrics_cache.clone();
    let auto_snapshot = cfg.auto_snapshot_on_sync;

    cancel_flag.store(false, std::sync::atomic::Ordering::SeqCst);

    let res = match tokio::task::spawn_blocking(move || {
        let mut sync = SyncEngine::new(&mut db, &cache_mgr);
        if let Ok(mut map) = sync_prog_arc.lock() {
            map.insert(project_id, SyncProgressTracker {
                processed_files: 0,
                total_files: 1,
                percent: 0,
            });
        }
        *bulk_job_arc.lock().unwrap() = Some(BulkJobDto {
            active: true,
            total_projects: 1,
            completed_projects: 0,
            current_project_id: Some(project_id),
            current_project_name: proj_name_clone.clone(),
            current_file: String::new(),
            current_files_done: 0,
            current_files_total: 0,
            overall_percent: 0,
            status_text: format!("{} eşitleniyor...", proj_name_clone),
        });

        let sync_arc_cb = sync_prog_arc.clone();
        let bulk_arc_cb = bulk_job_arc.clone();
        let cancel_cb = cancel_flag.clone();
        let pname_cb = proj_name_clone.clone();

        let r = sync.sync_bundle_with_progress(&canonical, &remote_path, auto_snapshot, move |curr_file, done, total| {
            if cancel_cb.load(std::sync::atomic::Ordering::SeqCst) {
                return false;
            }
            let pct = if total > 0 { (done as f64 / total as f64 * 100.0) as u32 } else { 0 };
            if let Ok(mut map) = sync_arc_cb.lock() {
                map.insert(project_id, SyncProgressTracker {
                    processed_files: done,
                    total_files: total,
                    percent: pct,
                });
            }
            if let Ok(mut bj) = bulk_arc_cb.lock()
                && let Some(ref mut job) = *bj {
                    job.current_file = curr_file.to_string();
                    job.current_files_done = done;
                    job.current_files_total = total;
                    job.overall_percent = pct.min(100) as u8;
                    job.status_text = format!("{}: {} / {} dosya", pname_cb, done, total);
                }
            true
        });
        if let Ok(mut map) = sync_prog_arc.lock() {
            map.remove(&project_id);
        }
        *bulk_job_arc.lock().unwrap() = None;
        if r.is_ok() {
            let m = compute_project_metrics(&proj_clone, &cfg);
            metrics_cache_arc.lock().unwrap().insert(project_id, m);
        }
        r
    }).await {
        Ok(sync_result) => sync_result,
        Err(join_err) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(ApiResponse::<()> {
                    success: false,
                    message: format!("Senkronizasyon görevi başarısız: {}", join_err),
                    data: None,
                }),
            );
        }
    };

    match res {
        Ok(bundle_report) => {
            crate::send_system_notification(
                "Senkronizasyon Tamamlandı",
                &format!("'{}': {} dosya paketlendi.", proj.name, bundle_report.files_packaged),
            );
            (
                StatusCode::OK,
                Json(ApiResponse::<()> {
                    success: true,
                    message: format!(
                        "Bundle Eşitleme Tamamlandı: {} dosya paketlendi, arşiv boyutu: {}.",
                        bundle_report.files_packaged,
                        format_bytes(bundle_report.bundle_bytes)
                    ),
                    data: None,
                }),
            )
        }
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(ApiResponse::<()> {
                success: false,
                message: e.to_string(),
                data: None,
            }),
        ),
    }
}

async fn sync_all_projects_handler(
    State(state): State<AppState>,
) -> impl IntoResponse {
    let cfg = state.get_config();
    let _ = crate::watcher::discover_projects_once(&state.db_path, &cfg);

    let db_path = state.db_path.clone();
    let sync_prog_arc = state.sync_progress.clone();
    let bulk_job_arc = state.bulk_job.clone();
    let cancel_flag = state.cancel_flag.clone();
    let metrics_cache_arc = state.metrics_cache.clone();
    let auto_snapshot = cfg.auto_snapshot_on_sync;
    let max_cache_bytes = cfg.max_cache_bytes();

    cancel_flag.store(false, std::sync::atomic::Ordering::SeqCst);

    tokio::task::spawn_blocking(move || {
        let cache_dir = CacheManager::default_cache_dir();
        if let (Ok(mut db), Ok(cache_mgr)) = (
            Database::open(&db_path),
            CacheManager::new(&cache_dir, max_cache_bytes),
        )
            && let Ok(projects) = db.list_projects() {
                let mut targets: Vec<_> = projects
                    .into_iter()
                    .filter(|p| {
                        p.status == "active"
                            && !crate::offload::is_showcase_dir(Path::new(&p.local_path))
                            && Path::new(&p.local_path).exists()
                    })
                    .collect();

                // Sort Shortest-Job-First (küçükten büyüğe)
                {
                    let cache = metrics_cache_arc.lock().unwrap();
                    targets.sort_by_key(|p| {
                        cache.get(&p.id).map(|m| m.local_file_count).unwrap_or(usize::MAX)
                    });
                }

                let total_projects = targets.len();
                let mut sync = SyncEngine::new(&mut db, &cache_mgr);
                let mut completed_count = 0;

                *bulk_job_arc.lock().unwrap() = Some(BulkJobDto {
                    active: true,
                    total_projects,
                    completed_projects: 0,
                    current_project_id: None,
                    current_project_name: String::new(),
                    current_file: String::new(),
                    current_files_done: 0,
                    current_files_total: 0,
                    overall_percent: 0,
                    status_text: "Yedekleme başlatılıyor...".to_string(),
                });

                for (proj_idx, proj) in targets.into_iter().enumerate() {
                    if cancel_flag.load(std::sync::atomic::Ordering::SeqCst) {
                        tracing::info!("Toplu eşitleme kullanıcı isteğiyle durduruldu.");
                        break;
                    }

                    let local_path = PathBuf::from(&proj.local_path);
                    let remote_path = PathBuf::from(&proj.remote_path);
                    let pid = proj.id;
                    let pname = proj.name.clone();

                    {
                        let mut bj = bulk_job_arc.lock().unwrap();
                        if let Some(ref mut job) = *bj {
                            job.current_project_id = Some(pid);
                            job.current_project_name = pname.clone();
                            job.completed_projects = completed_count;
                            job.status_text = format!("{} eşitleniyor ({}/{})", pname, proj_idx + 1, total_projects);
                        }
                    }

                    let bulk_arc_cb = bulk_job_arc.clone();
                    let sync_arc_cb = sync_prog_arc.clone();
                    let cancel_cb = cancel_flag.clone();
                    let pname_cb = pname.clone();

                    let res = sync.sync_bundle_with_progress(
                        &local_path,
                        &remote_path,
                        auto_snapshot,
                        move |curr_file, done, total| {
                            if cancel_cb.load(std::sync::atomic::Ordering::SeqCst) {
                                return false;
                            }
                            let pct = if total > 0 { (done as f64 / total as f64 * 100.0) as u32 } else { 0 };
                            if let Ok(mut map) = sync_arc_cb.lock() {
                                map.insert(pid, SyncProgressTracker {
                                    processed_files: done,
                                    total_files: total,
                                    percent: pct,
                                });
                            }
                            if let Ok(mut bj) = bulk_arc_cb.lock()
                                && let Some(ref mut job) = *bj {
                                    job.current_file = curr_file.to_string();
                                    job.current_files_done = done;
                                    job.current_files_total = total;
                                    let proj_pct = if total > 0 { done as f64 / total as f64 } else { 0.0 };
                                    job.overall_percent = (((proj_idx as f64 + proj_pct) / total_projects.max(1) as f64) * 100.0).min(100.0) as u8;
                                    job.status_text = format!("{}: {} / {} dosya", pname_cb, done, total);
                                }
                            true
                        },
                    );

                    if let Ok(mut map) = sync_prog_arc.lock() {
                        map.remove(&pid);
                    }

                    if res.is_ok() {
                        completed_count += 1;
                        let cfg = DevDriveConfig::default();
                        let m = compute_project_metrics(&proj, &cfg);
                        metrics_cache_arc.lock().unwrap().insert(pid, m);
                    }
                }

                *bulk_job_arc.lock().unwrap() = None;
                sync_prog_arc.lock().unwrap().clear();

                if completed_count > 0 {
                    crate::send_system_notification(
                        "Toplu Eşitleme Tamamlandı",
                        &format!("{} aktif proje Google Drive'a başarıyla eşitlendi.", completed_count),
                    );
                }
            }
    });

    (
        StatusCode::OK,
        Json(ApiResponse::<()> {
            success: true,
            message: "Tüm aktif projeler arka planda Google Drive'a eşitleniyor.".to_string(),
            data: None,
        }),
    )
}

async fn get_remote_projects_handler(State(state): State<AppState>) -> impl IntoResponse {
    let base_path = PathBuf::from(&state.get_config().remote_drive_base_path);
    let res = match tokio::task::spawn_blocking(move || {
        crate::sync::list_remote_projects(&base_path)
    }).await {
        Ok(Ok(list)) => list,
        Ok(Err(e)) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(ApiResponse::<Vec<crate::sync::RemoteProjectSummary>> {
                    success: false,
                    message: format!("Uzak projeler listelenemedi: {}", e),
                    data: None,
                }),
            );
        }
        Err(join_err) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(ApiResponse::<Vec<crate::sync::RemoteProjectSummary>> {
                    success: false,
                    message: format!("Uzak projeler listelenirken hata: {}", join_err),
                    data: None,
                }),
            );
        }
    };

    (
        StatusCode::OK,
        Json(ApiResponse {
            success: true,
            message: "OK".into(),
            data: Some(res),
        }),
    )
}

async fn restore_project_handler(
    State(state): State<AppState>,
    Json(payload): Json<RestoreReq>,
) -> impl IntoResponse {
    let remote_name = payload.remote_name.trim();
    if remote_name.is_empty() {
        return (
            StatusCode::BAD_REQUEST,
            Json(ApiResponse::<crate::sync::RestoreReport> {
                success: false,
                message: "Uzak proje adı boş olamaz".into(),
                data: None,
            }),
        );
    }

    if let Err(e) = crate::sync::validate_project_name(remote_name) {
        return (
            StatusCode::BAD_REQUEST,
            Json(ApiResponse::<crate::sync::RestoreReport> {
                success: false,
                message: format!("Geçersiz uzak proje adı: {}", e),
                data: None,
            }),
        );
    }

    let cfg = state.get_config();
    let remote_base = PathBuf::from(&cfg.remote_drive_base_path);
    let remote_path = remote_base.join(remote_name);

    if !remote_path.exists() {
        return (
            StatusCode::NOT_FOUND,
            Json(ApiResponse::<crate::sync::RestoreReport> {
                success: false,
                message: format!("Uzak proje bulunamadı: {}", remote_path.display()),
                data: None,
            }),
        );
    }

    // Path traversal safety check
    if let (Ok(can_base), Ok(can_remote)) = (remote_base.canonicalize(), remote_path.canonicalize())
        && !can_remote.starts_with(&can_base)
    {
        return (
            StatusCode::BAD_REQUEST,
            Json(ApiResponse::<crate::sync::RestoreReport> {
                success: false,
                message: "Güvenlik İhlali: Path traversal tespit edildi".into(),
                data: None,
            }),
        );
    }

    let target_path_str = payload.target_path.trim().to_string();
    if target_path_str.is_empty() {
        return (
            StatusCode::BAD_REQUEST,
            Json(ApiResponse::<crate::sync::RestoreReport> {
                success: false,
                message: "Hedef yerel dizin yolu belirtilmelidir".into(),
                data: None,
            }),
        );
    }

    let mut target_path = PathBuf::from(&target_path_str);
    if target_path.is_dir() {
        let last_name = target_path.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
        if last_name != remote_name {
            target_path.push(remote_name);
        }
    }
    let force = payload.force.unwrap_or(false);
    let db_path = state.db_path.clone();
    let max_cache = cfg.max_cache_bytes();

    let res = match tokio::task::spawn_blocking(move || {
        if remote_path.join("bundle.tar.gz").exists() || remote_path.join("tree.tar.gz").exists() {
            crate::sync::restore_project_bundle(&remote_path, &target_path)
        } else {
            let cache_dir = CacheManager::default_cache_dir();
            let cache_mgr = CacheManager::new(&cache_dir, max_cache)?;
            let mut db = Database::open(&db_path)?;
            let mut sync = SyncEngine::new(&mut db, &cache_mgr);
            sync.restore_from_remote(&remote_path, &target_path, force)
        }
    }).await {
        Ok(r) => r,
        Err(join_err) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(ApiResponse::<crate::sync::RestoreReport> {
                    success: false,
                    message: format!("Geri getirme görevi başarısız: {}", join_err),
                    data: None,
                }),
            );
        }
    };

    match res {
        Ok(report) => (
            StatusCode::OK,
            Json(ApiResponse {
                success: true,
                message: format!("'{}' projesi başarıyla geri getirildi!", remote_name),
                data: Some(report),
            }),
        ),
        Err(e) => (
            StatusCode::BAD_REQUEST,
            Json(ApiResponse::<crate::sync::RestoreReport> {
                success: false,
                message: e.to_string(),
                data: None,
            }),
        ),
    }
}

async fn snapshot_handler(
    State(state): State<AppState>,
    Json(payload): Json<SnapshotReq>,
) -> impl IntoResponse {
    let local_path = match PathBuf::from(&payload.path).canonicalize() {
        Ok(c) => c,
        Err(_) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(ApiResponse::<()> {
                    success: false,
                    message: "Geçersiz dizin".into(),
                    data: None,
                }),
            )
        }
    };

    let cache_dir = CacheManager::default_cache_dir();
    let max_cache = state.get_config().max_cache_bytes();
    let db_path = state.db_path.clone();

    let res = tokio::task::spawn_blocking(move || -> Result<crate::snapshot::SnapshotManifest> {
        let cache_mgr = CacheManager::new(&cache_dir, max_cache)?;
        let mut db = Database::open(&db_path)?;
        let mut engine = SnapshotEngine::new(&mut db, &cache_mgr);
        engine.create_snapshot(&local_path, &payload.message)
    }).await;

    match res {
        Ok(Ok(manifest)) => (
            StatusCode::OK,
            Json(ApiResponse::<()> {
                success: true,
                message: format!("Snapshot alındı: {} ({} dosya)", manifest.id, manifest.files.len()),
                data: None,
            }),
        ),
        Ok(Err(e)) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(ApiResponse::<()> {
                success: false,
                message: e.to_string(),
                data: None,
            }),
        ),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(ApiResponse::<()> {
                success: false,
                message: e.to_string(),
                data: None,
            }),
        ),
    }
}

#[derive(Serialize)]
pub struct SnapshotDto {
    pub id: String,
    pub message: String,
    pub created_at: String,
}

async fn get_snapshots_handler(
    State(state): State<AppState>,
    Query(query): Query<SnapshotsQuery>,
) -> impl IntoResponse {
    let local_path = match PathBuf::from(&query.path).canonicalize() {
        Ok(c) => c,
        Err(_) => return (StatusCode::BAD_REQUEST, Json(ApiResponse { success: false, message: "Geçersiz yol".into(), data: None })),
    };

    let db = match Database::open(&state.db_path) {
        Ok(db) => db,
        Err(e) => return (StatusCode::INTERNAL_SERVER_ERROR, Json(ApiResponse { success: false, message: e.to_string(), data: None })),
    };

    if let Ok(Some(proj)) = db.get_project_by_local_path(&local_path.to_string_lossy())
        && let Ok(snaps) = db.list_snapshots(proj.id) {
            let dtos: Vec<SnapshotDto> = snaps.into_iter().map(|s| SnapshotDto {
                id: s.id,
                message: s.message,
                created_at: s.created_at,
            }).collect();
            return (StatusCode::OK, Json(ApiResponse { success: true, message: "OK".into(), data: Some(dtos) }));
        }

    (StatusCode::OK, Json(ApiResponse { success: true, message: "OK".into(), data: Some(vec![]) }))
}

async fn rollback_handler(
    State(state): State<AppState>,
    Json(payload): Json<RollbackReq>,
) -> impl IntoResponse {
    let local_path = match PathBuf::from(&payload.path).canonicalize() {
        Ok(c) => c,
        Err(_) => return (StatusCode::BAD_REQUEST, Json(ApiResponse::<()> { success: false, message: "Geçersiz yol".into(), data: None })),
    };

    let cache_dir = CacheManager::default_cache_dir();
    let max_cache = state.get_config().max_cache_bytes();
    let db_path = state.db_path.clone();
    let snapshot_id = payload.snapshot_id.clone();

    let res = tokio::task::spawn_blocking(move || -> Result<()> {
        let cache_mgr = CacheManager::new(&cache_dir, max_cache)?;
        let mut db = Database::open(&db_path)?;
        let mut engine = SnapshotEngine::new(&mut db, &cache_mgr);
        engine.rollback_to_snapshot(&local_path, &snapshot_id)
    }).await;

    match res {
        Ok(Ok(_)) => (StatusCode::OK, Json(ApiResponse::<()> { success: true, message: format!("'{}' sürümüne başarıyla geri sarıldı!", payload.snapshot_id), data: None })),
        Ok(Err(e)) => (StatusCode::INTERNAL_SERVER_ERROR, Json(ApiResponse::<()> { success: false, message: e.to_string(), data: None })),
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, Json(ApiResponse::<()> { success: false, message: e.to_string(), data: None })),
    }
}

async fn cache_clean_handler(State(state): State<AppState>) -> impl IntoResponse {
    let cfg = state.get_config();
    let cache_dir = CacheManager::default_cache_dir();
    let cache_mgr = match CacheManager::new(&cache_dir, cfg.max_cache_bytes()) {
        Ok(m) => m,
        Err(e) => return (StatusCode::INTERNAL_SERVER_ERROR, Json(ApiResponse::<()> { success: false, message: e.to_string(), data: None })),
    };
    let usage = cache_mgr.current_cache_usage().unwrap_or(0);
    let max = cfg.max_cache_bytes();

    if usage > max {
        let freed = cache_mgr.evict_lru().unwrap_or(0);
        (StatusCode::OK, Json(ApiResponse::<()> { success: true, message: format!("Önbellek temizliği yapıldı ({} serbest bırakıldı).", freed), data: None }))
    } else {
        (StatusCode::OK, Json(ApiResponse::<()> { success: true, message: "Önbellek zaten optimal düzeyde.".into(), data: None }))
    }
}

async fn simulate_handler(Json(payload): Json<SimulateReq>) -> impl IntoResponse {
    let root = match PathBuf::from(&payload.path).canonicalize() {
        Ok(c) => c,
        Err(_) => return (StatusCode::BAD_REQUEST, Json(ApiResponse { success: false, message: "Dizin bulunamadı veya geçersiz yol.".into(), data: None })),
    };

    let res = tokio::task::spawn_blocking(move || {
        let filter = crate::filter::DevFilter::for_project(&root);
        let mut total_files = 0;
        let mut source_files = 0;
        let mut source_bytes = 0;
        let mut ephemeral_files = 0;
        let mut ephemeral_bytes = 0;
        let mut transient_files = 0;
        let mut sample_sources = Vec::new();
        let mut sample_ephemerals = Vec::new();

        for entry in walkdir::WalkDir::new(&root).into_iter().filter_map(|e| e.ok()) {
            if entry.file_type().is_file() {
                total_files += 1;
                let rel = match entry.path().strip_prefix(&root) {
                    Ok(r) => r,
                    Err(_) => continue,
                };
                let len = entry.metadata().map(|m| m.len()).unwrap_or(0);
                match filter.categorize(rel) {
                    crate::filter::FileCategory::Source => {
                        source_files += 1;
                        source_bytes += len;
                        if sample_sources.len() < 25 {
                            sample_sources.push(rel.to_string_lossy().to_string());
                        }
                    }
                    crate::filter::FileCategory::Ephemeral => {
                        ephemeral_files += 1;
                        ephemeral_bytes += len;
                        if sample_ephemerals.len() < 25 {
                            sample_ephemerals.push(rel.to_string_lossy().to_string());
                        }
                    }
                    crate::filter::FileCategory::TransientLock => {
                        transient_files += 1;
                    }
                }
            }
        }

        SimulateResult {
            total_files,
            source_files,
            source_bytes,
            ephemeral_files,
            ephemeral_bytes,
            transient_files,
            sample_sources,
            sample_ephemerals,
        }
    }).await;

    match res {
        Ok(sim_res) => (StatusCode::OK, Json(ApiResponse { success: true, message: "Simülasyon tamamlandı".into(), data: Some(sim_res) })),
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, Json(ApiResponse { success: false, message: e.to_string(), data: None })),
    }
}

async fn get_vault_handler(
    State(state): State<AppState>,
    Query(query): Query<VaultQuery>,
) -> impl IntoResponse {
    let db = match Database::open(&state.db_path) {
        Ok(d) => d,
        Err(e) => return (StatusCode::INTERNAL_SERVER_ERROR, Json(ApiResponse { success: false, message: e.to_string(), data: None })),
    };

    let projects = match db.list_projects() {
        Ok(p) => p,
        Err(e) => return (StatusCode::INTERNAL_SERVER_ERROR, Json(ApiResponse { success: false, message: e.to_string(), data: None })),
    };

    let mut items = Vec::new();
    for p in projects {
        if let Some(ref target) = query.project_path
            && &p.local_path != target {
                continue;
            }
        let trash_dir = PathBuf::from(&p.remote_path).join(".trash");
        if trash_dir.exists()
            && let Ok(entries) = std::fs::read_dir(&trash_dir) {
                for entry in entries.filter_map(|e| e.ok()) {
                    if let Ok(meta) = entry.metadata() {
                        let fname = entry.file_name().to_string_lossy().to_string();
                        let (deleted_at, orig_name) = if let Some(idx) = fname.find('_') {
                            let ts_str = &fname[..idx];
                            let name_part = &fname[idx + 1..];
                            if let Ok(ts) = ts_str.parse::<i64>() {
                                if let Some(dt) = chrono::DateTime::from_timestamp(ts, 0) {
                                    (dt.with_timezone(&chrono::Local).format("%Y-%m-%d %H:%M:%S").to_string(), name_part.to_string())
                                } else {
                                    ("-".to_string(), fname.clone())
                                }
                            } else {
                                ("-".to_string(), fname.clone())
                            }
                        } else {
                            ("-".to_string(), fname.clone())
                        };

                        items.push(VaultItem {
                            file_name: fname,
                            original_name: orig_name,
                            deleted_at,
                            size_bytes: meta.len(),
                            full_path: entry.path().to_string_lossy().to_string(),
                            project_path: p.local_path.clone(),
                        });
                    }
                }
            }
    }

    (StatusCode::OK, Json(ApiResponse { success: true, message: "OK".into(), data: Some(items) }))
}

async fn restore_vault_handler(Json(payload): Json<VaultRestoreReq>) -> impl IntoResponse {
    let src = PathBuf::from(&payload.file_path);
    if !src.exists() {
        return (StatusCode::NOT_FOUND, Json(ApiResponse::<()> { success: false, message: "Kasa dosyası bulunamadı".into(), data: None }));
    }

    let target_dir = match PathBuf::from(&payload.target_project_path).canonicalize() {
        Ok(c) => c,
        Err(_) => return (StatusCode::BAD_REQUEST, Json(ApiResponse::<()> { success: false, message: "Hedef proje klasörü bulunamadı veya geçersiz".into(), data: None })),
    };

    let fname = src.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
    let clean_name = if let Some(idx) = fname.find('_') {
        &fname[idx + 1..]
    } else {
        &fname
    };

    // Path traversal shield on vault restore filename
    if clean_name.contains("..") || clean_name.contains('/') || clean_name.contains('\\') || clean_name.trim().is_empty() {
        return (StatusCode::BAD_REQUEST, Json(ApiResponse::<()> { success: false, message: "Geçersiz veya güvensiz dosya adı (path traversal engellendi)".into(), data: None }));
    }

    let dest = target_dir.join(clean_name);
    if !dest.starts_with(&target_dir) {
        return (StatusCode::BAD_REQUEST, Json(ApiResponse::<()> { success: false, message: "Hedef proje sınırları aşıldı".into(), data: None }));
    }

    match std::fs::copy(&src, &dest) {
        Ok(_) => (StatusCode::OK, Json(ApiResponse::<()> { success: true, message: format!("'{}' dosyası projeye geri yüklendi!", clean_name), data: None })),
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, Json(ApiResponse::<()> { success: false, message: e.to_string(), data: None })),
    }
}

async fn get_config_handler(State(state): State<AppState>) -> impl IntoResponse {
    let cfg = state.get_config();
    (StatusCode::OK, Json(ApiResponse { success: true, message: "OK".into(), data: Some(cfg) }))
}

async fn update_config_handler(
    State(state): State<AppState>,
    Json(payload): Json<UpdateConfigReq>,
) -> impl IntoResponse {
    let mut cfg = state.get_config();
    cfg.max_cache_gb = payload.max_cache_gb;
    cfg.debounce_ms = payload.debounce_ms;
    cfg.auto_snapshot_on_sync = payload.auto_snapshot_on_sync;
    if let Some(r) = payload.remote_drive_base_path
        && !r.trim().is_empty() {
            cfg.remote_drive_base_path = r;
        }
    if let Some(be) = payload.backup_env_files {
        cfg.backup_env_files = be;
    }
    if let Some(qd) = payload.quarantine_days {
        cfg.quarantine_days = qd;
    }
    if let Some(ig) = payload.include_git {
        cfg.include_git = ig;
    }
    if let Some(idle) = payload.auto_sync_idle_minutes {
        cfg.auto_sync_idle_minutes = idle;
    }
    if let Some(bg) = payload.battery_guard_enabled {
        cfg.battery_guard_enabled = bg;
    }
    if let Some(ase) = payload.auto_sync_enabled {
        cfg.auto_sync_enabled = ase;
    }
    if let Some(ade) = payload.auto_discover_enabled {
        cfg.auto_discover_enabled = ade;
    }
    if let Some(asd) = payload.auto_slim_days {
        cfg.auto_slim_days = asd;
    }
    if let Some(ashd) = payload.auto_showcase_days {
        cfg.auto_showcase_days = ashd;
    }
    match cfg.save() {
        Ok(_) => {
            if let Ok(mut lock) = state.config.write() {
                *lock = cfg;
            }
            (StatusCode::OK, Json(ApiResponse::<()> { success: true, message: "Yapılandırma başarıyla kaydedildi!".into(), data: None }))
        }
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, Json(ApiResponse::<()> { success: false, message: e.to_string(), data: None })),
    }
}

async fn pin_project_handler(
    State(state): State<AppState>,
    Json(payload): Json<PinProjectReq>,
) -> impl IntoResponse {
    let mut db = match Database::open(&state.db_path) {
        Ok(d) => d,
        Err(e) => return (StatusCode::INTERNAL_SERVER_ERROR, Json(ApiResponse::<()> { success: false, message: e.to_string(), data: None })),
    };
    match db.set_project_pinned(payload.id, payload.pinned) {
        Ok(_) => {
            let msg = if payload.pinned {
                "Proje otomatik temizlikten muaf tutuldu (sabitlendi)."
            } else {
                "Proje sabitlemesi kaldırıldı."
            };
            (StatusCode::OK, Json(ApiResponse::<()> { success: true, message: msg.into(), data: None }))
        }
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, Json(ApiResponse::<()> { success: false, message: e.to_string(), data: None })),
    }
}

async fn auto_discover_handler(
    State(state): State<AppState>,
) -> impl IntoResponse {
    let cfg = state.get_config();
    match crate::watcher::discover_projects_once(&state.db_path, &cfg) {
        Ok(count) => {
            let msg = format!("Otomatik keşif tamamlandı: {} yeni proje DevDrive'a eklendi.", count);
            (StatusCode::OK, Json(ApiResponse::<usize> { success: true, message: msg, data: Some(count) }))
        }
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, Json(ApiResponse::<usize> { success: false, message: e.to_string(), data: None })),
    }
}

async fn reveal_in_finder_handler(Json(payload): Json<RevealReq>) -> impl IntoResponse {
    let p = PathBuf::from(&payload.path);
    if p.exists() {
        #[cfg(target_os = "macos")]
        {
            let _ = std::process::Command::new("open").arg("-R").arg(&p).spawn();
        }
        (StatusCode::OK, Json(ApiResponse::<()> { success: true, message: "Finder'da gösterildi".into(), data: None }))
    } else {
        (StatusCode::NOT_FOUND, Json(ApiResponse::<()> { success: false, message: "Yol bulunamadı".into(), data: None }))
    }
}

async fn pick_folder_handler(Json(payload): Json<PickFolderReq>) -> impl IntoResponse {
    let prompt = payload.prompt.unwrap_or_else(|| "DevDrive için Klasör Seçin:".to_string());

    #[cfg(target_os = "macos")]
    {
        let script = "on run argv\ntell application \"System Events\"\nactivate\nset f to choose folder with prompt (item 1 of argv)\nreturn POSIX path of f\nend tell\nend run";
        let output = std::process::Command::new("osascript")
            .arg("-e")
            .arg(script)
            .arg(&prompt)
            .output();

        match output {
            Ok(out) if out.status.success() => {
                let raw_path = String::from_utf8_lossy(&out.stdout).trim().to_string();
                if raw_path.is_empty() {
                    return (StatusCode::BAD_REQUEST, Json(ApiResponse { success: false, message: "Klasör seçilmedi".into(), data: None }));
                }
                let p = PathBuf::from(&raw_path);
                let name = p.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
                (StatusCode::OK, Json(ApiResponse {
                    success: true,
                    message: "Klasör başarıyla seçildi".into(),
                    data: Some(PickFolderResp { path: raw_path, name }),
                }))
            }
            Ok(_) => {
                (StatusCode::OK, Json(ApiResponse { success: false, message: "Klasör seçimi iptal edildi".into(), data: None }))
            }
            Err(e) => {
                (StatusCode::INTERNAL_SERVER_ERROR, Json(ApiResponse { success: false, message: e.to_string(), data: None }))
            }
        }
    }

    #[cfg(not(target_os = "macos"))]
    {
        (StatusCode::NOT_IMPLEMENTED, Json(ApiResponse { success: false, message: "Sadece macOS üzerinde destekleniyor".into(), data: None }))
    }
}

async fn get_drive_accounts_handler() -> impl IntoResponse {
    let accounts = DevDriveConfig::detect_google_drive_accounts();
    (StatusCode::OK, Json(ApiResponse {
        success: true,
        message: "OK".into(),
        data: Some(accounts),
    }))
}

async fn test_drive_handler(
    State(state): State<AppState>,
    Json(payload): Json<TestDriveReq>,
) -> impl IntoResponse {
    let target = payload.path.unwrap_or_else(|| state.get_config().remote_drive_base_path);
    let target_path = PathBuf::from(&target);

    let start = std::time::Instant::now();

    if let Err(e) = std::fs::create_dir_all(&target_path) {
        return (StatusCode::OK, Json(ApiResponse {
            success: false,
            message: format!("Hedef klasör oluşturulamadı veya erişim reddedildi: {}", e),
            data: None,
        }));
    }

    let probe_file = target_path.join(".devdrive_probe");
    let test_data = b"DevDrive Health Probe";
    if let Err(e) = std::fs::write(&probe_file, test_data) {
        return (StatusCode::OK, Json(ApiResponse {
            success: false,
            message: format!("Google Drive'a yazma izni başarısız: {}", e),
            data: None,
        }));
    }

    match std::fs::read(&probe_file) {
        Ok(read_data) if read_data == test_data => {
            let _ = std::fs::remove_file(&probe_file);
            let elapsed = start.elapsed().as_millis();
            (StatusCode::OK, Json(ApiResponse {
                success: true,
                message: format!("Google Drive bağlantısı ve yazma izinleri doğrulandı ({} ms) ✓", elapsed),
                data: Some(TestDriveResp {
                    latency_ms: elapsed,
                    path: target,
                    writable: true,
                }),
            }))
        }
        _ => {
            let _ = std::fs::remove_file(&probe_file);
            (StatusCode::OK, Json(ApiResponse {
                success: false,
                message: "Yazılan test verisi okunamadı".into(),
                data: None,
            }))
        }
    }
}

async fn remove_project_handler(
    State(state): State<AppState>,
    Json(payload): Json<RemoveProjectReq>,
) -> impl IntoResponse {
    match Database::open(&state.db_path) {
        Ok(mut db) => {
            if db.unregister_project(payload.id).is_ok() {
                (StatusCode::OK, Json(ApiResponse::<()> { success: true, message: "Proje DevDrive korumasından kaldırıldı".into(), data: None }))
            } else {
                (StatusCode::INTERNAL_SERVER_ERROR, Json(ApiResponse::<()> { success: false, message: "Proje veritabanından silinemedi".into(), data: None }))
            }
        }
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, Json(ApiResponse::<()> { success: false, message: e.to_string(), data: None })),
    }
}

#[derive(Deserialize)]
pub struct CleanRemoteReq {
    pub id: i64,
}

async fn clean_remote_project_handler(
    State(state): State<AppState>,
    Json(payload): Json<CleanRemoteReq>,
) -> impl IntoResponse {
    match Database::open(&state.db_path) {
        Ok(db) => {
            if let Ok(projects) = db.list_projects()
                && let Some(proj) = projects.into_iter().find(|p| p.id == payload.id) {
                    let remote_base_raw = PathBuf::from(&state.get_config().remote_drive_base_path);
                    let remote_base = remote_base_raw.canonicalize().unwrap_or(remote_base_raw);

                    let remote_path_raw = PathBuf::from(&proj.remote_path);
                    let remote_path = remote_path_raw.canonicalize().unwrap_or(remote_path_raw);

                    // Safety Shield: remote_path MUST be strictly inside remote_base and not equal to remote_base itself
                    if !remote_path.starts_with(&remote_base) || remote_path == remote_base {
                        return (
                            StatusCode::FORBIDDEN,
                            Json(ApiResponse::<()> {
                                success: false,
                                message: "Güvenlik Engeli: Silinmek istenen yol izin verilen Google Drive kök dizininin dışında!".into(),
                                data: None,
                            }),
                        );
                    }

                    if remote_path.exists() {
                        let _ = tokio::task::spawn_blocking(move || {
                            std::fs::remove_dir_all(&remote_path)
                        }).await;
                    }
                    return (
                        StatusCode::OK,
                        Json(ApiResponse::<()> {
                            success: true,
                            message: format!("'{}' projesinin Google Drive yedeği temizlendi (Drive sıfırlandı).", proj.name),
                            data: None,
                        }),
                    );
                }
            (
                StatusCode::NOT_FOUND,
                Json(ApiResponse::<()> {
                    success: false,
                    message: "Proje bulunamadı".into(),
                    data: None,
                }),
            )
        }
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(ApiResponse::<()> {
                success: false,
                message: e.to_string(),
                data: None,
            }),
        ),
    }
}

async fn remove_all_projects_handler(
    State(state): State<AppState>,
) -> impl IntoResponse {
    match Database::open(&state.db_path) {
        Ok(mut db) => {
            match db.unregister_all_projects() {
                Ok(count) => (
                    StatusCode::OK,
                    Json(ApiResponse::<()> {
                        success: true,
                        message: format!("Tüm projeler ({}) DevDrive takibinden kaldırıldı.", count),
                        data: None,
                    }),
                ),
                Err(e) => (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    Json(ApiResponse::<()> {
                        success: false,
                        message: e.to_string(),
                        data: None,
                    }),
                ),
            }
        }
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(ApiResponse::<()> {
                success: false,
                message: e.to_string(),
                data: None,
            }),
        ),
    }
}

async fn clean_artifacts_handler(
    State(state): State<AppState>,
    Json(payload): Json<CleanArtifactsReq>,
) -> impl IntoResponse {
    let db = match Database::open(&state.db_path) {
        Ok(db) => db,
        Err(e) => return (StatusCode::INTERNAL_SERVER_ERROR, Json(ApiResponse { success: false, message: e.to_string(), data: None })),
    };
    let proj = match db.get_project_by_id(payload.id) {
        Ok(Some(p)) => p,
        Ok(None) => return (StatusCode::NOT_FOUND, Json(ApiResponse { success: false, message: "Proje bulunamadı".into(), data: None })),
        Err(e) => return (StatusCode::INTERNAL_SERVER_ERROR, Json(ApiResponse { success: false, message: e.to_string(), data: None })),
    };

    let p_path = PathBuf::from(&proj.local_path);
    let dry_run = payload.dry_run.unwrap_or(false);
    match crate::offload::OffloadEngine::clean_artifacts(&p_path, dry_run) {
        Ok(rep) => (StatusCode::OK, Json(ApiResponse { success: true, message: format!("{} bayt temizlendi", rep.total_freed_bytes), data: Some(rep) })),
        Err(e) => (StatusCode::BAD_REQUEST, Json(ApiResponse { success: false, message: e.to_string(), data: None })),
    }
}

async fn offload_project_handler(
    State(state): State<AppState>,
    Json(payload): Json<OffloadProjectReq>,
) -> impl IntoResponse {
    let mut db = match Database::open(&state.db_path) {
        Ok(db) => db,
        Err(e) => return (StatusCode::INTERNAL_SERVER_ERROR, Json(ApiResponse { success: false, message: e.to_string(), data: None })),
    };

    let mut cfg = state.get_config();
    if let Some(be) = payload.backup_env {
        cfg.backup_env_files = be;
    }
    let dry_run = payload.dry_run.unwrap_or(false) || !payload.yes.unwrap_or(false);
    let bypass_env = payload.ignore_env_warning.unwrap_or(false);

    match crate::offload::OffloadEngine::offload(&mut db, payload.id, &cfg, dry_run, bypass_env) {
        Ok(rep) => {
            if !dry_run {
                crate::send_system_notification(
                    "Tahliye Tamamlandı",
                    &format!("'{}' projesi arşivlendi. {} SSD alanı açıldı.", rep.project_name, rep.class_a_freed_bytes),
                );
            }
            (StatusCode::OK, Json(ApiResponse { success: true, message: "Tahliye işlemi başarılı".into(), data: Some(rep) }))
        }
        Err(e) => (StatusCode::BAD_REQUEST, Json(ApiResponse { success: false, message: e.to_string(), data: None })),
    }
}

async fn unarchive_project_handler(
    State(state): State<AppState>,
    Json(payload): Json<UnarchiveProjectReq>,
) -> impl IntoResponse {
    let mut db = match Database::open(&state.db_path) {
        Ok(db) => db,
        Err(e) => return (StatusCode::INTERNAL_SERVER_ERROR, Json(ApiResponse { success: false, message: e.to_string(), data: None })),
    };

    let cfg = state.get_config();
    let rebuild = payload.rebuild.unwrap_or(false);
    match crate::offload::OffloadEngine::unarchive(&mut db, payload.id, &cfg, rebuild) {
        Ok(rep) => {
            crate::send_system_notification(
                "Geri Yükleme Tamamlandı",
                &format!("'{}' projesi SSD'ye geri getirildi.", rep.project_name),
            );
            (StatusCode::OK, Json(ApiResponse { success: true, message: "Proje başarıyla geri yüklendi".into(), data: Some(rep) }))
        }
        Err(e) => (StatusCode::BAD_REQUEST, Json(ApiResponse { success: false, message: e.to_string(), data: None })),
    }
}

async fn archive_verify_handler(
    State(state): State<AppState>,
    Json(payload): Json<ArchiveVerifyReq>,
) -> impl IntoResponse {
    let db = match Database::open(&state.db_path) {
        Ok(db) => db,
        Err(e) => return (StatusCode::INTERNAL_SERVER_ERROR, Json(ApiResponse { success: false, message: e.to_string(), data: None })),
    };
    let proj = match db.get_project_by_id(payload.id) {
        Ok(Some(p)) => p,
        Ok(None) => return (StatusCode::NOT_FOUND, Json(ApiResponse { success: false, message: "Proje bulunamadı".into(), data: None })),
        Err(e) => return (StatusCode::INTERNAL_SERVER_ERROR, Json(ApiResponse { success: false, message: e.to_string(), data: None })),
    };

    let arch_path = match proj.archive_remote_path {
        Some(p) => PathBuf::from(p),
        None => return (StatusCode::BAD_REQUEST, Json(ApiResponse { success: false, message: "Kayıtlı arşiv yolu bulunamadı".into(), data: None })),
    };

    match crate::offload::OffloadEngine::archive_verify(&arch_path) {
        Ok(rep) => (StatusCode::OK, Json(ApiResponse { success: rep.is_valid, message: if rep.is_valid { "Arşiv doğrulandı".into() } else { "Arşiv doğrulanamadı".into() }, data: Some(rep) })),
        Err(e) => (StatusCode::BAD_REQUEST, Json(ApiResponse { success: false, message: e.to_string(), data: None })),
    }
}

async fn purge_quarantine_handler(
    State(state): State<AppState>,
    Json(payload): Json<PurgeQuarantineReq>,
) -> impl IntoResponse {
    let mut db = match Database::open(&state.db_path) {
        Ok(db) => db,
        Err(e) => return (StatusCode::INTERNAL_SERVER_ERROR, Json(ApiResponse { success: false, message: e.to_string(), data: None })),
    };

    let force = payload.force.unwrap_or(false);
    match crate::offload::OffloadEngine::purge(&mut db, payload.id, force) {
        Ok(rep) => (StatusCode::OK, Json(ApiResponse { success: true, message: format!("Karantina kalıcı olarak silindi. Açılan alan: {} bayt", rep.freed_bytes), data: Some(rep) })),
        Err(e) => (StatusCode::BAD_REQUEST, Json(ApiResponse { success: false, message: e.to_string(), data: None })),
    }
}

#[derive(Deserialize)]
struct PurgeExpiredReq {
    force: Option<bool>,
}

async fn purge_expired_quarantine_handler(
    State(state): State<AppState>,
    Json(payload): Json<PurgeExpiredReq>,
) -> impl IntoResponse {
    let mut db = match Database::open(&state.db_path) {
        Ok(db) => db,
        Err(e) => return (StatusCode::INTERNAL_SERVER_ERROR, Json(ApiResponse { success: false, message: e.to_string(), data: None })),
    };

    let force = payload.force.unwrap_or(false);
    match crate::offload::OffloadEngine::purge_expired(&mut db, force) {
        Ok(reports) => {
            let total_freed: u64 = reports.iter().map(|r| r.freed_bytes).sum();
            (
                StatusCode::OK,
                Json(ApiResponse {
                    success: true,
                    message: format!("{} projenin karantinası temizlendi. Toplam açılan alan: {} bayt", reports.len(), total_freed),
                    data: Some(reports),
                }),
            )
        }
        Err(e) => (StatusCode::BAD_REQUEST, Json(ApiResponse { success: false, message: e.to_string(), data: None })),
    }
}

#[derive(Deserialize)]
struct DiffQuery {
    snapshot_id: String,
    project_id: i64,
}

async fn get_snapshot_diff_handler(
    State(state): State<AppState>,
    axum::extract::Query(query): axum::extract::Query<DiffQuery>,
) -> impl IntoResponse {
    let mut db = match Database::open(&state.db_path) {
        Ok(db) => db,
        Err(e) => return (StatusCode::INTERNAL_SERVER_ERROR, Json(ApiResponse { success: false, message: e.to_string(), data: None })),
    };

    let proj = match db.get_project_by_id(query.project_id) {
        Ok(Some(p)) => p,
        Ok(None) => return (StatusCode::NOT_FOUND, Json(ApiResponse { success: false, message: "Proje bulunamadı".into(), data: None })),
        Err(e) => return (StatusCode::INTERNAL_SERVER_ERROR, Json(ApiResponse { success: false, message: e.to_string(), data: None })),
    };

    let cache_dir = CacheManager::default_cache_dir();
    let cache_mgr = match CacheManager::new(&cache_dir, state.get_config().max_cache_bytes()) {
        Ok(c) => c,
        Err(e) => return (StatusCode::INTERNAL_SERVER_ERROR, Json(ApiResponse { success: false, message: e.to_string(), data: None })),
    };

    let mut engine = crate::snapshot::SnapshotEngine::new(&mut db, &cache_mgr);
    match engine.diff_snapshot(Path::new(&proj.local_path), &query.snapshot_id) {
        Ok(report) => (StatusCode::OK, Json(ApiResponse { success: true, message: "OK".into(), data: Some(report) })),
        Err(e) => (StatusCode::BAD_REQUEST, Json(ApiResponse { success: false, message: e.to_string(), data: None })),
    }
}

pub fn detect_tech_badges(project_root: &Path) -> Vec<String> {
    let mut badges = Vec::new();
    if !project_root.exists() {
        return badges;
    }

    let mut has_rust = false;
    let mut has_flutter = false;
    let mut has_node = false;
    let mut has_python = false;
    let mut has_go = false;

    for entry in walkdir::WalkDir::new(project_root)
        .max_depth(4)
        .into_iter()
        .filter_entry(|e| {
            let name = e.file_name().to_string_lossy();
            name != ".git" && name != "node_modules" && name != "target" && name != ".venv" && name != "build" && name != "dist"
        })
        .filter_map(|e| e.ok())
    {
        let name = entry.file_name().to_string_lossy();
        if name == "Cargo.toml" { has_rust = true; }
        else if name == "pubspec.yaml" { has_flutter = true; }
        else if name == "package.json" { has_node = true; }
        else if name == "pyproject.toml" || name == "requirements.txt" || name == "Pipfile" { has_python = true; }
        else if name == "go.mod" { has_go = true; }
    }

    if has_rust { badges.push("Rust".to_string()); }
    if has_flutter { badges.push("Flutter".to_string()); }
    if has_node { badges.push("Node.js".to_string()); }
    if has_python { badges.push("Python".to_string()); }
    if has_go { badges.push("Go".to_string()); }

    badges
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TreeNodeDto {
    pub name: String,
    pub path: String,
    pub is_dir: bool,
    pub size_bytes: u64,
    pub children: Vec<TreeNodeDto>,
}

#[derive(Deserialize)]
pub struct ProjectTreeQuery {
    pub project_id: i64,
}

#[derive(Serialize)]
pub struct ProjectTreeResponse {
    pub project_name: String,
    pub total_files: usize,
    pub total_bytes: u64,
    pub is_archived: bool,
    pub root: TreeNodeDto,
}

pub fn build_tree_from_paths(items: Vec<(String, u64)>) -> TreeNodeDto {
    use std::collections::BTreeMap;

    struct InternalNode {
        name: String,
        path: String,
        is_dir: bool,
        size_bytes: u64,
        children: BTreeMap<String, InternalNode>,
    }

    let mut root = InternalNode {
        name: "/".to_string(),
        path: "".to_string(),
        is_dir: true,
        size_bytes: 0,
        children: BTreeMap::new(),
    };

    for (rel_path, size) in items {
        let parts: Vec<&str> = rel_path.split('/').filter(|s| !s.is_empty()).collect();
        let mut curr = &mut root;
        curr.size_bytes += size;

        let mut accumulated_path = String::new();
        for (i, part) in parts.iter().enumerate() {
            if !accumulated_path.is_empty() {
                accumulated_path.push('/');
            }
            accumulated_path.push_str(part);

            let is_last = i == parts.len() - 1;
            curr = curr.children.entry(part.to_string()).or_insert_with(|| InternalNode {
                name: part.to_string(),
                path: accumulated_path.clone(),
                is_dir: !is_last,
                size_bytes: 0,
                children: BTreeMap::new(),
            });
            curr.size_bytes += size;
        }
    }

    fn to_dto(node: InternalNode) -> TreeNodeDto {
        let mut children: Vec<TreeNodeDto> = node.children.into_values().map(to_dto).collect();
        children.sort_by(|a, b| {
            b.is_dir.cmp(&a.is_dir).then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
        });
        TreeNodeDto {
            name: node.name,
            path: node.path,
            is_dir: node.is_dir,
            size_bytes: node.size_bytes,
            children,
        }
    }

    to_dto(root)
}

async fn get_project_tree_handler(
    State(state): State<AppState>,
    Query(query): Query<ProjectTreeQuery>,
) -> impl IntoResponse {
    let db = match Database::open(&state.db_path) {
        Ok(db) => db,
        Err(e) => return (StatusCode::INTERNAL_SERVER_ERROR, Json(ApiResponse { success: false, message: e.to_string(), data: None })),
    };

    let proj = match db.get_project_by_id(query.project_id) {
        Ok(Some(p)) => p,
        Ok(None) => return (StatusCode::NOT_FOUND, Json(ApiResponse { success: false, message: "Proje bulunamadı".into(), data: None })),
        Err(e) => return (StatusCode::INTERNAL_SERVER_ERROR, Json(ApiResponse { success: false, message: e.to_string(), data: None })),
    };

    let local_path = PathBuf::from(&proj.local_path);
    let remote_path = PathBuf::from(&proj.remote_path);
    let is_archived = proj.status == "archived" || proj.status == "purged";

    let mut file_items: Vec<(String, u64)> = Vec::new();
    let mut manifest_loaded = false;

    // 1. Check archive or quarantine manifest if archived
    let candidate_manifest = if is_archived {
        if let Some(ref arch_path) = proj.archive_remote_path {
            let p = PathBuf::from(arch_path).join("manifest.json");
            if p.exists() {
                Some(p)
            } else {
                None
            }
        } else if let Some(ref q_path) = proj.quarantine_path {
            let p = PathBuf::from(q_path).join(".devdrive_manifest.json");
            if p.exists() {
                Some(p)
            } else {
                None
            }
        } else {
            None
        }
    } else {
        None
    };

    // 2. Check local or remote manifest.json for bundle-synced or normal projects
    let manifest_path = candidate_manifest.or_else(|| {
        let p_local = local_path.join("manifest.json");
        if p_local.exists() {
            Some(p_local)
        } else {
            let p_remote = remote_path.join("manifest.json");
            if p_remote.exists() {
                Some(p_remote)
            } else {
                None
            }
        }
    });

    if let Some(ref m_path) = manifest_path
        && let Ok(content) = std::fs::read_to_string(m_path) {
            #[derive(serde::Deserialize)]
            struct ManifestEntryQuick {
                path: String,
                #[serde(default)]
                size_bytes: u64,
            }
            #[derive(serde::Deserialize)]
            struct ManifestQuick {
                #[serde(default)]
                entries: Vec<ManifestEntryQuick>,
            }

            if let Ok(manifest) = serde_json::from_str::<ManifestQuick>(&content)
                && !manifest.entries.is_empty() {
                    for entry in manifest.entries {
                        file_items.push((entry.path, entry.size_bytes));
                    }
                    manifest_loaded = true;
                }
        }

    if !manifest_loaded && local_path.exists() {
        let filter = crate::filter::DevFilter::for_project(&local_path);
        for entry in walkdir::WalkDir::new(&local_path)
            .into_iter()
            .filter_entry(|e| {
                if e.file_type().is_dir() {
                    if let Ok(rel) = e.path().strip_prefix(&local_path)
                        && filter.categorize(rel) == crate::filter::FileCategory::Ephemeral {
                            return false;
                        }
                    let name = e.file_name().to_string_lossy();
                    let is_root = e.depth() == 1;
                    let parent_name = e.path().parent().and_then(|p| p.file_name()).and_then(|n| n.to_str());
                    !filter.should_skip_dir_with_parent(&name, is_root, parent_name)
                } else {
                    true
                }
            })
            .filter_map(|e| e.ok())
        {
            if entry.file_type().is_file()
                && let Ok(rel) = entry.path().strip_prefix(&local_path)
                    && filter.categorize(rel) == crate::filter::FileCategory::Source {
                        let size = entry.metadata().map(|m| m.len()).unwrap_or(0);
                        file_items.push((rel.to_string_lossy().to_string(), size));
                    }
        }
    }

    let total_files = file_items.len();
    let total_bytes: u64 = file_items.iter().map(|(_, s)| *s).sum();
    let root_node = build_tree_from_paths(file_items);

    (
        StatusCode::OK,
        Json(ApiResponse {
            success: true,
            message: "OK".into(),
            data: Some(ProjectTreeResponse {
                project_name: proj.name,
                total_files,
                total_bytes,
                is_archived,
                root: root_node,
            }),
        }),
    )
}

#[derive(Deserialize)]
pub struct ExportSubfolderReq {
    pub project_id: i64,
    pub subfolder: String,
    pub target_path: String,
}

#[derive(Serialize)]
pub struct ExportSubfolderResponse {
    pub files_exported: usize,
    pub bytes_exported: u64,
    pub target_path: String,
}

async fn export_subfolder_handler(
    State(state): State<AppState>,
    Json(payload): Json<ExportSubfolderReq>,
) -> impl IntoResponse {
    let db = match Database::open(&state.db_path) {
        Ok(db) => db,
        Err(e) => return (StatusCode::INTERNAL_SERVER_ERROR, Json(ApiResponse { success: false, message: e.to_string(), data: None })),
    };

    let proj = match db.get_project_by_id(payload.project_id) {
        Ok(Some(p)) => p,
        Ok(None) => return (StatusCode::NOT_FOUND, Json(ApiResponse { success: false, message: "Proje bulunamadı".into(), data: None })),
        Err(e) => return (StatusCode::INTERNAL_SERVER_ERROR, Json(ApiResponse { success: false, message: e.to_string(), data: None })),
    };

    let target_dir = PathBuf::from(&payload.target_path);
    if let Err(e) = std::fs::create_dir_all(&target_dir) {
        return (StatusCode::BAD_REQUEST, Json(ApiResponse { success: false, message: format!("Hedef klasör oluşturulamadı: {}", e), data: None }));
    }

    let is_archived = proj.status == "archived" || proj.status == "purged";
    let subfolder = payload.subfolder.trim_matches('/').to_string();

    let mut files_exported = 0usize;
    let mut bytes_exported = 0u64;

    if is_archived {
        let arch_dir = if let Some(ref r) = proj.archive_remote_path {
            PathBuf::from(r)
        } else {
            return (StatusCode::BAD_REQUEST, Json(ApiResponse { success: false, message: "Arşiv dizini bulunamadı".into(), data: None }));
        };

        let tar_gz = if arch_dir.join("bundle.tar.gz").exists() {
            arch_dir.join("bundle.tar.gz")
        } else {
            arch_dir.join("tree.tar.gz")
        };
        if !tar_gz.exists() {
            return (StatusCode::BAD_REQUEST, Json(ApiResponse { success: false, message: "Arşiv dosyası (bundle.tar.gz veya tree.tar.gz) bulunamadı".into(), data: None }));
        }

        let file = match std::fs::File::open(&tar_gz) {
            Ok(f) => f,
            Err(e) => return (StatusCode::INTERNAL_SERVER_ERROR, Json(ApiResponse { success: false, message: e.to_string(), data: None })),
        };
        let gz = flate2::read::GzDecoder::new(file);
        let mut archive = tar::Archive::new(gz);

        match archive.entries() {
            Ok(entries) => {
                for mut entry in entries.flatten() {
                    let path = match entry.path() {
                        Ok(p) => p.to_string_lossy().to_string(),
                        Err(_) => continue,
                    };

                    let should_extract = if subfolder.is_empty() {
                        true
                    } else {
                        path == subfolder || path.starts_with(&format!("{}/", subfolder))
                    };

                    if should_extract {
                        let dest_rel = if subfolder.is_empty() {
                            path.clone()
                        } else if path == subfolder {
                            PathBuf::from(&path).file_name().unwrap_or_default().to_string_lossy().to_string()
                        } else {
                            path.strip_prefix(&format!("{}/", subfolder)).unwrap_or(&path).to_string()
                        };

                        let out_path = target_dir.join(dest_rel);
                        if let Some(parent) = out_path.parent() {
                            let _ = std::fs::create_dir_all(parent);
                        }
                        if entry.header().entry_type().is_file()
                            && let Ok(_) = entry.unpack(&out_path) {
                                files_exported += 1;
                                bytes_exported += entry.size();
                            }
                    }
                }
            }
            Err(e) => return (StatusCode::INTERNAL_SERVER_ERROR, Json(ApiResponse { success: false, message: format!("Arşiv okuma hatası: {}", e), data: None })),
        }
    } else {
        let local_path = PathBuf::from(&proj.local_path);
        let filter = crate::filter::DevFilter::for_project(&local_path);
        let search_path = if subfolder.is_empty() {
            local_path.clone()
        } else {
            local_path.join(&subfolder)
        };

        if !search_path.exists() {
            return (StatusCode::BAD_REQUEST, Json(ApiResponse { success: false, message: "İstenen alt klasör bulunamadı".into(), data: None }));
        }

        for entry in walkdir::WalkDir::new(&search_path).into_iter().filter_map(|e| e.ok()) {
            if entry.file_type().is_file()
                && let Ok(rel) = entry.path().strip_prefix(&local_path)
                    && filter.categorize(rel) == crate::filter::FileCategory::Source {
                        let dest_rel = if subfolder.is_empty() {
                            rel.to_path_buf()
                        } else {
                            match entry.path().strip_prefix(&search_path) {
                                Ok(r) => r.to_path_buf(),
                                Err(_) => continue,
                            }
                        };
                        let out_path = target_dir.join(dest_rel);
                        if let Some(parent) = out_path.parent() {
                            let _ = std::fs::create_dir_all(parent);
                        }
                        if let Ok(bytes) = std::fs::copy(entry.path(), &out_path) {
                            files_exported += 1;
                            bytes_exported += bytes;
                        }
                    }
        }
    }

    (
        StatusCode::OK,
        Json(ApiResponse {
            success: true,
            message: format!("{} dosya başarıyla çıkarıldı ({})", files_exported, target_dir.display()),
            data: Some(ExportSubfolderResponse {
                files_exported,
                bytes_exported,
                target_path: target_dir.to_string_lossy().to_string(),
            }),
        }),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_build_tree_from_paths() {
        let items = vec![
            ("src/main.rs".to_string(), 100),
            ("src/utils/math.rs".to_string(), 250),
            ("Cargo.toml".to_string(), 50),
            ("README.md".to_string(), 200),
        ];

        let tree = build_tree_from_paths(items);
        assert_eq!(tree.size_bytes, 600);
        assert_eq!(tree.children.len(), 3); // src (dir), Cargo.toml (file), README.md (file)
        
        let src_dir = tree.children.iter().find(|c| c.name == "src").unwrap();
        assert!(src_dir.is_dir);
        assert_eq!(src_dir.size_bytes, 350);
        assert_eq!(src_dir.children.len(), 2); // utils (dir), main.rs (file)
    }

    #[test]
    fn test_detect_tech_badges() {
        let temp = tempfile::tempdir().unwrap();
        std::fs::write(temp.path().join("Cargo.toml"), b"[package]\nname = \"demo\"").unwrap();
        
        let sub_web = temp.path().join("web");
        std::fs::create_dir_all(&sub_web).unwrap();
        std::fs::write(sub_web.join("package.json"), b"{\"name\": \"web\"}").unwrap();

        let sub_app = temp.path().join("mobile");
        std::fs::create_dir_all(&sub_app).unwrap();
        std::fs::write(sub_app.join("pubspec.yaml"), b"name: mobile").unwrap();

        let badges = detect_tech_badges(temp.path());
        assert!(badges.contains(&"Rust".to_string()));
        assert!(badges.contains(&"Node.js".to_string()));
        assert!(badges.contains(&"Flutter".to_string()));
    }

    #[tokio::test]
    async fn test_get_project_tree_reads_bundle_manifest() {
        let temp = tempfile::tempdir().unwrap();
        let db_path = temp.path().join("test.db");
        let local_dir = temp.path().join("local_proj");
        let remote_dir = temp.path().join("remote_proj");
        std::fs::create_dir_all(&local_dir).unwrap();
        std::fs::create_dir_all(&remote_dir).unwrap();

        // Write bundle manifest.json into remote_dir
        let manifest = crate::sync::BundleManifest {
            version: "1.0.0".to_string(),
            project_name: "test-bundle".to_string(),
            created_at: chrono::Utc::now().to_rfc3339(),
            total_files: 3,
            total_bytes: 550,
            entries: vec![
                crate::sync::BundleFileEntry {
                    path: "src/main.rs".to_string(),
                    size_bytes: 200,
                    mtime: 1000,
                    sha256: None,
                },
                crate::sync::BundleFileEntry {
                    path: "package.json".to_string(),
                    size_bytes: 150,
                    mtime: 1000,
                    sha256: None,
                },
                crate::sync::BundleFileEntry {
                    path: "assets/logo.png".to_string(),
                    size_bytes: 200,
                    mtime: 1000,
                    sha256: None,
                },
            ],
        };
        std::fs::write(
            remote_dir.join("manifest.json"),
            serde_json::to_string(&manifest).unwrap(),
        )
        .unwrap();

        // Register project in DB
        let proj = {
            let mut db = Database::open(&db_path).unwrap();
            db.register_project(
                "test-bundle",
                &local_dir.to_string_lossy(),
                &remote_dir.to_string_lossy(),
            )
            .unwrap()
        };

        let state = AppState::new(
            db_path,
            std::sync::Arc::new(std::sync::RwLock::new(
                crate::config::DevDriveConfig::default(),
            )),
            std::sync::Arc::new(std::sync::Mutex::new(std::collections::HashMap::new())),
        );

        let query = ProjectTreeQuery {
            project_id: proj.id,
        };

        let resp = get_project_tree_handler(State(state), Query(query)).await;
        let response = resp.into_response();
        assert_eq!(response.status(), StatusCode::OK);

        let body_bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let val: serde_json::Value = serde_json::from_slice(&body_bytes).unwrap();
        assert_eq!(val["success"], true);

        let data = &val["data"];
        assert_eq!(data["total_files"], 3);
        assert_eq!(data["total_bytes"], 550);
        assert_eq!(data["project_name"], "test-bundle");
        // Check root tree node contains 3 children: src, assets, package.json
        assert_eq!(data["root"]["children"].as_array().unwrap().len(), 3);
    }

    #[tokio::test]
    async fn test_get_status_and_sync_all_v2() {
        let temp_dir = tempfile::tempdir().unwrap();
        let db_path = temp_dir.path().join("devdrive.db");
        let local_dir = temp_dir.path().join("local_proj");
        std::fs::create_dir_all(&local_dir).unwrap();
        std::fs::write(local_dir.join("main.rs"), "fn main() {}").unwrap();

        let remote_dir = temp_dir.path().join("remote_proj");
        std::fs::create_dir_all(&remote_dir).unwrap();

        {
            let mut db = Database::open(&db_path).unwrap();
            db.register_project(
                "v2-proj",
                &local_dir.to_string_lossy(),
                &remote_dir.to_string_lossy(),
            )
            .unwrap();
        }

        let cfg = crate::config::DevDriveConfig {
            auto_discover_enabled: false,
            auto_discover_paths: vec![],
            remote_drive_base_path: temp_dir.path().join("drive").to_string_lossy().to_string(),
            ..Default::default()
        };

        let state = AppState::new(
            db_path,
            std::sync::Arc::new(std::sync::RwLock::new(cfg)),
            std::sync::Arc::new(std::sync::Mutex::new(std::collections::HashMap::new())),
        );

        // Test sync-all handler
        let resp = sync_all_projects_handler(State(state.clone())).await.into_response();
        assert_eq!(resp.status(), StatusCode::OK);

        // Test get_status returns V2 lifecycle fields
        let status_resp = get_status(State(state)).await.into_response();
        assert_eq!(status_resp.status(), StatusCode::OK);

        let body_bytes = axum::body::to_bytes(status_resp.into_body(), usize::MAX)
            .await
            .unwrap();
        let val: serde_json::Value = serde_json::from_slice(&body_bytes).unwrap();
        let projects = val["projects"].as_array().unwrap();
        assert_eq!(projects.len(), 1);
        assert!(projects[0]["lifecycle_badge"].is_string());
        assert!(projects[0]["inactive_days"].is_number());
    }
}


