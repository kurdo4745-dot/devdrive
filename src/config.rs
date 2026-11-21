use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct DevDriveConfig {
    pub remote_drive_base_path: String,
    pub max_cache_gb: u64,
    pub debounce_ms: u64,
    pub auto_snapshot_on_sync: bool,
    pub custom_ephemeral_dirs: Vec<String>,
    #[serde(default = "default_true")]
    pub backup_env_files: bool,
    #[serde(default = "default_quarantine_days")]
    pub quarantine_days: u64,
    #[serde(default = "default_quarantine_max_gb")]
    pub quarantine_max_gb: u64,
    #[serde(default = "default_true")]
    pub include_git: bool,
    #[serde(default)]
    pub quarantine_base_path: Option<String>,
    #[serde(default = "default_auto_sync_idle_minutes")]
    pub auto_sync_idle_minutes: u64,
    #[serde(default = "default_true")]
    pub battery_guard_enabled: bool,
    #[serde(default = "default_true")]
    pub auto_sync_enabled: bool,
    #[serde(default = "default_true")]
    pub auto_discover_enabled: bool,
    #[serde(default = "default_auto_discover_paths")]
    pub auto_discover_paths: Vec<String>,
    #[serde(default = "default_auto_slim_days")]
    pub auto_slim_days: u64,
    #[serde(default = "default_auto_showcase_days")]
    pub auto_showcase_days: u64,
    #[serde(default = "default_true")]
    pub use_direct_showcase: bool,
}

fn default_true() -> bool {
    true
}

fn default_auto_slim_days() -> u64 {
    7
}

fn default_auto_showcase_days() -> u64 {
    30
}

fn default_auto_discover_paths() -> Vec<String> {
    let home = dirs::home_dir().unwrap_or_else(|| PathBuf::from("."));
    vec![
        home.join("Desktop").to_string_lossy().to_string(),
        home.join("Documents").to_string_lossy().to_string(),
        home.join("Documents").join("antigravity").to_string_lossy().to_string(),
    ]
}

fn default_auto_sync_idle_minutes() -> u64 {
    30
}

fn default_quarantine_days() -> u64 {
    7
}

fn default_quarantine_max_gb() -> u64 {
    5
}

impl Default for DevDriveConfig {
    fn default() -> Self {
        let home = dirs::home_dir().unwrap_or_else(|| PathBuf::from("."));
        let mut detected_remote = home
            .join("Google Drive")
            .join("My Drive")
            .join("DevDrive");

        // Auto-detect modern macOS CloudStorage path (~/Library/CloudStorage/GoogleDrive-.../)
        let cloud_storage = home.join("Library").join("CloudStorage");
        if cloud_storage.exists()
            && let Ok(entries) = std::fs::read_dir(cloud_storage) {
                for entry in entries.filter_map(|e| e.ok()) {
                    let name = entry.file_name().to_string_lossy().to_string();
                    if name.starts_with("GoogleDrive-") {
                        let base = entry.path();
                        let turkish_drive = base.join("Drive'ım").join("DevDrive");
                        let english_drive = base.join("My Drive").join("DevDrive");

                        if base.join("Drive'ım").exists() {
                            detected_remote = turkish_drive;
                            break;
                        } else if base.join("My Drive").exists() {
                            detected_remote = english_drive;
                            break;
                        }
                    }
                }
            }

        Self {
            remote_drive_base_path: detected_remote.to_string_lossy().to_string(),
            max_cache_gb: 20,
            debounce_ms: 500,
            auto_snapshot_on_sync: true,
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
            auto_discover_paths: default_auto_discover_paths(),
            auto_slim_days: 7,
            auto_showcase_days: 30,
            use_direct_showcase: true,
        }
    }
}

impl DevDriveConfig {
    pub fn config_path() -> PathBuf {
        dirs::home_dir()
            .unwrap_or_else(|| PathBuf::from("."))
            .join(".config")
            .join("devdrive")
            .join("config.toml")
    }

    pub fn load() -> Result<Self> {
        let path = Self::config_path();
        if !path.exists() {
            let default_cfg = Self::default();
            default_cfg.save()?;
            return Ok(default_cfg);
        }

        let content = std::fs::read_to_string(&path)?;
        let config: Self = toml::from_str(&content)?;
        Ok(config)
    }

    pub fn save(&self) -> Result<()> {
        let path = Self::config_path();
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let content = toml::to_string_pretty(self)?;
        std::fs::write(path, content)?;
        Ok(())
    }

    pub fn max_cache_bytes(&self) -> u64 {
        self.max_cache_gb * 1024 * 1024 * 1024
    }

    pub fn detect_google_drive_accounts() -> Vec<DetectedDriveAccount> {
        let mut accounts = Vec::new();
        let home = dirs::home_dir().unwrap_or_else(|| PathBuf::from("."));
        let cloud_storage = home.join("Library").join("CloudStorage");
        if cloud_storage.exists()
            && let Ok(entries) = std::fs::read_dir(cloud_storage) {
                for entry in entries.filter_map(|e| e.ok()) {
                    let name = entry.file_name().to_string_lossy().to_string();
                    if name.starts_with("GoogleDrive-") {
                        let email = name.strip_prefix("GoogleDrive-").unwrap_or(&name).to_string();
                        let base = entry.path();
                        let turkish_drive = base.join("Drive'ım").join("DevDrive");
                        let english_drive = base.join("My Drive").join("DevDrive");

                        let target_path = if base.join("Drive'ım").exists() {
                            turkish_drive
                        } else if base.join("My Drive").exists() {
                            english_drive
                        } else {
                            base.join("DevDrive")
                        };

                        accounts.push(DetectedDriveAccount {
                            email,
                            path: target_path.to_string_lossy().to_string(),
                        });
                    }
                }
            }
        accounts
    }
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct DetectedDriveAccount {
    pub email: String,
    pub path: String,
}
