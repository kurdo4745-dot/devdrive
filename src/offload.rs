use anyhow::{bail, Context, Result};
use chrono::{DateTime, Duration, Utc};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use tracing::{info, warn};

use crate::cache::CacheManager;
use crate::config::DevDriveConfig;
use crate::db::Database;
use crate::filter::DevFilter;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ManifestEntryKind {
    File,
    Symlink,
    Directory,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ManifestEntry {
    pub path: String,
    pub kind: ManifestEntryKind,
    pub size_bytes: u64,
    pub sha256: String,
    pub mode: u32,
    pub mtime: i64,
    pub symlink_target: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OffloadManifest {
    pub version: String,
    pub project_name: String,
    pub original_local_path: String,
    pub archived_at: String,
    pub total_files: usize,
    pub total_bytes: u64,
    pub entries: Vec<ManifestEntry>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RecipeLockfile {
    pub path: String,
    pub sha256: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RecipeArtifact {
    pub path: String,
    pub size_bytes: u64,
    pub tool_or_rule: String,
    pub lockfiles: Vec<RecipeLockfile>,
    pub rebuild_command: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Recipe {
    pub version: String,
    pub project_name: String,
    pub artifacts: Vec<RecipeArtifact>,
    pub rebuild_commands: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PreflightReport {
    pub project_name: String,
    pub local_path: String,
    pub class_a_items: Vec<RecipeArtifact>,
    pub class_a_total_bytes: u64,
    pub class_b_items: Vec<ManifestEntry>,
    pub class_b_total_bytes: u64,
    pub class_c_blockers: Vec<String>,
    pub env_files_count: usize,
    pub env_backed_up: bool,
    pub can_proceed: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CleanArtifactsReport {
    pub project_name: String,
    pub local_path: String,
    pub dry_run: bool,
    pub cleaned_items: Vec<RecipeArtifact>,
    pub total_freed_bytes: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OffloadReport {
    pub project_id: i64,
    pub project_name: String,
    pub dry_run: bool,
    pub class_a_freed_bytes: u64,
    pub class_b_archived_bytes: u64,
    pub class_b_file_count: usize,
    pub archive_remote_path: String,
    pub quarantine_path: Option<String>,
    pub quarantine_until: Option<String>,
    pub manifest_sha256: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ArchiveVerificationReport {
    pub archive_path: String,
    pub manifest_sha256: String,
    pub total_entries: usize,
    pub verified_files: usize,
    pub verified_bytes: u64,
    pub is_valid: bool,
    pub errors: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UnarchiveReport {
    pub project_id: i64,
    pub project_name: String,
    pub local_path: String,
    pub restored_from_quarantine: bool,
    pub restored_from_archive: bool,
    pub files_restored: usize,
    pub bytes_restored: u64,
    pub rebuild_executed: bool,
    pub rebuild_output: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PurgeReport {
    pub project_id: i64,
    pub project_name: String,
    pub deleted_quarantine_path: String,
    pub freed_bytes: u64,
}

pub const SHOWCASE_META_FILE: &str = ".devdrive-showcase.json";
pub const SHOWCASE_HTML_FILE: &str = "📋 BULUT_VITRINI.html";
pub const SHOWCASE_WAKE_SCRIPT: &str = "Projeyi_Uyandir.command";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ShowcaseMeta {
    pub project_name: String,
    pub archived_at: String,
    pub archive_remote_path: String,
    pub total_files: usize,
    pub total_bytes: u64,
}

pub fn is_showcase_dir(path: &Path) -> bool {
    path.join(SHOWCASE_META_FILE).exists()
}

pub fn has_only_showcase_files(path: &Path) -> bool {
    if !path.exists() || !path.is_dir() {
        return false;
    }
    if let Ok(entries) = std::fs::read_dir(path) {
        for entry in entries.filter_map(|e| e.ok()) {
            let name = entry.file_name().to_string_lossy().to_string();
            if name != SHOWCASE_META_FILE
                && name != SHOWCASE_HTML_FILE
                && name != SHOWCASE_WAKE_SCRIPT
                && name != "⚡ Projeyi_Uyandir.command"
                && name != "README.md"
                && name != ".DS_Store"
            {
                return false;
            }
        }
        return true;
    }
    false
}

fn html_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&#39;")
}

pub fn write_showcase_files(
    local_path: &Path,
    manifest: &OffloadManifest,
    archive_remote_path: &Path,
) -> Result<()> {
    if !local_path.exists() {
        std::fs::create_dir_all(local_path)?;
    }

    // 1. .devdrive-showcase.json
    let meta = ShowcaseMeta {
        project_name: manifest.project_name.clone(),
        archived_at: manifest.archived_at.clone(),
        archive_remote_path: archive_remote_path.to_string_lossy().to_string(),
        total_files: manifest.total_files,
        total_bytes: manifest.total_bytes,
    };
    let meta_json = serde_json::to_string_pretty(&meta)?;
    std::fs::write(local_path.join(SHOWCASE_META_FILE), meta_json)?;

    // 2. Projeyi_Uyandir.command
    let script_content = r#"#!/bin/bash
echo "⚡ DevDrive: Proje Google Drive arşivinden uyandırılıyor..."
export PATH="$HOME/.cargo/bin:/usr/local/bin:$PATH"
devdrive unarchive "$(cd "$(dirname "$0")" && pwd)"
echo "✅ İşlem tamamlandı. Bu pencereyi kapatabilirsiniz."
"#;
    let script_path = local_path.join(SHOWCASE_WAKE_SCRIPT);
    std::fs::write(&script_path, script_content)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if let Ok(meta) = std::fs::metadata(&script_path) {
            let mut perms = meta.permissions();
            perms.set_mode(0o755);
            let _ = std::fs::set_permissions(&script_path, perms);
        }
    }

    // 3. 📋 BULUT_VITRINI.html
    let mut table_rows = String::new();
    for entry in &manifest.entries {
        let (icon, size_str) = match entry.kind {
            ManifestEntryKind::Directory => ("📁", "-".to_string()),
            ManifestEntryKind::Symlink => ("🔗", "symlink".to_string()),
            ManifestEntryKind::File => {
                let s = crate::format_bytes(entry.size_bytes);
                ("📄", s)
            }
        };
        let escaped_path = html_escape(&entry.path);
        table_rows.push_str(&format!(
            "<tr><td>{} {}</td><td class=\"file-size\">{}</td></tr>\n",
            icon, escaped_path, size_str
        ));
    }

    let human_total_bytes = crate::format_bytes(manifest.total_bytes);
    let html_content = format!(
        r#"<!DOCTYPE html>
<html lang="tr">
<head>
<meta charset="UTF-8">
<meta name="viewport" content="width=device-width, initial-scale=1.0">
<title>DevDrive Bulut Vitrini - {project_name}</title>
<style>
  :root {{
    --bg: #0b0f19;
    --card-bg: #151d2f;
    --border: #1e293b;
    --text: #f1f5f9;
    --text-muted: #94a3b8;
    --accent: #38bdf8;
    --accent-glow: rgba(56, 189, 248, 0.15);
    --emerald: #10b981;
    --font: -apple-system, BlinkMacSystemFont, "Segoe UI", Roboto, sans-serif;
    --mono: "SF Mono", Menlo, Monaco, Consolas, monospace;
  }}
  * {{ box-sizing: border-box; margin: 0; padding: 0; }}
  body {{ background: var(--bg); color: var(--text); font-family: var(--font); padding: 32px 20px; }}
  .container {{ max-width: 900px; margin: 0 auto; }}
  .badge {{ display: inline-flex; align-items: center; gap: 6px; padding: 4px 10px; border-radius: 9999px; font-size: 12px; font-weight: 600; background: var(--accent-glow); color: var(--accent); border: 1px solid rgba(56, 189, 248, 0.3); margin-bottom: 12px; }}
  .header {{ background: var(--card-bg); border: 1px solid var(--border); border-radius: 16px; padding: 28px; margin-bottom: 24px; }}
  h1 {{ font-size: 28px; font-weight: 700; margin-bottom: 8px; color: #fff; }}
  .desc {{ color: var(--text-muted); font-size: 14px; margin-bottom: 20px; }}
  .meta-grid {{ display: grid; grid-template-columns: repeat(auto-fit, minmax(200px, 1fr)); gap: 12px; margin-top: 16px; }}
  .meta-item {{ background: rgba(15, 23, 42, 0.6); border: 1px solid var(--border); padding: 12px 16px; border-radius: 10px; }}
  .meta-label {{ font-size: 11px; text-transform: uppercase; color: var(--text-muted); font-weight: 600; letter-spacing: 0.5px; }}
  .meta-val {{ font-size: 14px; font-weight: 600; color: #fff; margin-top: 4px; font-family: var(--mono); word-break: break-all; }}
  .wake-card {{ background: linear-gradient(135deg, rgba(16, 185, 129, 0.08), rgba(56, 189, 248, 0.08)); border: 1px solid rgba(16, 185, 129, 0.3); border-radius: 14px; padding: 20px; margin-bottom: 24px; display: flex; align-items: center; justify-content: space-between; gap: 16px; }}
  .wake-title {{ font-size: 16px; font-weight: 700; color: #fff; margin-bottom: 4px; }}
  .wake-desc {{ font-size: 13px; color: var(--text-muted); }}
  .wake-code {{ font-family: var(--mono); background: rgba(0,0,0,0.4); padding: 4px 8px; border-radius: 6px; font-size: 12px; color: #38bdf8; display: inline-block; margin-top: 6px; }}
  .files-section {{ background: var(--card-bg); border: 1px solid var(--border); border-radius: 16px; padding: 24px; }}
  .files-header {{ display: flex; align-items: center; justify-content: space-between; margin-bottom: 16px; flex-wrap: wrap; gap: 10px; }}
  .search-input {{ background: rgba(15, 23, 42, 0.8); border: 1px solid var(--border); color: var(--text); padding: 8px 14px; border-radius: 8px; font-size: 13px; width: 260px; outline: none; }}
  .search-input:focus {{ border-color: var(--accent); }}
  .file-table {{ width: 100%; border-collapse: collapse; font-size: 13px; }}
  .file-table th {{ text-align: left; padding: 10px 12px; color: var(--text-muted); font-size: 11px; text-transform: uppercase; border-bottom: 1px solid var(--border); }}
  .file-table td {{ padding: 10px 12px; border-bottom: 1px solid rgba(255, 255, 255, 0.04); font-family: var(--mono); }}
  .file-size {{ text-align: right; color: var(--text-muted); }}
</style>
</head>
<body>
<div class="container">
  <div class="header">
    <div class="badge">👻 BULUT VİTRİNİ</div>
    <h1>{project_name}</h1>
    <p class="desc">Bu projenin tüm kaynak kodları ve git geçmişi Google Drive üzerinde güvenle saklanmaktadır. Yerel diskinizde yer tasarrufu amacıyla bu vitrin görünümü bırakılmıştır.</p>
    <div class="meta-grid">
      <div class="meta-item"><div class="meta-label">📅 Arşivlenme Tarihi</div><div class="meta-val">{archived_at}</div></div>
      <div class="meta-item"><div class="meta-label">📦 Toplam Dosya</div><div class="meta-val">{total_files} adet</div></div>
      <div class="meta-item"><div class="meta-label">💾 Arşiv Boyutu</div><div class="meta-val">{human_total_bytes}</div></div>
      <div class="meta-item"><div class="meta-label">☁️ Drive Arşiv Konumu</div><div class="meta-val">{archive_remote_str}</div></div>
    </div>
  </div>

  <div class="wake-card">
    <div>
      <div class="wake-title">⚡ Bu Projeyi Uyandır (SSD'ye Geri Yükle)</div>
      <div class="wake-desc">Bu klasördeki <span class="wake-code">Projeyi_Uyandir.command</span> betiğine çift tıklayabilir veya terminalden çalıştırabilirsiniz:</div>
      <div class="wake-code">devdrive unarchive "{local_path_str}"</div>
    </div>
  </div>

  <div class="files-section">
    <div class="files-header">
      <div style="font-weight: 700; color: #fff;">Arşivdeki Dosyalar ({total_files})</div>
      <input type="text" class="search-input" id="searchBox" placeholder="Dosyalarda filtrele..." oninput="filterFiles()">
    </div>
    <table class="file-table" id="filesTable">
      <thead>
        <tr><th>Dosya / Dizin</th><th style="text-align: right;">Boyut</th></tr>
      </thead>
      <tbody>
        {table_rows}
      </tbody>
    </table>
  </div>
</div>
<script>
function filterFiles() {{
  const q = document.getElementById('searchBox').value.toLowerCase();
  const rows = document.querySelectorAll('#filesTable tbody tr');
  rows.forEach(r => {{
    const text = r.children[0].textContent.toLowerCase();
    r.style.display = text.includes(q) ? '' : 'none';
  }});
}}
</script>
</body>
</html>"#,
        project_name = html_escape(&manifest.project_name),
        archived_at = html_escape(&manifest.archived_at),
        total_files = manifest.total_files,
        human_total_bytes = human_total_bytes,
        archive_remote_str = html_escape(&archive_remote_path.to_string_lossy()),
        local_path_str = html_escape(&local_path.to_string_lossy()),
        table_rows = table_rows,
    );

    std::fs::write(local_path.join(SHOWCASE_HTML_FILE), html_content)?;
    Ok(())
}

pub struct OffloadEngine;

impl OffloadEngine {
    /// Returns default local quarantine base directory: ~/.devdrive/quarantine
    pub fn default_quarantine_base_dir() -> PathBuf {
        dirs::home_dir()
            .unwrap_or_else(|| PathBuf::from("."))
            .join(".devdrive")
            .join("quarantine")
    }

    /// Performs a preflight scan of the project, classifying every file into Class A, B, or C
    pub fn scan_project<P: AsRef<Path>>(
        project_path: P,
        config: &DevDriveConfig,
        bypass_env_warning: bool,
    ) -> Result<PreflightReport> {
        let p_path = project_path.as_ref();
        if !p_path.exists() || !p_path.is_dir() {
            bail!("Proje dizini bulunamadı veya bir dizin değil: {}", p_path.display());
        }

        let canonical_project = p_path.canonicalize()
            .with_context(|| format!("Proje dizini doğrulanamadı: {}", p_path.display()))?;

        let project_name = canonical_project
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_else(|| "project".to_string());

        let rules = DevFilter::reproducible_rules();
        let mut class_a_items: Vec<RecipeArtifact> = Vec::new();
        let mut class_a_prefixes: HashSet<PathBuf> = HashSet::new();
        let mut class_c_blockers: Vec<String> = Vec::new();

        // 1. Identify Class A (reproducible directories)
        // First scan directory entries at root and nested levels
        for entry in walkdir::WalkDir::new(&canonical_project).min_depth(1).into_iter().filter_map(|e| e.ok()) {
            if !entry.file_type().is_dir() {
                continue;
            }

            let entry_path = entry.path();
            if let Ok(rel) = entry_path.strip_prefix(&canonical_project) {
                // If already inside an existing Class A prefix, skip
                if class_a_prefixes.iter().any(|prefix| rel.starts_with(prefix)) {
                    continue;
                }

                let dir_name = entry.file_name().to_string_lossy();
                let depth = rel.components().count();

                // Check matching reproducible rule
                if let Some(rule) = rules.iter().find(|r| {
                    if r.is_root_only && depth != 1 {
                        false
                    } else {
                        r.dir_name == dir_name
                    }
                }) {
                    // Check required markers (e.g. lockfiles or manifest)
                    let mut lockfiles_found = Vec::new();
                    let mut has_marker = rule.required_markers.is_empty();

                    for &marker in rule.required_markers {
                        let marker_path = canonical_project.join(marker);
                        if marker_path.exists() {
                            has_marker = true;
                            if let Ok(hash) = CacheManager::compute_sha256(&marker_path) {
                                lockfiles_found.push(RecipeLockfile {
                                    path: marker.to_string(),
                                    sha256: hash,
                                });
                            }
                        }
                    }

                    if has_marker {
                        // Measure size
                        let mut size_bytes = 0u64;
                        for f in walkdir::WalkDir::new(entry_path).into_iter().filter_map(|e| e.ok()) {
                            if (f.file_type().is_file() || f.file_type().is_symlink())
                                && let Ok(m) = f.metadata() {
                                    size_bytes += m.len();
                                }
                        }

                        class_a_items.push(RecipeArtifact {
                            path: rel.to_string_lossy().to_string(),
                            size_bytes,
                            tool_or_rule: rule.dir_name.to_string(),
                            lockfiles: lockfiles_found,
                            rebuild_command: rule.rebuild_command.to_string(),
                        });
                        class_a_prefixes.insert(rel.to_path_buf());
                    } else {
                        // Rule matched but required lockfile / marker is missing -> Class C blocker!
                        class_c_blockers.push(format!(
                            "Gereken kilit dosyası bulunamadı: '{}' için beklenen dosyalar ({:?}) projede yok.",
                            rel.display(),
                            rule.required_markers
                        ));
                    }
                }
            }
        }

        // 2. Classify Class B and Class C files
        let mut class_b_items: Vec<ManifestEntry> = Vec::new();
        let mut env_files_count = 0;

        for entry in walkdir::WalkDir::new(&canonical_project)
            .into_iter()
            .filter_entry(|e| {
                if e.file_type().is_dir() {
                    if e.file_name() == ".git" {
                        return config.include_git;
                    }
                    if class_a_prefixes.iter().any(|prefix| {
                        e.path().strip_prefix(&canonical_project).map(|r| r.starts_with(prefix)).unwrap_or(false)
                    }) {
                        return false;
                    }
                    if crate::filter::is_ephemeral_dir_by_signature(e.path()) {
                        return false;
                    }
                }
                true
            })
            .filter_map(|e| e.ok()) {
            let entry_path = entry.path();
            if entry_path == canonical_project {
                continue;
            }

            let rel = match entry_path.strip_prefix(&canonical_project) {
                Ok(r) => r,
                Err(_) => continue,
            };

            // Skip if inside Class A directory
            if class_a_prefixes.iter().any(|prefix| rel.starts_with(prefix)) {
                continue;
            }

            let file_name = entry.file_name().to_string_lossy();
            let is_env = file_name == ".env" || file_name.starts_with(".env.");

            if is_env {
                env_files_count += 1;
                if !config.backup_env_files {
                    if !bypass_env_warning {
                        class_c_blockers.push(format!(
                            "'.env' koruma engeli: '{}' dosyası bulundu ve 'backup_env_files' ayarı kapalı. Elle yedeklendiğini onaylamak için --yes ve onay bayrağı gereklidir.",
                            rel.display()
                        ));
                    }
                    // When not backing up env files, do not include in Class B archive
                    continue;
                }
            }

            // Check if inside .git
            let is_in_git = rel.components().any(|c| c.as_os_str() == ".git");
            if is_in_git && !config.include_git {
                continue;
            }

            let file_type = entry.file_type();

            if file_type.is_symlink() {
                // Symlink analysis
                match std::fs::read_link(entry_path) {
                    Ok(target) => {
                        let target_str = target.to_string_lossy().to_string();
                        // Resolve target to see if it escapes project root
                        let resolved_target = if target.is_absolute() {
                            target.clone()
                        } else {
                            entry_path.parent().unwrap_or(entry_path).join(&target)
                        };

                        // Check if symlink target points outside the project root
                        let is_outside = if let Ok(canonical_target) = resolved_target.canonicalize() {
                            !canonical_target.starts_with(&canonical_project)
                        } else {
                            // If target doesn't exist, check normalized path
                            let mut depth: i32 = 0;
                            let mut escapes = false;
                            for comp in rel.parent().unwrap_or(Path::new("")).components() {
                                if comp.as_os_str() != "." { depth += 1; }
                            }
                            for comp in target.components() {
                                match comp {
                                    std::path::Component::ParentDir => {
                                        depth -= 1;
                                        if depth < 0 { escapes = true; break; }
                                    }
                                    std::path::Component::Normal(_) => depth += 1,
                                    std::path::Component::RootDir if !target.starts_with(&canonical_project) => {
                                        escapes = true; break;
                                    }
                                    _ => {}
                                }
                            }
                            escapes
                        };

                        if is_outside {
                            class_c_blockers.push(format!(
                                "Güvenlik Engeli: Proje dışına işaret eden symlink tespit edildi: '{}' -> '{}'",
                                rel.display(),
                                target_str
                            ));
                        } else {
                            let meta = entry.metadata().or_else(|_| std::fs::symlink_metadata(entry_path)).ok();
                            let mode = meta.as_ref().map(|m| m.permissions().mode()).unwrap_or(0o777);
                            let mtime = meta.as_ref().and_then(|m| m.modified().ok())
                                .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                                .map(|d| d.as_secs() as i64)
                                .unwrap_or(0);

                            class_b_items.push(ManifestEntry {
                                path: rel.to_string_lossy().to_string(),
                                kind: ManifestEntryKind::Symlink,
                                size_bytes: 0,
                                sha256: String::new(),
                                mode,
                                mtime,
                                symlink_target: Some(target_str),
                            });
                        }
                    }
                    Err(e) => {
                        class_c_blockers.push(format!(
                            "Okunamayan symlink '{}': {}",
                            rel.display(),
                            e
                        ));
                    }
                }
            } else if file_type.is_dir() {
                // Record directory in Class B if it's empty (preserves empty folder structure)
                if let Ok(mut rd) = std::fs::read_dir(entry_path)
                    && rd.next().is_none() {
                        let meta = entry.metadata().ok();
                        let mode = meta.as_ref().map(|m| m.permissions().mode()).unwrap_or(0o755);
                        let mtime = meta.as_ref().and_then(|m| m.modified().ok())
                            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                            .map(|d| d.as_secs() as i64)
                            .unwrap_or(0);

                        class_b_items.push(ManifestEntry {
                            path: rel.to_string_lossy().to_string(),
                            kind: ManifestEntryKind::Directory,
                            size_bytes: 0,
                            sha256: String::new(),
                            mode,
                            mtime,
                            symlink_target: None,
                        });
                    }
            } else if file_type.is_file() {
                match CacheManager::compute_sha256(entry_path) {
                    Ok(sha256) => {
                        let meta = entry.metadata().with_context(|| format!("Metadata okunamadı: {}", rel.display()))?;
                        let mode = meta.permissions().mode();
                        let mtime = meta.modified().ok()
                            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                            .map(|d| d.as_secs() as i64)
                            .unwrap_or(0);

                        class_b_items.push(ManifestEntry {
                            path: rel.to_string_lossy().to_string(),
                            kind: ManifestEntryKind::File,
                            size_bytes: meta.len(),
                            sha256,
                            mode,
                            mtime,
                            symlink_target: None,
                        });
                    }
                    Err(e) => {
                        class_c_blockers.push(format!(
                            "Okunamayan veya erişilemeyen dosya '{}': {}",
                            rel.display(),
                            e
                        ));
                    }
                }
            }
        }

        // Yoğunluk / Boyut Anomali Kalkanı (Heuristic Guard)
        {
            let mut dir_file_counts: std::collections::HashMap<PathBuf, usize> = std::collections::HashMap::new();
            let mut dir_byte_counts: std::collections::HashMap<PathBuf, u64> = std::collections::HashMap::new();
            for item in &class_b_items {
                let p = Path::new(&item.path);
                let size = item.size_bytes;
                let mut curr = p.parent();
                while let Some(parent) = curr {
                    if parent.as_os_str().is_empty() {
                        break;
                    }
                    *dir_file_counts.entry(parent.to_path_buf()).or_insert(0) += 1;
                    *dir_byte_counts.entry(parent.to_path_buf()).or_insert(0) += size;
                    curr = parent.parent();
                }
            }
            let mut anomalous_dirs: Vec<PathBuf> = Vec::new();
            for (dir, &count) in &dir_file_counts {
                let bytes = dir_byte_counts.get(dir).copied().unwrap_or(0);
                if count > 2500 || bytes > 250 * 1024 * 1024 {
                    anomalous_dirs.push(dir.clone());
                }
            }
            if !anomalous_dirs.is_empty() {
                anomalous_dirs.sort_by_key(|p| p.components().count());
                let mut top_anomalies: Vec<PathBuf> = Vec::new();
                for dir in anomalous_dirs {
                    if !top_anomalies.iter().any(|top| dir.starts_with(top)) {
                        let count = dir_file_counts.get(&dir).copied().unwrap_or(0);
                        let bytes = dir_byte_counts.get(&dir).copied().unwrap_or(0);
                        tracing::warn!(
                            "⚠️ Yoğunluk/Boyut Anomali Kalkanı: '{}' alt klasörü tek başına {} dosya ve {:.2} MB içeriyor. Ağır Veri/Ortam Klasörü olarak otomatik atlandı.",
                            dir.display(),
                            count,
                            bytes as f64 / (1024.0 * 1024.0)
                        );
                        top_anomalies.push(dir);
                    }
                }
                class_b_items.retain(|item| {
                    let p = Path::new(&item.path);
                    !top_anomalies.iter().any(|top| p.starts_with(top))
                });
            }
        }

        let class_a_total_bytes: u64 = class_a_items.iter().map(|a| a.size_bytes).sum();
        let class_b_total_bytes: u64 = class_b_items.iter().map(|b| b.size_bytes).sum();
        let can_proceed = class_c_blockers.is_empty();

        if !config.include_git && canonical_project.join(".git").exists() {
            eprintln!("⚠️  UYARI: Projede '.git' dizini mevcut ancak 'include_git' ayarı kapalı! Git geçmişi Google Drive arşivine dahil EDİLMEYECEK.");
        }

        Ok(PreflightReport {
            project_name,
            local_path: canonical_project.to_string_lossy().to_string(),
            class_a_items,
            class_a_total_bytes,
            class_b_items,
            class_b_total_bytes,
            class_c_blockers,
            env_files_count,
            env_backed_up: config.backup_env_files,
            can_proceed,
        })
    }

    /// Cleans Class A reproducible artifacts (e.g. node_modules, target, .venv) from local project
    pub fn clean_artifacts<P: AsRef<Path>>(
        project_path: P,
        dry_run: bool,
    ) -> Result<CleanArtifactsReport> {
        let p_path = project_path.as_ref();
        let config = DevDriveConfig::load().unwrap_or_default();
        let preflight = Self::scan_project(p_path, &config, true)?;

        let mut cleaned_items = Vec::new();
        let mut total_freed_bytes = 0u64;

        for item in preflight.class_a_items {
            let target_path = Path::new(&preflight.local_path).join(&item.path);
            if !dry_run && target_path.exists() {
                std::fs::remove_dir_all(&target_path)
                    .with_context(|| format!("Artifakt silinemedi: {}", target_path.display()))?;
                info!("Temizlendi: {}", target_path.display());
            }
            total_freed_bytes += item.size_bytes;
            cleaned_items.push(item);
        }

        Ok(CleanArtifactsReport {
            project_name: preflight.project_name,
            local_path: preflight.local_path,
            dry_run,
            cleaned_items,
            total_freed_bytes,
        })
    }

    /// Full Offload: Class A deleted, Class B archived and placed into local quarantine (7 days)
    pub fn offload(
        db: &mut Database,
        project_id: i64,
        config: &DevDriveConfig,
        dry_run: bool,
        bypass_env_warning: bool,
    ) -> Result<OffloadReport> {
        let proj = db.get_project_by_id(project_id)?
            .with_context(|| format!("Proje ID {} bulunamadı", project_id))?;

        if proj.status == "archived" {
            bail!("Proje '{}' zaten arşivlenmiş (SSD'den tahliye edilmiş) durumda.", proj.name);
        }

        let local_path = PathBuf::from(&proj.local_path);
        if !local_path.exists() {
            bail!("Yerel proje dizini mevcut değil: {}", local_path.display());
        }

        // 1. Preflight Scan & Gates
        let preflight = Self::scan_project(&local_path, config, bypass_env_warning)?;
        if !preflight.can_proceed {
            bail!(
                "Tahliye (Offload) Güvenlik Kapısı Başarısız: Sınıf C engelleyiciler tespit edildi:\n  - {}",
                preflight.class_c_blockers.join("\n  - ")
            );
        }

        let now_dt: DateTime<Utc> = Utc::now();
        let timestamp_str = now_dt.format("%Y%m%d_%H%M%S_%3f").to_string();

        let remote_archives_root = Path::new(&config.remote_drive_base_path)
            .join(".archives")
            .join(&proj.name)
            .join(&timestamp_str);

        let q_base = if let Some(ref custom_q) = config.quarantine_base_path {
            PathBuf::from(custom_q)
        } else {
            Self::default_quarantine_base_dir()
        };
        let quarantine_dir = q_base.join(format!("{}_{}", proj.name, timestamp_str));

        if dry_run {
            return Ok(OffloadReport {
                project_id,
                project_name: proj.name,
                dry_run: true,
                class_a_freed_bytes: preflight.class_a_total_bytes,
                class_b_archived_bytes: preflight.class_b_total_bytes,
                class_b_file_count: preflight.class_b_items.len(),
                archive_remote_path: remote_archives_root.to_string_lossy().to_string(),
                quarantine_path: Some(quarantine_dir.to_string_lossy().to_string()),
                quarantine_until: Some((now_dt + Duration::days(config.quarantine_days as i64)).to_rfc3339()),
                manifest_sha256: String::new(),
            });
        }

        // Mark DB status as archiving and journal
        let _ = db.update_project_status(project_id, "archiving");
        let _ = db.record_offload_journal(project_id, "preflight", "success", "Preflight passed");

        // 2. Prepare Remote Archive Directory: .archives/<proj>/<time>
        std::fs::create_dir_all(&remote_archives_root)
            .with_context(|| format!("Arşiv dizini oluşturulamadı: {}", remote_archives_root.display()))?;

        let _ = db.record_offload_journal(project_id, "copy_archive", "started", "Packaging Class B files into compressed archive (tree.tar.gz)");

        // 3. Package and compress Class B files into tree.tar.gz with byte-by-byte SHA256 integrity verification
        let tar_gz_path = remote_archives_root.join("tree.tar.gz");
        {
            let tar_file = std::fs::File::create(&tar_gz_path)
                .with_context(|| format!("Arşiv paketi oluşturulamadı: {}", tar_gz_path.display()))?;
            let enc = flate2::write::GzEncoder::new(tar_file, flate2::Compression::default());
            let mut tar_builder = tar::Builder::new(enc);

            for item in &preflight.class_b_items {
                let src_file = local_path.join(&item.path);

                match item.kind {
                    ManifestEntryKind::Directory => {
                        let mut header = tar::Header::new_gnu();
                        header.set_entry_type(tar::EntryType::Directory);
                        header.set_size(0);
                        header.set_mode(item.mode);
                        header.set_mtime(item.mtime as u64);
                        header.set_cksum();
                        tar_builder.append_data(&mut header, &item.path, std::io::empty())?;
                    }
                    ManifestEntryKind::Symlink => {
                        if let Some(ref target) = item.symlink_target {
                            let mut header = tar::Header::new_gnu();
                            header.set_entry_type(tar::EntryType::Symlink);
                            header.set_size(0);
                            header.set_mode(item.mode);
                            header.set_mtime(item.mtime as u64);
                            header.set_link_name(target)?;
                            header.set_cksum();
                            tar_builder.append_data(&mut header, &item.path, std::io::empty())?;
                        }
                    }
                    ManifestEntryKind::File => {
                        let pre_meta = src_file.metadata()?;
                        let mut file = std::fs::File::open(&src_file)?;

                        let mut header = tar::Header::new_gnu();
                        header.set_entry_type(tar::EntryType::Regular);
                        header.set_size(pre_meta.len());
                        header.set_mode(item.mode);
                        header.set_mtime(item.mtime as u64);
                        header.set_cksum();
                        tar_builder.append_data(&mut header, &item.path, &mut file)?;

                        // Post-copy mutation check
                        let post_meta = src_file.metadata()?;
                        if pre_meta.len() != post_meta.len() || pre_meta.modified()? != post_meta.modified()? {
                            let _ = db.record_offload_journal(project_id, "copy_archive", "failed", "File mutated during packaging");
                            bail!("Arşivleme sırasında dosya içeriği değişti (Mutation detected): {}", src_file.display());
                        }

                        // Local hash verification against item.sha256
                        let hash = CacheManager::compute_sha256(&src_file)?;
                        if hash != item.sha256 {
                            let _ = db.record_offload_journal(project_id, "copy_archive", "failed", "Checksum mismatch");
                            bail!(
                                "Bütünlük hatası: Dosya özeti tutmuyor: {}. Beklenen: {}, Alınan: {}",
                                src_file.display(),
                                item.sha256,
                                hash
                            );
                        }
                    }
                }
            }

            let enc = tar_builder.into_inner()?;
            enc.finish()?;
        }

        let _ = db.record_offload_journal(project_id, "copy_archive", "success", "Class B files packaged and compressed into tree.tar.gz");

        // 4. Write recipe.json
        let rebuild_commands: Vec<String> = preflight.class_a_items.iter().map(|a| a.rebuild_command.clone()).collect();
        let recipe = Recipe {
            version: "1.0".to_string(),
            project_name: proj.name.clone(),
            artifacts: preflight.class_a_items.clone(),
            rebuild_commands,
        };
        let recipe_json = serde_json::to_string_pretty(&recipe)?;
        std::fs::write(remote_archives_root.join("recipe.json"), recipe_json)?;

        // 5. Write manifest.json and seal with manifest.sha256
        let manifest = OffloadManifest {
            version: "1.0".to_string(),
            project_name: proj.name.clone(),
            original_local_path: local_path.to_string_lossy().to_string(),
            archived_at: now_dt.to_rfc3339(),
            total_files: preflight.class_b_items.len(),
            total_bytes: preflight.class_b_total_bytes,
            entries: preflight.class_b_items.clone(),
        };

        let manifest_json = serde_json::to_string_pretty(&manifest)?;
        let manifest_path = remote_archives_root.join("manifest.json");
        std::fs::write(&manifest_path, &manifest_json)?;

        // Compute manifest hash and write manifest.sha256 (En son mühürleme)
        let manifest_hash = CacheManager::compute_sha256(&manifest_path)?;
        std::fs::write(remote_archives_root.join("manifest.sha256"), format!("{}  manifest.json\n", manifest_hash))?;

        let _ = db.record_offload_journal(project_id, "write_manifest", "success", "Manifest sealed with SHA256");

        // 6. Pre-Deletion Verification: Re-hash local Class B files. Every file must still match manifest.
        for item in &preflight.class_b_items {
            if item.kind == ManifestEntryKind::File {
                let src_file = local_path.join(&item.path);
                if !src_file.exists() {
                    bail!("Silme öncesi doğrulama hatası: Yerel dosya eksik: {}", src_file.display());
                }
                let current_hash = CacheManager::compute_sha256(&src_file)?;
                if current_hash != item.sha256 {
                    let _ = db.record_offload_journal(project_id, "verify_before_delete", "failed", "Hash mismatch before deletion");
                    bail!("Silme öncesi doğrulama hatası: {} içeriği değişmiş!", src_file.display());
                }
            }
        }

        let _ = db.record_offload_journal(project_id, "verify_before_delete", "success", "Pre-deletion verification passed");

        // 7. Delete Class A folders (Frees SSD space immediately!)
        for item in &preflight.class_a_items {
            let a_dir = local_path.join(&item.path);
            if a_dir.exists() {
                std::fs::remove_dir_all(&a_dir)
                    .with_context(|| format!("Sınıf A dizini silinemedi: {}", a_dir.display()))?;
                info!("Sınıf A klasörü temizlendi: {}", a_dir.display());
            }
        }

        let _ = db.record_offload_journal(project_id, "delete_class_a", "success", "Class A artifacts deleted");

        // 8. Direct Showcase Mode or Atomic Rename into Quarantine
        let is_direct_showcase = config.use_direct_showcase && config.quarantine_base_path.is_none();

        let (quarantine_str_opt, q_until_str_opt, freed_bytes) = if is_direct_showcase {
            // Delete remaining Class B files and folders, keeping README.md if present
            if let Ok(entries) = std::fs::read_dir(&local_path) {
                for entry in entries.filter_map(|e| e.ok()) {
                    let name = entry.file_name().to_string_lossy().to_string();
                    if name == "README.md" {
                        continue;
                    }
                    let p = entry.path();
                    if p.is_dir() {
                        let _ = std::fs::remove_dir_all(&p);
                    } else {
                        let _ = std::fs::remove_file(&p);
                    }
                }
            }

            write_showcase_files(&local_path, &manifest, &remote_archives_root)?;
            let _ = db.record_offload_journal(project_id, "write_showcase", "success", "Showcase files created in project dir");

            let total_freed = (preflight.class_a_total_bytes + preflight.class_b_total_bytes) as i64;
            (None, None, total_freed)
        } else {
            if let Some(parent) = quarantine_dir.parent() {
                std::fs::create_dir_all(parent)?;
            }

            Self::move_or_copy_dir(&local_path, &quarantine_dir)
                .with_context(|| format!("Yerel proje karantinaya taşınamadı: {} -> {}", local_path.display(), quarantine_dir.display()))?;

            let _ = db.record_offload_journal(project_id, "quarantine_rename", "success", "Moved to local quarantine");

            let quarantine_until = now_dt + Duration::days(config.quarantine_days as i64);
            (
                Some(quarantine_dir.to_string_lossy().to_string()),
                Some(quarantine_until.to_rfc3339()),
                preflight.class_a_total_bytes as i64,
            )
        };

        // 9. Update DB Status to 'archived'
        let archive_remote_str = remote_archives_root.to_string_lossy().to_string();

        db.update_project_offload_info(
            project_id,
            "archived",
            Some(&now_dt.to_rfc3339()),
            Some(&archive_remote_str),
            quarantine_str_opt.as_deref(),
            q_until_str_opt.as_deref(),
            Some(&manifest_hash),
            freed_bytes,
        )?;

        let _ = db.record_offload_journal(project_id, "complete", "success", "Offload completed successfully");

        info!(
            "Offload tamamlandı: Proje '{}' arşivlendi. Açılan alan: {} bayt, Karantina: {:?}",
            proj.name, freed_bytes, quarantine_str_opt
        );

        Ok(OffloadReport {
            project_id,
            project_name: proj.name,
            dry_run: false,
            class_a_freed_bytes: preflight.class_a_total_bytes,
            class_b_archived_bytes: preflight.class_b_total_bytes,
            class_b_file_count: preflight.class_b_items.len(),
            archive_remote_path: archive_remote_str,
            quarantine_path: quarantine_str_opt,
            quarantine_until: q_until_str_opt,
            manifest_sha256: manifest_hash,
        })
    }

    /// Verifies the integrity of a remote archive by validating manifest.sha256 and reading every file
    pub fn archive_verify<P: AsRef<Path>>(remote_archive_path: P) -> Result<ArchiveVerificationReport> {
        let arch_dir = remote_archive_path.as_ref();
        if !arch_dir.exists() || !arch_dir.is_dir() {
            bail!("Arşiv dizini mevcut değil: {}", arch_dir.display());
        }

        let manifest_path = arch_dir.join("manifest.json");
        let manifest_sha_path = arch_dir.join("manifest.sha256");

        if !manifest_path.exists() || !manifest_sha_path.exists() {
            bail!("Arşiv eksik: manifest.json veya manifest.sha256 bulunamadı: {}", arch_dir.display());
        }

        let sha_file_content = std::fs::read_to_string(&manifest_sha_path)?;
        let expected_manifest_hash = sha_file_content.split_whitespace().next().unwrap_or("").to_string();

        let actual_manifest_hash = CacheManager::compute_sha256(&manifest_path)?;
        if expected_manifest_hash != actual_manifest_hash {
            bail!(
                "Manifest mühür doğrulaması başarısız! Beklenen: {}, Gerçek: {}",
                expected_manifest_hash,
                actual_manifest_hash
            );
        }

        let manifest_content = std::fs::read_to_string(&manifest_path)?;
        let manifest: OffloadManifest = serde_json::from_str(&manifest_content)
            .context("Manifest JSON çözümlenemedi")?;

        let tar_gz_path = arch_dir.join("tree.tar.gz");
        let tree_dir = arch_dir.join("tree");

        if !tar_gz_path.exists() && !tree_dir.exists() {
            bail!("Arşiv içeriği (tree.tar.gz veya tree/) bulunamadı: {}", arch_dir.display());
        }

        let mut verified_files = 0;
        let mut verified_bytes = 0u64;
        let mut errors = Vec::new();

        if tar_gz_path.exists() {
            let file = match std::fs::File::open(&tar_gz_path) {
                Ok(f) => f,
                Err(e) => {
                    return Ok(ArchiveVerificationReport {
                        archive_path: arch_dir.to_string_lossy().to_string(),
                        manifest_sha256: actual_manifest_hash,
                        total_entries: manifest.entries.len(),
                        verified_files: 0,
                        verified_bytes: 0,
                        is_valid: false,
                        errors: vec![format!("Arşiv paketi açılamadı: {}", e)],
                    });
                }
            };
            let gz = flate2::read::GzDecoder::new(file);
            let mut archive = tar::Archive::new(gz);

            let mut archive_files: HashMap<String, (String, u64)> = HashMap::new();
            let mut archive_dirs: HashSet<String> = HashSet::new();
            let mut archive_symlinks: HashSet<String> = HashSet::new();

            match archive.entries() {
                Ok(entries) => {
                    for entry_res in entries {
                        match entry_res {
                            Ok(mut entry) => {
                                let path_str = match entry.path() {
                                    Ok(p) => p.to_string_lossy().to_string(),
                                    Err(_) => continue,
                                };
                                let entry_type = entry.header().entry_type();

                                if entry_type.is_file() {
                                    use sha2::{Digest, Sha256};
                                    let mut hasher = Sha256::new();
                                    let mut buf = [0u8; 64 * 1024];
                                    let mut total_read = 0u64;
                                    let mut read_failed = false;
                                    loop {
                                        match std::io::Read::read(&mut entry, &mut buf) {
                                            Ok(0) => break,
                                            Ok(n) => {
                                                hasher.update(&buf[..n]);
                                                total_read += n as u64;
                                            }
                                            Err(e) => {
                                                errors.push(format!("Arşiv içi dosya okuma hatası {}: {}", path_str, e));
                                                read_failed = true;
                                                break;
                                            }
                                        }
                                    }
                                    if !read_failed {
                                        let hash = format!("{:x}", hasher.finalize());
                                        archive_files.insert(path_str, (hash, total_read));
                                    }
                                } else if entry_type.is_dir() {
                                    archive_dirs.insert(path_str);
                                } else if entry_type.is_symlink() {
                                    archive_symlinks.insert(path_str);
                                }
                            }
                            Err(e) => {
                                errors.push(format!("Bozuk arşiv girişi: {}", e));
                            }
                        }
                    }
                }
                Err(e) => {
                    errors.push(format!("Arşiv paketi çözümlenemedi (bozuk dosya): {}", e));
                }
            }

            for entry in &manifest.entries {
                match entry.kind {
                    ManifestEntryKind::Directory => {
                        let clean_path = entry.path.trim_end_matches('/');
                        if !archive_dirs.iter().any(|d| d.trim_end_matches('/') == clean_path) {
                            errors.push(format!("Klasör eksik: {}", entry.path));
                        }
                    }
                    ManifestEntryKind::Symlink => {
                        if !archive_symlinks.contains(&entry.path) {
                            errors.push(format!("Symlink eksik: {}", entry.path));
                        }
                    }
                    ManifestEntryKind::File => {
                        if let Some((hash, size)) = archive_files.get(&entry.path) {
                            if *hash != entry.sha256 {
                                errors.push(format!("Özet uyuşmazlığı: {} (beklenen: {}, bulunan: {})", entry.path, entry.sha256, hash));
                            } else {
                                verified_files += 1;
                                verified_bytes += *size;
                            }
                        } else {
                            errors.push(format!("Dosya eksik: {}", entry.path));
                        }
                    }
                }
            }
        } else {
            for entry in &manifest.entries {
                let target_path = tree_dir.join(&entry.path);
                match entry.kind {
                    ManifestEntryKind::Directory => {
                        if !target_path.exists() || !target_path.is_dir() {
                            errors.push(format!("Klasör eksik: {}", entry.path));
                        }
                    }
                    ManifestEntryKind::Symlink => {
                        if !target_path.exists() && std::fs::symlink_metadata(&target_path).is_err() {
                            errors.push(format!("Symlink eksik: {}", entry.path));
                        }
                    }
                    ManifestEntryKind::File => {
                        if !target_path.exists() || !target_path.is_file() {
                            errors.push(format!("Dosya eksik: {}", entry.path));
                            continue;
                        }

                        match CacheManager::compute_sha256(&target_path) {
                            Ok(hash) => {
                                if hash != entry.sha256 {
                                    errors.push(format!("Özet uyuşmazlığı: {} (beklenen: {}, bulunan: {})", entry.path, entry.sha256, hash));
                                } else {
                                    verified_files += 1;
                                    verified_bytes += entry.size_bytes;
                                }
                            }
                            Err(e) => {
                                errors.push(format!("Dosya okuma hatası {}: {}", entry.path, e));
                            }
                        }
                    }
                }
            }
        }

        let is_valid = errors.is_empty();

        Ok(ArchiveVerificationReport {
            archive_path: arch_dir.to_string_lossy().to_string(),
            manifest_sha256: actual_manifest_hash,
            total_entries: manifest.entries.len(),
            verified_files,
            verified_bytes,
            is_valid,
            errors,
        })
    }

    /// Brings project back to local SSD either from local quarantine (instant rename) or from remote Drive archive
    pub fn unarchive(
        db: &mut Database,
        project_id: i64,
        _config: &DevDriveConfig,
        rebuild: bool,
    ) -> Result<UnarchiveReport> {
        let proj = db.get_project_by_id(project_id)?
            .with_context(|| format!("Proje ID {} bulunamadı", project_id))?;

        if proj.status == "active" {
            bail!("Proje '{}' zaten aktif durumda.", proj.name);
        }

        let target_local_path = PathBuf::from(&proj.local_path);
        if target_local_path.exists() {
            let is_showcase = is_showcase_dir(&target_local_path) || has_only_showcase_files(&target_local_path);
            if is_showcase {
                let _ = std::fs::remove_file(target_local_path.join(SHOWCASE_META_FILE));
                let _ = std::fs::remove_file(target_local_path.join(SHOWCASE_HTML_FILE));
                let _ = std::fs::remove_file(target_local_path.join(SHOWCASE_WAKE_SCRIPT));
                let _ = std::fs::remove_file(target_local_path.join("⚡ Projeyi_Uyandir.command"));
                let _ = std::fs::remove_file(target_local_path.join(".DS_Store"));

                // If restoring from quarantine, remove target_local_path so move_or_copy_dir succeeds cleanly
                if proj.quarantine_path.is_some() {
                    let _ = std::fs::remove_dir_all(&target_local_path);
                }
            } else {
                bail!(
                    "Hedef yerel dizin '{}' zaten mevcut! Çakışmayı önlemek için işlem durduruldu.",
                    target_local_path.display()
                );
            }
        }

        let mut restored_from_quarantine = false;
        let mut restored_from_archive = false;
        let mut files_restored = 0;
        let mut bytes_restored = 0u64;

        // Path 1: Restore from local Quarantine (Instant rename, zero internet needed)
        if let Some(ref q_path_str) = proj.quarantine_path {
            let q_path = PathBuf::from(q_path_str);
            if q_path.exists() && q_path.is_dir() {
                if let Some(parent) = target_local_path.parent() {
                    std::fs::create_dir_all(parent)?;
                }
                Self::move_or_copy_dir(&q_path, &target_local_path)
                    .with_context(|| format!("Karantinadan geri taşıma hatası: {} -> {}", q_path.display(), target_local_path.display()))?;

                restored_from_quarantine = true;
                info!("Karantinadan anında geri yüklendi: {}", target_local_path.display());
            }
        }

        // Path 2: If quarantine is gone, restore from Google Drive remote archive with full verification
        if !restored_from_quarantine {
            let archive_path_str = proj.archive_remote_path.as_deref()
                .context("Arşiv uzak yolu bulunamadı")?;
            let archive_dir = PathBuf::from(archive_path_str);

            let verification = Self::archive_verify(&archive_dir)
                .with_context(|| format!("Arşiv doğrulaması başarısız oldu: {}", archive_dir.display()))?;

            if !verification.is_valid {
                bail!("Arşiv bozuk veya eksik: {:?}", verification.errors);
            }

            let manifest_path = archive_dir.join("manifest.json");
            let manifest_content = std::fs::read_to_string(&manifest_path)?;
            let manifest: OffloadManifest = serde_json::from_str(&manifest_content)?;

            let tar_gz_path = archive_dir.join("tree.tar.gz");
            let tree_dir = archive_dir.join("tree");
            let restore_res: Result<(usize, u64)> = (|| {
                let mut f_restored = 0;
                let mut b_restored = 0;
                std::fs::create_dir_all(&target_local_path)?;

                if tar_gz_path.exists() {
                    let file = std::fs::File::open(&tar_gz_path)?;
                    let gz = flate2::read::GzDecoder::new(file);
                    let mut archive = tar::Archive::new(gz);
                    archive.set_preserve_permissions(true);
                    archive.set_preserve_mtime(true);
                    archive.unpack(&target_local_path)?;

                    for entry in &manifest.entries {
                        if entry.kind == ManifestEntryKind::File {
                            let dst_entry = target_local_path.join(&entry.path);
                            if !dst_entry.exists() {
                                bail!("Geri yüklenen dosya bulunamadı: {}", dst_entry.display());
                            }
                            let hash = CacheManager::compute_sha256(&dst_entry)?;
                            if hash != entry.sha256 {
                                bail!("Geri yükleme bütünlük hatası: {}", dst_entry.display());
                            }
                            f_restored += 1;
                            b_restored += entry.size_bytes;
                        }
                    }
                } else if tree_dir.exists() {
                    for entry in &manifest.entries {
                        let src_entry = tree_dir.join(&entry.path);
                        let dst_entry = target_local_path.join(&entry.path);

                        match entry.kind {
                            ManifestEntryKind::Directory => {
                                std::fs::create_dir_all(&dst_entry)?;
                                let _ = std::fs::set_permissions(&dst_entry, std::fs::Permissions::from_mode(entry.mode));
                            }
                            ManifestEntryKind::Symlink => {
                                if let Some(ref target) = entry.symlink_target {
                                    if let Some(parent) = dst_entry.parent() {
                                        std::fs::create_dir_all(parent)?;
                                    }
                                    if dst_entry.symlink_metadata().is_ok() {
                                        let _ = std::fs::remove_file(&dst_entry);
                                    }
                                    std::os::unix::fs::symlink(target, &dst_entry)?;
                                }
                            }
                            ManifestEntryKind::File => {
                                if let Some(parent) = dst_entry.parent() {
                                    std::fs::create_dir_all(parent)?;
                                }

                                std::fs::copy(&src_entry, &dst_entry)?;
                                let _ = std::fs::set_permissions(&dst_entry, std::fs::Permissions::from_mode(entry.mode));
                                let mtime = filetime::FileTime::from_unix_time(entry.mtime, 0);
                                let _ = filetime::set_file_mtime(&dst_entry, mtime);

                                // Verify integrity
                                let hash = CacheManager::compute_sha256(&dst_entry)?;
                                if hash != entry.sha256 {
                                    bail!("Geri yükleme bütünlük hatası: {}", dst_entry.display());
                                }

                                f_restored += 1;
                                b_restored += entry.size_bytes;
                            }
                        }
                    }
                } else {
                    bail!("Arşiv içeriği (tree.tar.gz veya tree/) bulunamadı: {}", archive_dir.display());
                }

                Ok((f_restored, b_restored))
            })();

            let (f, b) = match restore_res {
                Ok(res) => res,
                Err(e) => {
                    // Hata durumunda yarım kalan hedef klasörü temizle ki sonraki denemeler kilitlenmesin
                    let _ = std::fs::remove_dir_all(&target_local_path);
                    return Err(e);
                }
            };

            files_restored = f;
            bytes_restored = b;

            restored_from_archive = true;
            info!("Drive arşivinden başarıyla geri yüklendi: {}", target_local_path.display());
        }

        // Rebuild execution if requested
        let mut rebuild_executed = false;
        let mut rebuild_output = None;

        if rebuild {
            let recipe_file = proj.archive_remote_path.as_ref()
                .map(|p| PathBuf::from(p).join("recipe.json"));

            if let Some(rf) = recipe_file
                && rf.exists()
                    && let Ok(content) = std::fs::read_to_string(&rf)
                        && let Ok(recipe) = serde_json::from_str::<Recipe>(&content) {
                            let mut outputs = Vec::new();
                            for cmd in &recipe.rebuild_commands {
                                info!("Yeniden kurulum komutu çalıştırılıyor: {}", cmd);
                                let output = std::process::Command::new("sh")
                                    .arg("-c")
                                    .arg(cmd)
                                    .current_dir(&target_local_path)
                                    .output();
                                match output {
                                    Ok(out) => {
                                        outputs.push(format!("$ {}\n{}", cmd, String::from_utf8_lossy(&out.stdout)));
                                    }
                                    Err(e) => {
                                        outputs.push(format!("$ {} (HATA: {})", cmd, e));
                                    }
                                }
                            }
                            rebuild_executed = true;
                            rebuild_output = Some(outputs.join("\n"));
                        }
        }

        // Update DB status to 'active'
        db.update_project_offload_info(
            project_id,
            "active",
            None,
            proj.archive_remote_path.as_deref(),
            None,
            None,
            None,
            0,
        )?;

        let _ = db.record_offload_journal(project_id, "unarchive", "success", "Project unarchived to local path");

        Ok(UnarchiveReport {
            project_id,
            project_name: proj.name,
            local_path: target_local_path.to_string_lossy().to_string(),
            restored_from_quarantine,
            restored_from_archive,
            files_restored,
            bytes_restored,
            rebuild_executed,
            rebuild_output,
        })
    }

    /// Purges local quarantine folder permanently after 7 days gate AND archive verification
    pub fn purge(
        db: &mut Database,
        project_id: i64,
        force: bool,
    ) -> Result<PurgeReport> {
        let proj = db.get_project_by_id(project_id)?
            .with_context(|| format!("Proje ID {} bulunamadı", project_id))?;

        if proj.status != "archived" {
            bail!("Yalnızca 'archived' durumundaki projelerin karantinası temizlenebilir.");
        }

        let q_path_str = proj.quarantine_path.as_deref()
            .context("Projede karantina yolu kayıtlı değil.")?;
        let q_path = PathBuf::from(q_path_str);

        if !q_path.exists() {
            bail!("Karantina klasörü fiziksel olarak bulunamadı: {}", q_path.display());
        }

        // Safety Gate 1: Ensure q_path is strictly inside default quarantine base dir or inside a quarantine directory
        let q_base = Self::default_quarantine_base_dir();
        let canonical_base = q_base.canonicalize().unwrap_or(q_base);
        let canonical_q = q_path.canonicalize().unwrap_or(q_path.clone());

        let is_in_default_quarantine = canonical_q.starts_with(&canonical_base) && canonical_q != canonical_base;
        let is_in_quarantine_dir = canonical_q.components().any(|c| c.as_os_str() == "quarantine") && canonical_q.file_name().map(|n| n != "quarantine").unwrap_or(false);

        if !is_in_default_quarantine && !is_in_quarantine_dir {
            bail!("Güvenlik Engeli: Karantina yolu güvenli karantina dizininin dışında!");
        }

        // Safety Gate 2: Archive Verification MUST succeed!
        let archive_path_str = proj.archive_remote_path.as_deref()
            .context("Doğrulama için uzak arşiv yolu bulunamadı")?;
        let verification = Self::archive_verify(Path::new(archive_path_str))
            .with_context(|| "Purge öncesi arşiv doğrulaması başarısız")?;

        if !verification.is_valid {
            bail!("Arşiv doğrulanmadı! Veri kaybını önlemek için karantina ASLA silinemez: {:?}", verification.errors);
        }

        // Safety Gate 3: Time gate (7 days quarantine duration) unless forced
        if let Some(ref until_str) = proj.quarantine_until
            && let Ok(until_dt) = DateTime::parse_from_rfc3339(until_str) {
                let now = Utc::now();
                if now < until_dt && !force {
                    bail!(
                        "Karantina süresi henüz dolmadı (Kalan süre: {}). Zorla silmek için --force kullanın.",
                        until_dt
                    );
                }
            }

        // Measure size of quarantine before deletion
        let mut freed_bytes = 0u64;
        for entry in walkdir::WalkDir::new(&q_path).into_iter().filter_map(|e| e.ok()) {
            if entry.file_type().is_file()
                && let Ok(m) = entry.metadata() {
                    freed_bytes += m.len();
                }
        }

        // Delete quarantine directory
        std::fs::remove_dir_all(&q_path)
            .with_context(|| format!("Karantina silinemedi: {}", q_path.display()))?;

        // Update DB
        db.update_project_offload_info(
            project_id,
            "purged",
            proj.archived_at.as_deref(),
            proj.archive_remote_path.as_deref(),
            None,
            None,
            proj.manifest_sha256.as_deref(),
            proj.freed_bytes + freed_bytes as i64,
        )?;

        let _ = db.record_offload_journal(project_id, "purge", "success", "Quarantine purged");

        info!("Karantina kalıcı olarak silindi: {}. Açılan ek alan: {} bayt", q_path.display(), freed_bytes);

        Ok(PurgeReport {
            project_id,
            project_name: proj.name,
            deleted_quarantine_path: q_path.to_string_lossy().to_string(),
            freed_bytes,
        })
    }

    /// Purges all expired quarantine folders across all projects in one batch
    pub fn purge_expired(
        db: &mut Database,
        force: bool,
    ) -> Result<Vec<PurgeReport>> {
        let projects = db.list_projects()?;
        let now = Utc::now();
        let mut reports = Vec::new();

        for proj in projects {
            if proj.status == "archived" && proj.quarantine_path.is_some() {
                let should_purge = if force {
                    true
                } else if let Some(ref until_str) = proj.quarantine_until {
                    if let Ok(until_dt) = DateTime::parse_from_rfc3339(until_str) {
                        now >= until_dt
                    } else {
                        false
                    }
                } else {
                    false
                };

                if should_purge {
                    match Self::purge(db, proj.id, force) {
                        Ok(rep) => {
                            info!("Süresi dolan karantina başarıyla temizlendi: {}", proj.name);
                            reports.push(rep);
                        }
                        Err(e) => {
                            warn!("Karantina temizlenirken hata oluştu ({}): {}", proj.name, e);
                        }
                    }
                }
            }
        }

        Ok(reports)
    }

    /// Moves a directory atomically if possible; falls back to cross-filesystem copy + remove if EXDEV occurs.
    pub fn move_or_copy_dir(src: &Path, dst: &Path) -> Result<()> {
        if let Some(parent) = dst.parent() {
            std::fs::create_dir_all(parent)?;
        }
        match std::fs::rename(src, dst) {
            Ok(()) => Ok(()),
            Err(e) => {
                warn!(
                    "Rename ({}) başarısız. Farklı disk bölümü (EXDEV) tespit edildi, kopyalama ile aktarılıyor: {} -> {}",
                    e,
                    src.display(),
                    dst.display()
                );
                Self::copy_dir_recursive(src, dst)?;
                std::fs::remove_dir_all(src)
                    .with_context(|| format!("Kopyalama sonrası kaynak dizin temizlenemedi: {}", src.display()))?;
                Ok(())
            }
        }
    }

    /// Recursively copies directory tree preserving files and symlinks.
    pub fn copy_dir_recursive(src: &Path, dst: &Path) -> Result<()> {
        std::fs::create_dir_all(dst)?;
        for entry in walkdir::WalkDir::new(src).into_iter().filter_map(|e| e.ok()) {
            let rel = match entry.path().strip_prefix(src) {
                Ok(r) => r,
                Err(_) => continue,
            };
            if rel.as_os_str().is_empty() {
                continue;
            }
            let target = dst.join(rel);
            let ft = entry.file_type();
            if ft.is_dir() {
                std::fs::create_dir_all(&target)?;
            } else if ft.is_symlink() {
                if let Ok(link_target) = std::fs::read_link(entry.path()) {
                    if target.symlink_metadata().is_ok() {
                        let _ = std::fs::remove_file(&target);
                    }
                    #[cfg(unix)]
                    std::os::unix::fs::symlink(&link_target, &target)?;
                }
            } else if ft.is_file() {
                if let Some(parent) = target.parent() {
                    std::fs::create_dir_all(parent)?;
                }
                std::fs::copy(entry.path(), &target)
                    .with_context(|| format!("Kopyalama hatası: {} -> {}", entry.path().display(), target.display()))?;
            }
        }
        Ok(())
    }

    /// Yalnızca Sınıf A (node_modules, target, .venv vb.) yeniden üretilebilir klasörlerin
    /// toplam boyutunu metadata üzerinden ölçer.
    /// Dosya içeriklerini okumaz, SHA-256 hesaplamaz ve .git dizinine girmez (/api/status için).
    pub fn measure_reclaimable_bytes<P: AsRef<Path>>(project_path: P) -> Result<u64> {
        let p_path = project_path.as_ref();
        if !p_path.exists() || !p_path.is_dir() {
            return Ok(0);
        }

        let canonical_project = p_path.canonicalize()?;
        let rules = DevFilter::reproducible_rules();
        let mut class_a_prefixes: HashSet<PathBuf> = HashSet::new();
        let mut total_bytes = 0u64;

        let mut it = walkdir::WalkDir::new(&canonical_project)
            .min_depth(1)
            .max_depth(4)
            .into_iter();

        while let Some(entry_res) = it.next() {
            let entry = match entry_res {
                Ok(e) => e,
                Err(_) => continue,
            };

            if entry.file_type().is_dir() {
                if entry.file_name() == ".git" {
                    it.skip_current_dir();
                    continue;
                }

                let entry_path = entry.path();
                if let Ok(rel) = entry_path.strip_prefix(&canonical_project) {
                    if class_a_prefixes.iter().any(|prefix| rel.starts_with(prefix)) {
                        it.skip_current_dir();
                        continue;
                    }

                    let dir_name = entry.file_name().to_string_lossy();
                    let depth = rel.components().count();

                    if let Some(rule) = rules.iter().find(|r| {
                        if r.is_root_only && depth != 1 {
                            false
                        } else {
                            r.dir_name == dir_name
                        }
                    }) {
                        let has_marker = rule.required_markers.is_empty()
                            || rule
                                .required_markers
                                .iter()
                                .any(|&m| canonical_project.join(m).exists());

                        if has_marker {
                            for f in walkdir::WalkDir::new(entry_path)
                                .into_iter()
                                .filter_map(|e| e.ok())
                            {
                                if (f.file_type().is_file() || f.file_type().is_symlink())
                                    && let Ok(m) = f.metadata() {
                                        total_bytes += m.len();
                                    }
                            }
                            class_a_prefixes.insert(rel.to_path_buf());
                            it.skip_current_dir();
                        }
                    } else if crate::filter::is_ephemeral_dir_by_signature(entry_path) {
                        it.skip_current_dir();
                        continue;
                    }
                }
            }
        }

        Ok(total_bytes)
    }
}
