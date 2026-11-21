use anyhow::{bail, Context, Result};
use clap::{Parser, Subcommand};
use colored::*;
use devdrive::cache::CacheManager;
use devdrive::config::DevDriveConfig;
use devdrive::db::Database;
use devdrive::filter::{DevFilter, FileCategory};
use devdrive::offload::OffloadEngine;
use devdrive::snapshot::SnapshotEngine;
use devdrive::sync::SyncEngine;
use devdrive::watcher::WorkspaceWatcher;
use std::path::{Path, PathBuf};
use walkdir::WalkDir;

#[derive(Parser)]
#[command(
    name = "devdrive",
    author = "DevDrive Team",
    version = "0.1.0",
    about = "⚡ Developer-Aware Smart Virtual Filesystem & Tiered Caching for macOS"
)]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Show disk status, cache consumption, and tracked projects
    Status,

    /// Register a project directory under DevDrive protection
    Init {
        /// Path to the project directory
        path: PathBuf,
        /// Optional project name
        #[arg(short, long)]
        name: Option<String>,
    },

    /// Start the real-time FSEvents watcher on a workspace or all projects
    Watch {
        /// Path to the project directory (optional if --all is used)
        #[arg(default_value = ".")]
        path: PathBuf,
        /// Watch all registered projects simultaneously with Smart Auto-Sync
        #[arg(short, long)]
        all: bool,
    },

    /// Run the DevDrive background daemon (monitors all projects with Smart Auto-Sync)
    Daemon,

    /// Create an instant point-in-time micro-snapshot (Time-Travel)
    Snapshot {
        /// Path to the project directory
        path: PathBuf,
        /// Snapshot message
        #[arg(short, long, default_value = "Manual snapshot")]
        message: String,
    },

    /// List all micro-snapshots for a project
    ListSnapshots {
        /// Path to the project directory
        path: PathBuf,
    },

    /// Roll back project to a specific micro-snapshot
    Rollback {
        /// Path to the project directory
        path: PathBuf,
        /// Snapshot ID to restore
        snapshot_id: String,
    },

    /// Synchronize changes to Google Drive
    Sync {
        /// Path to the project directory
        path: PathBuf,
        /// Optional custom remote drive directory
        #[arg(short, long)]
        remote: Option<PathBuf>,
        /// Push all tracked source files to remote drive (legacy file-by-file mode)
        #[arg(short, long)]
        all: bool,
        /// Prune remote files that were deleted locally (moves to remote .trash) (legacy mode)
        #[arg(short, long)]
        prune: bool,
        /// Force action without interactive confirmation / override safety gates
        #[arg(short, long)]
        force: bool,
        /// Use legacy file-by-file sync instead of default fast bundle packaging
        #[arg(long)]
        legacy_files: bool,
    },

    /// List available projects backed up in the remote Google Drive directory
    ListRemote,

    /// Restore a project from Google Drive backup into a local target directory
    Restore {
        /// Remote project name or path
        remote: String,
        /// Target local directory to restore files into
        target: PathBuf,
        /// Force restore even if target directory or files already exist
        #[arg(short, long)]
        force: bool,
    },

    /// Clean cold items from local cache
    CacheClean,

    /// Scan project(s) to measure reproducible artifacts (Class A) and reclaimable SSD space
    Scan {
        /// Optional project paths to scan (defaults to all registered projects)
        paths: Vec<PathBuf>,
    },

    /// Clean Class A reproducible build/dependency artifacts (node_modules, target, .venv)
    CleanArtifacts {
        /// Project name or path
        project: String,
        /// Show what will be removed without actually deleting
        #[arg(long)]
        dry_run: bool,
    },

    /// Offload project to Google Drive archive and local 7-day quarantine (frees SSD space safely)
    Offload {
        /// Project name or path
        project: String,
        /// Show dry-run summary without executing offload
        #[arg(long)]
        dry_run: bool,
        /// Confirm offload execution
        #[arg(short, long)]
        yes: bool,
        /// Include .env files in archive (default from config)
        #[arg(long)]
        backup_env: Option<bool>,
        /// Bypass confirmation warning if .env files are not backed up
        #[arg(long)]
        ignore_env_warning: bool,
    },

    /// Restore an offloaded project to local SSD from quarantine or Google Drive archive
    Unarchive {
        /// Project name or path
        project: String,
        /// Rebuild dependencies after restoring (e.g. npm ci, cargo build)
        #[arg(short, long)]
        rebuild: bool,
    },

    /// Verify integrity of remote Google Drive archive
    ArchiveVerify {
        /// Project name or path
        project: String,
    },

    /// Compare current local project files against a snapshot before rolling back
    Diff {
        /// Snapshot ID to compare against (e.g. snap_20261014_...)
        snapshot_id: String,
        /// Path to the local project (defaults to current directory)
        #[arg(default_value = ".")]
        project: String,
    },

    /// Permanently purge local quarantine folder for an archived project
    Purge {
        /// Project name or path (optional if --expired is used)
        #[arg(default_value = "")]
        project: String,
        /// Force purge before the 7-day quarantine period expires
        #[arg(short, long)]
        force: bool,
        /// Purge all expired quarantine folders across all projects
        #[arg(long)]
        expired: bool,
    },

    /// Launch the graphical web dashboard & visual control panel
    Ui {
        /// Port to bind the dashboard server on
        #[arg(short, long, default_value_t = 4040)]
        port: u16,
        /// Do not automatically open the browser
        #[arg(long)]
        no_open: bool,
    },

    /// Manage background system service (macOS LaunchAgent)
    Service {
        #[command(subcommand)]
        command: ServiceCommands,
    },
}

#[derive(clap::Subcommand, Debug)]
pub enum ServiceCommands {
    /// Install and launch DevDrive Web UI as a background LaunchAgent service
    Install {
        /// Port to bind the dashboard server on
        #[arg(short, long, default_value_t = 4040)]
        port: u16,
    },
    /// Stop and uninstall the background LaunchAgent service
    Uninstall,
    /// Check the status of the background LaunchAgent service
    Status,
}

fn format_bytes(bytes: u64) -> String {
    const KB: u64 = 1024;
    const MB: u64 = KB * 1024;
    const GB: u64 = MB * 1024;

    if bytes >= GB {
        format!("{:.2} GB", bytes as f64 / GB as f64)
    } else if bytes >= MB {
        format!("{:.2} MB", bytes as f64 / MB as f64)
    } else if bytes >= KB {
        format!("{:.2} KB", bytes as f64 / KB as f64)
    } else {
        format!("{} B", bytes)
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt::init();
    let cli = Cli::parse();
    let config = DevDriveConfig::load()?;
    let db_path = Database::default_path();
    let cache_dir = CacheManager::default_cache_dir();
    let cache_mgr = CacheManager::new(&cache_dir, config.max_cache_bytes())?;

    match cli.command {
        Commands::Status => {
            println!("{}", "═════════════════════════════════════════════════════════════".cyan().bold());
            println!("   {}", "⚡ DevDrive: Developer-Aware Tiered Cache & Sync".bold().green());
            println!("{}", "═════════════════════════════════════════════════════════════".cyan().bold());

            let current_cache = cache_mgr.current_cache_usage()?;
            let max_cache = config.max_cache_bytes();
            let cache_pct = (current_cache as f64 / max_cache as f64) * 100.0;

            println!(
                " 💾 {}: {} / {} ({:.1}%)",
                "Yerel Sıcak SSD Önbelleği".bold(),
                format_bytes(current_cache).yellow(),
                format_bytes(max_cache).cyan(),
                cache_pct
            );
            println!(
                " ☁️  {}: {}",
                "Bulut Hedefi (Google Drive)".bold(),
                config.remote_drive_base_path.blue()
            );

            if db_path.exists() {
                let db = Database::open(&db_path)?;
                let projects = db.list_projects()?;
                println!("\n 📂 {}:", "Kayıtlı Projeler".bold().magenta());
                if projects.is_empty() {
                    println!("    (Henüz kayıtlı proje yok. `devdrive init <yol>` ile ekleyebilirsiniz)");
                } else {
                    for p in projects {
                        println!(
                            "    • {} [{}]\n      ↳ {}",
                            p.name.bold(),
                            p.created_at.dimmed(),
                            p.local_path
                        );
                    }
                }
            } else {
                println!("\n ℹ️  Veritabanı henüz başlatılmadı. İlk projeyi eklemek için `devdrive init` kullanın.");
            }
            println!("{}", "═════════════════════════════════════════════════════════════".cyan().bold());
        }

        Commands::Init { path, name } => {
            let canonical = path.canonicalize().context("Could not resolve path")?;
            let project_name = name.unwrap_or_else(|| {
                canonical
                    .file_name()
                    .map(|n| n.to_string_lossy().to_string())
                    .unwrap_or_else(|| "project".to_string())
            });

            devdrive::sync::validate_project_name(&project_name)?;

            let remote_dest = PathBuf::from(&config.remote_drive_base_path).join(&project_name);
            let mut db = Database::open(&db_path)?;

            // Prevent collision: check if remote_dest is already registered to a different local path
            if let Some(existing) = db.get_project_by_remote_path(&remote_dest.to_string_lossy())?
                && existing.local_path != canonical.to_string_lossy()
            {
                anyhow::bail!(
                    "Hedef uzak klasör ('{}') zaten başka bir projeye ('{}' -> '{}') atanmış! Çakışmayı önlemek için lütfen --name ile farklı bir ad belirtin.",
                    remote_dest.display(),
                    existing.name,
                    existing.local_path
                );
            }

            // Check if remote destination exists on disk and is associated with another local path
            if remote_dest.exists()
                && let Some(meta) = devdrive::sync::read_devdrive_meta(&remote_dest)
                && meta.original_local_path != canonical.to_string_lossy()
            {
                let existing_in_db = db.get_project_by_local_path(&canonical.to_string_lossy())?;
                let is_same = existing_in_db.as_ref().map(|p| p.remote_path == remote_dest.to_string_lossy()).unwrap_or(false);
                if !is_same {
                    anyhow::bail!(
                        "Hedef uzak klasör ('{}') diskte mevcut ve başka bir projeye ('{}' -> '{}') ait! Çakışmayı önlemek için lütfen --name ile farklı bir ad belirtin.",
                        remote_dest.display(),
                        meta.project_name,
                        meta.original_local_path
                    );
                }
            }

            let proj = db.register_project(
                &project_name,
                &canonical.to_string_lossy(),
                &remote_dest.to_string_lossy(),
            )?;

            // Write metadata file to remote directory
            let _ = devdrive::sync::write_devdrive_meta(
                &remote_dest,
                &project_name,
                &canonical.to_string_lossy(),
            );

            println!(
                "{} Proje DevDrive korumasına alındı: {} ({})",
                "✔".green().bold(),
                project_name.bold(),
                canonical.display()
            );

            let filter = DevFilter::for_project(&canonical);
            let mut source_files = 0;
            let mut ephemeral_files = 0;
            let mut ephemeral_bytes = 0u64;

            for entry in WalkDir::new(&canonical).into_iter().filter_map(|e| e.ok()) {
                if entry.file_type().is_file()
                    && let Ok(rel) = entry.path().strip_prefix(&canonical) {
                        match filter.categorize(rel) {
                            FileCategory::Source => {
                                source_files += 1;
                                if let Ok(meta) = entry.metadata() {
                                    let hash = CacheManager::compute_sha256(entry.path())
                                        .unwrap_or_default();
                                    let now = chrono::Utc::now().timestamp();
                                    let _ = db.upsert_file_metadata(
                                        proj.id,
                                        &rel.to_string_lossy(),
                                        meta.len() as i64,
                                        &hash,
                                        "source",
                                        now,
                                        now,
                                        false,
                                    );
                                }
                            }
                            FileCategory::Ephemeral => {
                                ephemeral_files += 1;
                                if let Ok(meta) = entry.metadata() {
                                    ephemeral_bytes += meta.len();
                                }
                            }
                            FileCategory::TransientLock => {}
                        }
                    }
            }

            // Predictive cache warming
            let sync = SyncEngine::new(&mut db, &cache_mgr);
            let warmed = sync.warm_cache(&canonical)?;

            println!(" 📊 Taranan Kaynak Dosyaları: {}", source_files.to_string().cyan());
            println!(
                " 🛡️  Dev-Shield ile Engellenen Çöp (node_modules/target vb.): {} dosya ({})",
                ephemeral_files.to_string().yellow(),
                format_bytes(ephemeral_bytes).green().bold()
            );
            println!(" 🔥 Ön Isıtılan (Warmed) Dosyalar: {}", warmed.to_string().green());
            println!("\nİzlemeyi başlatmak için: {} {}", "devdrive watch".bold(), canonical.display());
        }

        Commands::Watch { path, all } => {
            if all {
                println!("{}", "═════════════════════════════════════════════════════════════".cyan().bold());
                println!("   {}", "⚡ DevDrive Akıllı Otomatik Eşitleme İzleyicisi (Tüm Projeler)".bold().green());
                println!("{}", "═════════════════════════════════════════════════════════════".cyan().bold());
                println!(" 🔋 Pil Koruması: {}", if config.battery_guard_enabled { "Aktif (<%20 şarjda ertele)".green() } else { "Devre Dışı".yellow() });
                println!(" ⏳ Boşta Kalma Süresi: {} dakika", config.auto_sync_idle_minutes.to_string().cyan());
                println!(" 🍃 Sistem Önceliği: macOS Background QoS (Düşük Öncelik, %0 CPU Boşta)");
                println!("\n Kayıtlı tüm projeler izleniyor. Durdurmak için Ctrl+C.");

                devdrive::watcher::run_multi_project_watcher(&db_path, &config, None)?;
            } else {
                let canonical = path.canonicalize()?;
                let watcher = WorkspaceWatcher::new(&canonical)?;
                println!(
                    "{} FSEvents izleyici başlatılıyor: {} (Boşta Kalma: {} dk)",
                    "⚡".yellow().bold(),
                    canonical.display(),
                    config.auto_sync_idle_minutes
                );
                watcher.watch_loop_with_config(&db_path, &config)?;
            }
        }

        Commands::Daemon => {
            println!("{}", "═════════════════════════════════════════════════════════════".cyan().bold());
            println!("   {}", "⚡ DevDrive Smart Auto-Sync Background Daemon".bold().green());
            println!("{}", "═════════════════════════════════════════════════════════════".cyan().bold());
            println!(" 🔋 Pil Koruması: {}", if config.battery_guard_enabled { "Aktif (<%20 şarjda otomatik eşitleme ertelenir)".green() } else { "Devre Dışı".yellow() });
            println!(" ⏳ Boşta Kalma Eşiği: {} dakika (kesintisiz değişiklik olmama şartı)", config.auto_sync_idle_minutes.to_string().cyan());
            println!(" 🍃 Sistem Önceliği: macOS Background QoS (Düşük Öncelik, %0 CPU Boşta)");
            println!(" ☁️  Eşitleme Modu: Bundle (tar.gz + manifest.json)");
            println!("\n Kayıtlı tüm projeler arka planda FSEvents ile izleniyor. Durdurmak için Ctrl+C.");

            devdrive::watcher::run_multi_project_watcher(&db_path, &config, None)?;
        }

        Commands::Snapshot { path, message } => {
            let canonical = path.canonicalize()?;
            let mut db = Database::open(&db_path)?;
            let mut engine = SnapshotEngine::new(&mut db, &cache_mgr);

            let manifest = engine.create_snapshot(&canonical, &message)?;
            println!(
                "{} Zaman Makinesi: Yeni anlık görüntü kaydedildi!",
                "📸".bold()
            );
            println!("  • ID: {}", manifest.id.yellow().bold());
            println!("  • Mesaj: {}", manifest.message.cyan());
            println!(
                "  • Kapsam: {} kaynak dosya ({})",
                manifest.files.len(),
                format_bytes(manifest.total_size_bytes)
            );
        }

        Commands::ListSnapshots { path } => {
            let canonical = path.canonicalize()?;
            let db = Database::open(&db_path)?;
            let proj = db
                .get_project_by_local_path(&canonical.to_string_lossy())?
                .context("Project not registered")?;

            let snapshots = db.list_snapshots(proj.id)?;
            println!(
                "📸 {} için Kaydedilen Anlık Görüntüler ({}):",
                proj.name.bold(),
                snapshots.len()
            );
            for s in snapshots {
                println!(
                    "  • {} [{}] - {}",
                    s.id.yellow().bold(),
                    s.created_at.dimmed(),
                    s.message.cyan()
                );
            }
        }

        Commands::Rollback { path, snapshot_id } => {
            let canonical = path.canonicalize()?;
            let mut db = Database::open(&db_path)?;
            let mut engine = SnapshotEngine::new(&mut db, &cache_mgr);

            engine.rollback_to_snapshot(&canonical, &snapshot_id)?;
            println!(
                "{} Proje başarıyla '{}' anlık görüntüsüne geri döndürüldü!",
                "✔".green().bold(),
                snapshot_id.yellow()
            );
        }

        Commands::Sync { path, remote, all, prune, force, legacy_files } => {
            let canonical = path.canonicalize()?;
            let mut db = Database::open(&db_path)?;
            let project_str = canonical.to_string_lossy().to_string();
            let proj = db
                .get_project_by_local_path(&project_str)?
                .context("Project not registered")?;

            let remote_path = remote.unwrap_or_else(|| PathBuf::from(&proj.remote_path));

            let mut sync = SyncEngine::new(&mut db, &cache_mgr);

            if legacy_files {
                println!(
                    "{} Değişiklikler Google Drive hedefine eşitleniyor (Legacy Dosya Modu): {}",
                    "🔄".cyan().bold(),
                    remote_path.display()
                );

                let report = if all || prune {
                    if all {
                        println!("📦 Tüm temiz kaynak dosyaları taranıyor ve eşitleniyor...");
                    }
                    if prune {
                        println!("🧹 Prune devrede: Yerelde silinmiş uzak dosyalar .trash kasasına taşınıyor...");
                    }
                    sync.sync_all_with_options(&canonical, &remote_path, config.auto_snapshot_on_sync, prune, force, |_, _| {})?
                } else {
                    sync.sync_pending(&canonical, &remote_path, config.auto_snapshot_on_sync)?
                };

                if let Some(snap_id) = &report.auto_snapshot_id {
                    println!(" 📸 {}: Eşitleme öncesi anlık görüntü kilitlendi ({})", "Demir Kalkan".green().bold(), snap_id.yellow());
                }
                if report.conflicts_detected > 0 {
                    println!(" ⚠️  {} adet çakışma tespit edildi! Dosyalar ezilmedi, .conflict kopyası oluşturuldu.", report.conflicts_detected.to_string().red().bold());
                }

                println!(
                    "{} Senkronizasyon Tamamlandı: {} dosya yüklendi, {} dosya kasaya taşındı ({})",
                    "✔".green().bold(),
                    report.files_uploaded,
                    report.files_deleted,
                    format_bytes(report.bytes_transferred).green()
                );
            } else {
                println!(
                    "{} Değişiklikler Google Drive hedefine paketlenerek eşitleniyor (Bundle Sync): {}",
                    "📦".cyan().bold(),
                    remote_path.display()
                );

                let report = sync.sync_bundle(&canonical, &remote_path, config.auto_snapshot_on_sync)?;

                if let Some(snap_id) = &report.auto_snapshot_id {
                    println!(" 📸 {}: Eşitleme öncesi anlık görüntü kilitlendi ({})", "Demir Kalkan".green().bold(), snap_id.yellow());
                }

                println!(
                    "{} Senkronizasyon Tamamlandı (Bundle Sync): {} dosya paketlendi, bundle.tar.gz boyutu: {} (Orijinal: {})",
                    "✔".green().bold(),
                    report.files_packaged.to_string().cyan().bold(),
                    format_bytes(report.bundle_bytes).green().bold(),
                    format_bytes(report.original_bytes).yellow()
                );
                println!("  • Arşiv:    {}", report.bundle_path.dimmed());
                println!("  • Manifest: {}", report.manifest_path.dimmed());
            }
        }

        Commands::ListRemote => {
            println!("{}", "═════════════════════════════════════════════════════════════".cyan().bold());
            println!("   {}", "☁️  DevDrive: Uzak Drive Projeleri".bold().green());
            println!("{}", "═════════════════════════════════════════════════════════════".cyan().bold());
            println!(" 📂 Konum: {}\n", config.remote_drive_base_path.blue());

            let remotes = devdrive::sync::list_remote_projects(&config.remote_drive_base_path)?;
            if remotes.is_empty() {
                println!("  (Uzak Drive üzerinde henüz yedeklenmiş proje bulunamadı)");
            } else {
                for r in remotes {
                    println!(
                        "  • {} ({} dosya, {})",
                        r.name.bold().green(),
                        r.file_count.to_string().cyan(),
                        format_bytes(r.total_bytes).yellow()
                    );
                    println!("    ↳ Yol: {}", r.path.dimmed());
                    if let Some(orig) = r.original_local_path {
                        println!("    ↳ İlk Yerel Konum: {}", orig.dimmed());
                    }
                    if let Some(lm) = r.last_modified {
                        println!("    ↳ Son Değişiklik: {}", lm.dimmed());
                    }
                    println!();
                }
                println!("Bir projeyi yerel klasöre geri getirmek için:");
                println!("  {} <proje_adı> <hedef_yol>", "devdrive restore".bold());
            }
            println!("{}", "═════════════════════════════════════════════════════════════".cyan().bold());
        }

        Commands::Restore { remote, mut target, force } => {
            let remote_path = if (remote.starts_with('/') || remote.starts_with('\\') || remote.starts_with('~'))
                && std::path::Path::new(&remote).exists()
            {
                PathBuf::from(&remote)
            } else {
                PathBuf::from(&config.remote_drive_base_path).join(&remote)
            };

            if !remote_path.exists() {
                anyhow::bail!(
                    "Uzak yedek klasörü bulunamadı: {}. Mevcut projeleri görmek için `devdrive list-remote` komutunu çalıştırın.",
                    remote_path.display()
                );
            }

            let project_name = remote_path.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_else(|| remote.clone());
            if target.is_dir() {
                let last_name = target.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
                if last_name != project_name {
                    target.push(&project_name);
                }
            }

            println!("{}", "═════════════════════════════════════════════════════════════".cyan().bold());
            println!("   {}", "📦 DevDrive: Yedekten Geri Getirme (Restore)".bold().green());
            println!("{}", "═════════════════════════════════════════════════════════════".cyan().bold());
            println!(" ☁️  Kaynak (Drive): {}", remote_path.display().to_string().blue());
            println!(" 💻 Hedef (Yerel):  {}", target.display().to_string().yellow());
            if force {
                println!(" ⚠️  Zorlama Modu: {}", "--force aktif (mevcut dosyalar yedeklenip ezilecek)".red().bold());
            }
            println!();

            let mut db = Database::open(&db_path)?;
            let mut sync = SyncEngine::new(&mut db, &cache_mgr);

            let report = if remote_path.join("bundle.tar.gz").exists() || remote_path.join("tree.tar.gz").exists() {
                devdrive::sync::restore_project_bundle(&remote_path, &target)?
            } else {
                sync.restore_from_remote(&remote_path, &target, force)?
            };

            println!(
                "{} Geri getirme başarıyla tamamlandı!",
                "✔".green().bold()
            );
            println!("  • Geri getirilen dosya sayısı: {}", report.files_restored.to_string().green().bold());
            if report.files_skipped > 0 {
                println!("  • Atlanan (zaten güncel):       {}", report.files_skipped.to_string().yellow());
            }
            println!("  • Toplam boyut:                {}", format_bytes(report.bytes).cyan());

            if !report.hints.is_empty() {
                println!("\n{}", "💡 Sonraki Adımlar & İpuçları:".bold().magenta());
                for hint in report.hints {
                    println!("  → {}", hint);
                }
            }
            println!("{}", "═════════════════════════════════════════════════════════════".cyan().bold());
        }

        Commands::CacheClean => {
            let usage = cache_mgr.current_cache_usage()?;
            let max = config.max_cache_bytes();
            println!(
                "Mevcut önbellek: {} / Limit: {}",
                format_bytes(usage).yellow(),
                format_bytes(max).cyan()
            );

            if usage > max {
                println!("⚠️  Önbellek sınırı aşıldı, soğuk nesneler (LRU) tahliye ediliyor...");
                let freed = cache_mgr.evict_lru()?;
                let new_usage = cache_mgr.current_cache_usage()?;
                println!(
                    "{} Önbellek tahliyesi tamamlandı: {} temizlendi. Yeni boyut: {}",
                    "✔".green().bold(),
                    format_bytes(freed).green(),
                    format_bytes(new_usage).cyan()
                );
            } else {
                println!("{} Önbellek optimal düzeyde, temizleme gerekmiyor.", "✔".green());
            }
        }

        Commands::Scan { paths } => {
            println!("{}", "═════════════════════════════════════════════════════════════".cyan().bold());
            println!("   {}", "🔍 DevDrive: Sınıf A (Yeniden Üretilebilir) Artifakt Taraması".bold().green());
            println!("{}", "═════════════════════════════════════════════════════════════".cyan().bold());

            let mut targets = Vec::new();
            if !paths.is_empty() {
                targets.extend(paths);
            } else {
                let db = Database::open(&db_path)?;
                for p in db.list_projects()? {
                    let path = PathBuf::from(&p.local_path);
                    if path.exists() {
                        targets.push(path);
                    }
                }
            }

            if targets.is_empty() {
                println!("Tarama yapılacak proje bulunamadı. Lütfen bir yol belirtin veya devdrive init ile proje ekleyin.");
                return Ok(());
            }

            let mut grand_total_reclaimable = 0u64;

            for t in &targets {
                println!("\n📂 Proje: {}", t.display().to_string().bold());
                match OffloadEngine::scan_project(t, &config, false) {
                    Ok(report) => {
                        println!("   • Yeniden üretilebilir (Sınıf A): {}", format_bytes(report.class_a_total_bytes).yellow().bold());
                        for item in &report.class_a_items {
                            println!("     - {} ({}) -> {}", item.path.cyan(), format_bytes(item.size_bytes), item.rebuild_command.dimmed());
                        }
                        println!("   • Değerli kaynak & ayarlar (Sınıf B): {}", format_bytes(report.class_b_total_bytes).blue());
                        if !report.class_c_blockers.is_empty() {
                            println!("   ⚠️  {} Sınıf C Engelleyici Tespit Edildi:", report.class_c_blockers.len().to_string().red().bold());
                            for b in &report.class_c_blockers {
                                println!("     ❌ {}", b.red());
                            }
                        }
                        grand_total_reclaimable += report.class_a_total_bytes;
                    }
                    Err(e) => {
                        println!("   ❌ Tarama hatası: {}", e.to_string().red());
                    }
                }
            }

            println!("\n{}", "─────────────────────────────────────────────────────────────".dimmed());
            println!("🚀 SSD'den Açılabilir Toplam Alan: {}", format_bytes(grand_total_reclaimable).green().bold());
        }

        Commands::CleanArtifacts { project, dry_run } => {
            let db = Database::open(&db_path)?;
            let p_path = if let Ok(Some(proj)) = db.get_project_by_name(&project) {
                PathBuf::from(proj.local_path)
            } else {
                PathBuf::from(&project)
            };

            let report = OffloadEngine::clean_artifacts(&p_path, dry_run)?;
            if dry_run {
                println!("🔍 [Dry-Run] Temizlenebilecek Artifaktlar: '{}'", report.project_name.bold());
                for item in &report.cleaned_items {
                    println!("   - {} ({})", item.path.yellow(), format_bytes(item.size_bytes));
                }
                println!("💾 Toplam açılabilecek alan: {}", format_bytes(report.total_freed_bytes).green().bold());
                println!("(Gerçek silme için --dry-run olmadan çalıştırın)");
            } else {
                println!("✔ '{}' projesinin Sınıf A artifaktları temizlendi!", report.project_name.green().bold());
                println!("💾 SSD'de açılan alan: {}", format_bytes(report.total_freed_bytes).green().bold());
            }
        }

        Commands::Offload { project, dry_run, yes, backup_env, ignore_env_warning } => {
            let mut db = Database::open(&db_path)?;
            let proj = if let Ok(Some(p)) = db.get_project_by_name(&project) {
                p
            } else if let Ok(Some(p)) = db.get_project_by_local_path(&project) {
                p
            } else {
                bail!("Kayıtlı proje bulunamadı: '{}'", project);
            };

            let mut custom_cfg = config.clone();
            if let Some(be) = backup_env {
                custom_cfg.backup_env_files = be;
            }

            let is_dry_run = dry_run || !yes;

            if is_dry_run {
                println!("🔍 [Dry-Run] '{}' Projesi Tahliye Simülasyonu", proj.name.bold());
                let report = OffloadEngine::offload(&mut db, proj.id, &custom_cfg, true, ignore_env_warning)?;
                println!("   • Silinecek (Sınıf A): {}", format_bytes(report.class_a_freed_bytes).green().bold());
                println!("   • Arşivlenecek (Sınıf B): {} dosya ({})", report.class_b_file_count, format_bytes(report.class_b_archived_bytes));
                println!("   • Arşiv Hedefi: {}", report.archive_remote_path.blue());
                if let Some(ref q) = report.quarantine_path {
                    println!("   • Yerel Karantina: {}", q.cyan());
                }
                println!("\n💡 Gerçek tahliye için '--yes' bayrağını ekleyin: devdrive offload {} --yes", proj.name);
            } else {
                println!("🚀 '{}' projesi Google Drive'a tahliye ediliyor...", proj.name.bold());
                let report = OffloadEngine::offload(&mut db, proj.id, &custom_cfg, false, ignore_env_warning)?;
                println!("✔ Tahliye tamamlandı!");
                println!("   • SSD'de Anında Açılan Alan: {}", format_bytes(report.class_a_freed_bytes).green().bold());
                println!("   • Arşivlenen Dosyalar: {} dosya ({})", report.class_b_file_count, format_bytes(report.class_b_archived_bytes));
                println!("   • Drive Arşivi: {}", report.archive_remote_path.blue());
                if let Some(ref q) = report.quarantine_path {
                    println!("   • Yerel Karantina (7 Gün Güvencesi): {}", q.cyan());
                }
            }
        }

        Commands::Unarchive { project, rebuild } => {
            let mut db = Database::open(&db_path)?;
            let proj = if let Ok(Some(p)) = db.get_project_by_name(&project) {
                p
            } else if let Ok(Some(p)) = db.get_project_by_local_path(&project) {
                p
            } else if let Ok(canon) = Path::new(&project).canonicalize()
                && let Ok(Some(p)) = db.get_project_by_local_path(&canon.to_string_lossy()) {
                p
            } else {
                bail!("Kayıtlı proje bulunamadı: '{}'", project);
            };

            println!("📦 '{}' projesi SSD'ye geri getiriliyor...", proj.name.bold());
            let report = OffloadEngine::unarchive(&mut db, proj.id, &config, rebuild)?;
            if report.restored_from_quarantine {
                println!("✔ Karantinadan tek rename ile anında geri yüklendi!");
            } else if report.restored_from_archive {
                println!("✔ Google Drive arşivinden doğrulanarak geri yüklendi ({} dosya, {})", report.files_restored, format_bytes(report.bytes_restored));
            }
            if let Some(ref out) = report.rebuild_output {
                println!("🔧 Yeniden Kurulum Çıktısı:\n{}", out);
            }
        }

        Commands::ArchiveVerify { project } => {
            let db = Database::open(&db_path)?;
            let proj = if let Ok(Some(p)) = db.get_project_by_name(&project) {
                p
            } else if let Ok(Some(p)) = db.get_project_by_local_path(&project) {
                p
            } else {
                bail!("Kayıtlı proje bulunamadı: '{}'", project);
            };

            let arch_path = proj.archive_remote_path
                .context("Bu proje için kaydedilmiş bir arşiv yolu bulunamadı.")?;

            println!("🔍 Google Drive Arşivi Doğrulanıyor: {}", arch_path.blue());
            let report = OffloadEngine::archive_verify(Path::new(&arch_path))?;
            if report.is_valid {
                println!("✔ Arşiv Bütünlüğü TAM DOĞRULANDI!");
                println!("   • Doğrulanan Dosya: {}", report.verified_files);
                println!("   • Toplam Bayt: {}", format_bytes(report.verified_bytes));
                println!("   • Manifest SHA-256: {}", report.manifest_sha256.dimmed());
            } else {
                println!("❌ Arşiv Doğrulaması BAŞARISIZ! Hatalar:");
                for e in &report.errors {
                    println!("   - {}", e.red());
                }
            }
        }

        Commands::Diff { snapshot_id, project } => {
            let mut db = Database::open(&db_path)?;
            let cache_mgr = CacheManager::new(&cache_dir, config.max_cache_bytes())?;
            let mut snapshot_engine = devdrive::snapshot::SnapshotEngine::new(&mut db, &cache_mgr);
            let canonical = std::path::PathBuf::from(&project).canonicalize()
                .with_context(|| format!("Proje dizini bulunamadı: {}", project))?;

            println!("🔍 Snapshot karşılaştırılıyor: {}...", snapshot_id.bold());
            let report = snapshot_engine.diff_snapshot(&canonical, &snapshot_id)?;

            println!("\n📸 Snapshot: {} (\"{}\")", report.snapshot_id.cyan(), report.message);
            println!("   Tarih: {}", report.created_at.dimmed());
            println!("   ──────────────────────────────────────────");
            println!("   🟡 Değişen Dosyalar (Modified):        {}", report.modified_count.to_string().yellow().bold());
            println!("   🟢 Yerelde Eklenen / Silinecek (Added): {}", report.added_locally_count.to_string().green().bold());
            println!("   🔴 Yerelde Silinmiş / Gelecek (Missing): {}", report.missing_locally_count.to_string().red().bold());
            println!("   ⚪ Değişmeyen / Aynı (Identical):       {}", report.identical_count.to_string().dimmed());
            println!("   ──────────────────────────────────────────");

            if report.modified_count == 0 && report.added_locally_count == 0 && report.missing_locally_count == 0 {
                println!("✨ Yerel çalışma dizini seçilen snapshot ile %100 birebir aynı!");
            } else {
                for entry in &report.entries {
                    match entry.status.as_str() {
                        "modified" => println!("   🟡 {} (içerik değişmiş)", entry.path.yellow()),
                        "added_locally" => println!("   🟢 + {} (yerelde yeni, rollback ile silinecek)", entry.path.green()),
                        "missing_locally" => println!("   🔴 - {} (yerelde silinmiş, rollback ile geri yüklenecek)", entry.path.red()),
                        _ => {}
                    }
                }
            }
        }

        Commands::Purge { project, force, expired } => {
            let mut db = Database::open(&db_path)?;
            if expired {
                println!("🗑️  Süresi dolmuş tüm karantinalar taranıyor...");
                let reports = OffloadEngine::purge_expired(&mut db, force)?;
                if reports.is_empty() {
                    println!("ℹ️  Temizlenecek süresi dolmuş karantina klasörü bulunamadı.");
                } else {
                    let mut total_freed = 0u64;
                    for r in &reports {
                        println!("   ✔ '{}' karantinası temizlendi: {}", r.project_name.bold(), format_bytes(r.freed_bytes).green());
                        total_freed += r.freed_bytes;
                    }
                    println!("🎉 Toplam açılan ek alan: {}", format_bytes(total_freed).green().bold());
                    devdrive::send_system_notification("Karantina Temizlendi", &format!("Süresi dolan karantinalardan {} alan açıldı.", format_bytes(total_freed)));
                }
            } else {
                if project.trim().is_empty() {
                    bail!("Lütfen bir proje adı belirtin veya süresi dolanları toplu silmek için '--expired' kullanın.");
                }
                let proj = if let Ok(Some(p)) = db.get_project_by_name(&project) {
                    p
                } else if let Ok(Some(p)) = db.get_project_by_local_path(&project) {
                    p
                } else {
                    bail!("Kayıtlı proje bulunamadı: '{}'", project);
                };

                println!("🗑️  '{}' projesinin yerel karantinası kalıcı olarak siliniyor...", proj.name.bold());
                let report = OffloadEngine::purge(&mut db, proj.id, force)?;
                println!("✔ Karantina başarıyla temizlendi: {}", report.deleted_quarantine_path.dimmed());
                println!("💾 Açılan ek alan: {}", format_bytes(report.freed_bytes).green().bold());
                devdrive::send_system_notification("Karantina Silindi", &format!("{} projesinin karantinası silindi ({}).", proj.name, format_bytes(report.freed_bytes)));
            }
        }

        Commands::Ui { port, no_open } => {
            println!("{}", "═════════════════════════════════════════════════════════════".cyan().bold());
            println!("   {}", "⚡ DevDrive: Görsel Kontrol Paneli & Akıllı Arka Plan İzleyici".bold().green());
            println!("{}", "═════════════════════════════════════════════════════════════".cyan().bold());
            let _watcher_handle = devdrive::watcher::start_all_projects_watcher_background(&db_path, &config);
            println!(" ⚡ Arka plan akıllı otomatik eşitleme izleyicisi aktif edildi.");
            devdrive::ui::start_ui_server(port, !no_open).await?;
        }

        Commands::Service { command } => {
            let plist_dir = dirs::home_dir().context("Ev dizini bulunamadı")?.join("Library/LaunchAgents");
            let plist_path = plist_dir.join("com.devdrive.ui.plist");

            match command {
                ServiceCommands::Install { port } => {
                    std::fs::create_dir_all(&plist_dir)?;
                    let current_exe = std::env::current_exe()?.canonicalize()?;
                    let exe_path_str = current_exe.to_string_lossy();
                    let home = dirs::home_dir().unwrap_or_default();
                    let log_dir = home.join(".gemini/antigravity");
                    std::fs::create_dir_all(&log_dir)?;

                    let plist_content = format!(r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>Label</key>
    <string>com.devdrive.ui</string>
    <key>ProgramArguments</key>
    <array>
        <string>{}</string>
        <string>ui</string>
        <string>--port</string>
        <string>{}</string>
        <string>--no-open</string>
    </array>
    <key>RunAtLoad</key>
    <true/>
    <key>KeepAlive</key>
    <true/>
    <key>StandardOutPath</key>
    <string>{}/devdrive_service.log</string>
    <key>StandardErrorPath</key>
    <string>{}/devdrive_service_err.log</string>
</dict>
</plist>
"#, exe_path_str, port, log_dir.display(), log_dir.display());

                    std::fs::write(&plist_path, plist_content)?;
                    println!("✔ LaunchAgent plist dosyası oluşturuldu: {}", plist_path.display());

                    let _ = std::process::Command::new("launchctl")
                        .arg("unload")
                        .arg(&plist_path)
                        .output();

                    let status = std::process::Command::new("launchctl")
                        .arg("load")
                        .arg(&plist_path)
                        .status();

                    match status {
                        Ok(s) if s.success() => {
                            println!("🚀 DevDrive LaunchAgent servisi başarıyla başlatıldı ve açılışa eklendi!");
                            println!("   Web Arayüzü: http://127.0.0.1:{}", port);
                            println!("   Akıllı Otomatik Eşitleme: 30 dk boşta kalma, pil koruması ve arka plan QoS devrede.");
                            devdrive::send_system_notification("Servis Başlatıldı", &format!("DevDrive arka plan servisi aktif: :{}", port));
                        }
                        Ok(_) => {
                            println!("⚠️  launchctl servisi yüklenirken uyarı verdi. Dosya yazıldı: {}", plist_path.display());
                        }
                        Err(e) => {
                            println!("❌ launchctl çalıştırılamadı: {}", e);
                        }
                    }
                }
                ServiceCommands::Uninstall => {
                    if plist_path.exists() {
                        let _ = std::process::Command::new("launchctl")
                            .arg("unload")
                            .arg(&plist_path)
                            .output();
                        let _ = std::fs::remove_file(&plist_path);
                        println!("✔ DevDrive LaunchAgent servisi durduruldu ve kaldırıldı.");
                        devdrive::send_system_notification("Servis Durduruldu", "DevDrive arka plan servisi kaldırıldı.");
                    } else {
                        println!("ℹ️  Yüklü bir LaunchAgent servisi bulunamadı.");
                    }
                }
                ServiceCommands::Status => {
                    if plist_path.exists() {
                        println!("📄 LaunchAgent plist: {}", plist_path.display().to_string().green());
                        let output = std::process::Command::new("launchctl")
                            .arg("list")
                            .output();
                        if let Ok(out) = output {
                            let text = String::from_utf8_lossy(&out.stdout);
                            if let Some(line) = text.lines().find(|l| l.contains("com.devdrive.ui")) {
                                println!("🟢 Servis Durumu: {}", line.green().bold());
                            } else {
                                println!("🟡 Servis dosyası mevcut ancak launchctl listesinde aktif değil.");
                            }
                        }
                    } else {
                        println!("⚪ DevDrive LaunchAgent servisi yüklü değil.");
                    }
                }
            }
        }
    }

    Ok(())
}
