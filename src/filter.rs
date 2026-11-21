use std::path::Path;

#[derive(Debug, PartialEq, Eq, Clone, Copy)]
pub enum FileCategory {
    /// Regular source file that should be tracked, snapshotted, and synced to Drive
    Source,
    /// Ephemeral build/dependency artifact that stays in local SSD cache and is NEVER sent to Drive
    Ephemeral,
    /// Temporary lock/transient file that should be ignored completely
    TransientLock,
}

pub const MAX_SOURCE_FILE_SIZE_BYTES: u64 = 25 * 1024 * 1024; // 25 MB

/// Inspects a directory before entering it in O(1) time using signature marker checks.
pub fn is_ephemeral_dir_by_signature(dir_path: &Path) -> bool {
    let dir_name = dir_path.file_name().and_then(|s| s.to_str()).unwrap_or("");
    if dir_name == "site-packages" {
        return true;
    }
    // Python virtualenv signature (e.g. mlx-env, ai-env)
    if dir_path.join("pyvenv.cfg").exists() {
        return true;
    }
    // Conda virtual environment
    if dir_path.join("conda-meta").is_dir() {
        return true;
    }
    // Standard cache directory tag
    if dir_path.join("CACHEDIR.TAG").exists() {
        return true;
    }
    // CMake build directory
    if dir_path.join("CMakeCache.txt").exists() && dir_path.join("CMakeFiles").is_dir() {
        return true;
    }
    // Python/virtualenv activation binary & library layout
    if dir_path.join("bin").join("activate").exists() && dir_path.join("lib").is_dir() {
        return true;
    }
    // Python packages directory
    if dir_path.join("site-packages").is_dir() {
        return true;
    }
    false
}

pub struct DevFilter {
    project_root: Option<std::path::PathBuf>,
    anywhere_ephemeral_dirs: Vec<&'static str>,
    root_ephemeral_dirs: Vec<&'static str>,
    custom_ephemeral_dirs: Vec<String>,
    transient_extensions: Vec<&'static str>,
    transient_exact: Vec<&'static str>,
    valid_lockfiles: Vec<&'static str>,
    heavy_extensions: Vec<&'static str>,
    project_ignore_patterns: Vec<String>,
}

impl Default for DevFilter {
    fn default() -> Self {
        Self::new()
    }
}

impl DevFilter {
    pub fn new() -> Self {
        Self::with_root(None)
    }

    pub fn with_root(project_root: Option<std::path::PathBuf>) -> Self {
        // config.toml mevcutsa kullanıcının özel geçici klasörlerini yan etkisiz (diske yazmadan) oku
        let custom_ephemeral_dirs = std::fs::read_to_string(crate::config::DevDriveConfig::config_path())
            .ok()
            .and_then(|content| toml::from_str::<crate::config::DevDriveConfig>(&content).ok())
            .map(|cfg| cfg.custom_ephemeral_dirs)
            .unwrap_or_default();

        Self {
            project_root,
            anywhere_ephemeral_dirs: vec![
                ".git",
                "node_modules",
                ".venv",
                "venv",
                "__pycache__",
                ".pytest_cache",
                ".mypy_cache",
                ".ruff_cache",
                ".next",
                ".nuxt",
                ".cache",
                ".turbo",
                ".gradle",
                "Pods",
                "DerivedData",
                ".build",
                ".dart_tool",
                ".dart-tool",
                ".dartServer",
                ".analysis-driver",
                ".cursor",
                ".expo",
                ".agents",
                ".junie",
                ".codeium",
                ".zap",
                ".continue",
                ".kutu",
                ".kotlin",
                ".wrongstack",
                ".cortex_test_db",
                "browser_profile",
                ".pub-cache",
                ".idea",
                ".vscode",
            ],
            root_ephemeral_dirs: vec![
                "target",
                "dist",
                "build",
                "out",
                "vendor",
            ],
            transient_extensions: vec![
                "tmp",
                "swp",
                "swo",
                "journal",
                "wal",
            ],
            transient_exact: vec![
                ".DS_Store",
                "Thumbs.db",
                ".git/index.lock",
                ".git/HEAD.lock",
                ".devdrive-showcase.json",
                "📋 BULUT_VITRINI.html",
                "Projeyi_Uyandir.command",
                "⚡ Projeyi_Uyandir.command",
            ],
            valid_lockfiles: vec![
                "Cargo.lock",
                "package-lock.json",
                "pnpm-lock.yaml",
                "yarn.lock",
                "poetry.lock",
                "composer.lock",
                "Gemfile.lock",
                "bun.lockb",
                "flake.lock",
                "mix.lock",
                "uv.lock",
                "Pipfile.lock",
                "Podfile.lock",
                "deno.lock",
                "pubspec.lock",
                "berksfile.lock",
                "pdm.lock",
            ],
            heavy_extensions: vec![
                // AI/ML models
                "pt", "onnx", "safetensors", "bin", "gguf", "npz", "task", "tflite",
                // Build outputs & binary libraries
                "a", "aar", "dylib", "so", "apk", "app", "dill", "framework", "rlib",
                // Audio & video media
                "mp4", "mov", "avi", "mkv", "wav", "mp3", "flac", "m4a",
                // Databases
                "db", "sqlite", "sqlite3", "redb",
                // Maps and intermediate outputs
                "map", "unlinked2",
                // Archives
                "tar", "gz", "7z",
            ],
            custom_ephemeral_dirs,
            project_ignore_patterns: Vec::new(),
        }
    }

    /// Sets or updates the project root path for relative file checks
    pub fn set_project_root<P: AsRef<Path>>(&mut self, root: P) {
        self.project_root = Some(root.as_ref().to_path_buf());
    }

    /// Creates a filter initialized for a specific project directory, loading its .devdriveignore if present
    pub fn for_project<P: AsRef<Path>>(project_root: P) -> Self {
        let root = project_root.as_ref().to_path_buf();
        let mut filter = Self::with_root(Some(root.clone()));
        filter.load_project_ignore(root);
        filter
    }

    /// Reads project-specific ignore patterns from .devdriveignore side-effect-free
    pub fn load_project_ignore<P: AsRef<Path>>(&mut self, project_root: P) {
        let ignore_file = project_root.as_ref().join(".devdriveignore");
        if let Ok(content) = std::fs::read_to_string(&ignore_file) {
            for line in content.lines() {
                let trimmed = line.trim();
                if trimmed.is_empty() || trimmed.starts_with('#') {
                    continue;
                }
                if !self.project_ignore_patterns.iter().any(|p| p == trimmed) {
                    self.project_ignore_patterns.push(trimmed.to_string());
                }
            }
        }
    }

    /// Checks if a relative path matches a .devdriveignore pattern (supports wildcards, dir prefixes, extensions)
    pub fn matches_ignore_pattern(pattern: &str, relative_path: &Path) -> bool {
        let pattern = pattern.trim();
        if pattern.is_empty() || pattern.starts_with('#') {
            return false;
        }

        let path_str = relative_path.to_string_lossy();
        let file_name = relative_path.file_name().and_then(|s| s.to_str()).unwrap_or("");

        // 1. Directory pattern (e.g. "models/" or "datasets/")
        if let Some(dir_name) = pattern.strip_suffix('/') {
            for comp in relative_path.components() {
                if comp.as_os_str().to_string_lossy() == dir_name {
                    return true;
                }
            }
            if path_str.starts_with(pattern) || path_str == dir_name {
                return true;
            }
        }

        // 2. Extension wildcard (e.g. "*.mp4", "*.onnx", "*.sqlite")
        if let Some(ext) = pattern.strip_prefix("*.")
            && let Some(file_ext) = relative_path.extension().and_then(|e| e.to_str())
                && file_ext.eq_ignore_ascii_case(ext) {
                    return true;
                }

        // 3. Exact match on filename or relative path
        if file_name == pattern || path_str == pattern {
            return true;
        }

        // 4. Any component match (e.g. "datasets")
        for comp in relative_path.components() {
            if comp.as_os_str().to_string_lossy() == pattern {
                return true;
            }
        }

        false
    }

    /// Creates a filter initialized with custom rules from the user configuration
    pub fn with_config(config: &crate::config::DevDriveConfig) -> Self {
        let mut filter = Self::new();
        filter.custom_ephemeral_dirs = config.custom_ephemeral_dirs.clone();
        filter
    }

    /// Dynamically appends a custom ephemeral directory name to ignore
    pub fn add_custom_ephemeral_dir(&mut self, dir: &str) {
        if !self.custom_ephemeral_dirs.iter().any(|d| d == dir) {
            self.custom_ephemeral_dirs.push(dir.to_string());
        }
    }

    /// Resolves file size on disk if available
    fn get_file_size(&self, relative_path: &Path) -> Option<u64> {
        let full_path = if let Some(ref root) = self.project_root {
            if relative_path.is_absolute() {
                relative_path.to_path_buf()
            } else {
                root.join(relative_path)
            }
        } else {
            relative_path.to_path_buf()
        };
        std::fs::metadata(&full_path).ok().filter(|m| m.is_file()).map(|m| m.len())
    }

    /// Categorizes a path relative to the project root with ZERO false positives.
    pub fn categorize(&self, relative_path: &Path) -> FileCategory {
        let size = self.get_file_size(relative_path);
        self.categorize_with_size(relative_path, size)
    }

    /// Categorizes a path relative to the project root with an optional known file size.
    pub fn categorize_with_size(&self, relative_path: &Path, file_size: Option<u64>) -> FileCategory {
        let path_str = relative_path.to_string_lossy();
        let file_name_opt = relative_path.file_name().and_then(|s| s.to_str());

        // 1. Check sensitive environment variable files (.env protection)
        // .env, .env.local, .env.production etc. must NEVER go to Drive or CAS (TransientLock)
        // Exceptions: .env.example, .env.sample, .env.template (must remain Source)
        if let Some(file_name) = file_name_opt
            && (file_name == ".env" || file_name.starts_with(".env.")) {
                let is_safe_template = file_name.ends_with(".example")
                    || file_name.ends_with(".sample")
                    || file_name.ends_with(".template");
                if !is_safe_template {
                    return FileCategory::TransientLock;
                }
            }

        // 2. Check transient exact matches or lock files (tam dosya adı veya exact path kontrolü)
        for exact in &self.transient_exact {
            if exact.contains('/') {
                if path_str == *exact || path_str.ends_with(&format!("/{}", exact)) {
                    return FileCategory::TransientLock;
                }
            } else if let Some(file_name) = file_name_opt
                && file_name == *exact {
                    return FileCategory::TransientLock;
                }
        }

        // 3. Check editor locks and refined double-extension backups
        if let Some(file_name) = file_name_opt {
            if file_name.starts_with(".#") || file_name.ends_with('~') {
                return FileCategory::TransientLock;
            }
            // Double-extension editor backups (e.g. main.rs.bak), while preserving legitimate files like database.bak
            if file_name.ends_with(".bak") && file_name[..file_name.len() - 4].contains('.') {
                return FileCategory::TransientLock;
            }
            // Hidden lock files or standalone process locks (e.g. .lock, .pid, hidden .*.lock)
            if (file_name.starts_with('.') && file_name.ends_with(".lock")) || file_name == "lock" {
                return FileCategory::TransientLock;
            }
        }

        let components: Vec<_> = relative_path.components().collect();

        // 4. Ephemeral directory checks BEFORE lockfile checks (Fix for lockfile leak inside node_modules/.venv)
        // Check root and subproject build directories (e.g. target/, build/, dist/, out/, vendor/)
        for (i, comp) in components.iter().enumerate() {
            let comp_str = comp.as_os_str().to_string_lossy();
            if self.root_ephemeral_dirs.iter().any(|&d| d == comp_str) {
                // At workspace root (i == 0) -> ALWAYS Ephemeral
                if i == 0 {
                    return FileCategory::Ephemeral;
                }
                // In subdirectories: Ephemeral unless directly inside src/ or lib/
                let parent_str = components[i - 1].as_os_str().to_string_lossy();
                if parent_str != "src" && parent_str != "lib" {
                    return FileCategory::Ephemeral;
                }
            }
        }

        // 5. Check anywhere-ephemeral directories (e.g. node_modules, .git, .venv, .dartServer) and user custom dirs
        for comp in &components {
            let comp_str = comp.as_os_str().to_string_lossy();
            if self.anywhere_ephemeral_dirs.iter().any(|&d| d == comp_str)
                || self.custom_ephemeral_dirs.iter().any(|d| d == &comp_str)
                || comp_str.ends_with(".framework")
                || comp_str.ends_with(".app")
                || comp_str == "site-packages"
            {
                return FileCategory::Ephemeral;
            }
        }

        // 5b. Signature-based ephemeral ancestor check
        if let Some(ref root) = self.project_root {
            let mut curr = root.clone();
            if let Some(parent) = relative_path.parent() {
                for comp in parent.components() {
                    curr.push(comp);
                    if is_ephemeral_dir_by_signature(&curr) {
                        return FileCategory::Ephemeral;
                    }
                }
            }
        }

        // 6. Check project-specific .devdriveignore rules (e.g. models/, *.mp4, *.onnx)
        for pattern in &self.project_ignore_patterns {
            if Self::matches_ignore_pattern(pattern, relative_path) {
                return FileCategory::Ephemeral;
            }
        }

        // 7. Lockfiles whitelist (checked AFTER ephemeral directories so lockfiles inside node_modules are Ephemeral)
        if let Some(file_name) = file_name_opt
            && self.valid_lockfiles.contains(&file_name) {
                return FileCategory::Source;
            }

        // 8. Check transient extensions
        if let Some(ext) = relative_path.extension().and_then(|e| e.to_str())
            && self.transient_extensions.iter().any(|&e| e.eq_ignore_ascii_case(ext)) {
                return FileCategory::TransientLock;
            }

        // 9. Non-code heavy extension shield (AI/ML models, binaries, media, databases, maps)
        if let Some(ext) = relative_path.extension().and_then(|e| e.to_str()) {
            let ext_lower = ext.to_ascii_lowercase();
            if self.heavy_extensions.iter().any(|&he| he == ext_lower) {
                // tree.tar.gz exception: DevDrive internal Class B archive
                let is_tree_archive = file_name_opt == Some("tree.tar.gz")
                    || path_str.ends_with("tree.tar.gz")
                    || (ext_lower == "gz" && file_name_opt == Some("tree.tar.gz"));
                if !is_tree_archive {
                    return FileCategory::Ephemeral;
                }
            }
        }

        // 10. Max file size guard: files larger than 25 MB are classified as Ephemeral
        if let Some(size) = file_size
            && size > MAX_SOURCE_FILE_SIZE_BYTES {
                return FileCategory::Ephemeral;
            }

        FileCategory::Source
    }

    /// Returns true if this file should be synced to remote Drive
    pub fn is_syncable(&self, relative_path: &Path) -> bool {
        self.categorize(relative_path) == FileCategory::Source
    }

    pub fn is_anywhere_ephemeral_dir(&self, name: &str) -> bool {
        self.anywhere_ephemeral_dirs.contains(&name)
            || self.custom_ephemeral_dirs.iter().any(|d| d == name)
            || self.project_ignore_patterns.iter().any(|p| p.trim().trim_end_matches('/') == name)
    }

    pub fn is_root_ephemeral_dir(&self, name: &str) -> bool {
        self.root_ephemeral_dirs.contains(&name)
    }

    pub fn should_skip_dir(&self, name: &str, is_root: bool) -> bool {
        self.should_skip_dir_with_parent(name, is_root, None)
    }

    pub fn should_skip_dir_with_parent(&self, name: &str, is_root: bool, parent_name: Option<&str>) -> bool {
        if self.is_anywhere_ephemeral_dir(name) {
            return true;
        }
        if name.ends_with(".framework") || name.ends_with(".app") {
            return true;
        }
        if self.is_root_ephemeral_dir(name) {
            if is_root {
                return true;
            }
            if let Some(parent) = parent_name {
                return parent != "src" && parent != "lib";
            }
        }
        false
    }

    /// Smart entry skip checking both name/parent patterns and signature-based markers in O(1) time
    pub fn should_skip_dir_entry(&self, dir_path: &Path, is_root: bool) -> bool {
        let dir_name = dir_path.file_name().and_then(|s| s.to_str()).unwrap_or("");
        let parent_name = dir_path.parent().and_then(|p| p.file_name()).and_then(|s| s.to_str());
        if self.should_skip_dir_with_parent(dir_name, is_root, parent_name) {
            return true;
        }

        if dir_path.is_absolute() {
            if is_ephemeral_dir_by_signature(dir_path) {
                return true;
            }
        } else if let Some(ref root) = self.project_root {
            if is_ephemeral_dir_by_signature(&root.join(dir_path)) {
                return true;
            }
        } else if is_ephemeral_dir_by_signature(dir_path) {
            return true;
        }

        false
    }

    pub fn reproducible_rules() -> Vec<ReproducibleRule> {
        vec![
            ReproducibleRule {
                dir_name: "target",
                is_root_only: true,
                required_markers: &["Cargo.lock", "Cargo.toml"],
                rebuild_command: "cargo fetch --locked && cargo build",
            },
            ReproducibleRule {
                dir_name: "node_modules",
                is_root_only: false,
                required_markers: &["package-lock.json", "yarn.lock", "pnpm-lock.yaml", "bun.lockb", "package.json"],
                rebuild_command: "npm ci",
            },
            ReproducibleRule {
                dir_name: ".venv",
                is_root_only: false,
                required_markers: &["poetry.lock", "uv.lock", "Pipfile.lock", "requirements.txt", "pyproject.toml", "pdm.lock"],
                rebuild_command: "uv sync --frozen",
            },
            ReproducibleRule {
                dir_name: "venv",
                is_root_only: false,
                required_markers: &["poetry.lock", "uv.lock", "Pipfile.lock", "requirements.txt", "pyproject.toml", "pdm.lock"],
                rebuild_command: "uv sync --frozen",
            },
            ReproducibleRule {
                dir_name: "__pycache__",
                is_root_only: false,
                required_markers: &[],
                rebuild_command: "python3 -m compileall .",
            },
            ReproducibleRule {
                dir_name: ".pytest_cache",
                is_root_only: false,
                required_markers: &[],
                rebuild_command: "pytest",
            },
            ReproducibleRule {
                dir_name: ".mypy_cache",
                is_root_only: false,
                required_markers: &[],
                rebuild_command: "mypy .",
            },
            ReproducibleRule {
                dir_name: ".ruff_cache",
                is_root_only: false,
                required_markers: &[],
                rebuild_command: "ruff check .",
            },
            ReproducibleRule {
                dir_name: ".next",
                is_root_only: false,
                required_markers: &["package.json"],
                rebuild_command: "npm run build",
            },
            ReproducibleRule {
                dir_name: ".nuxt",
                is_root_only: false,
                required_markers: &["package.json"],
                rebuild_command: "npm run build",
            },
            ReproducibleRule {
                dir_name: "Pods",
                is_root_only: false,
                required_markers: &["Podfile.lock", "Podfile"],
                rebuild_command: "pod install",
            },
            ReproducibleRule {
                dir_name: ".gradle",
                is_root_only: false,
                required_markers: &["build.gradle", "build.gradle.kts", "pom.xml"],
                rebuild_command: "./gradlew build",
            },
            ReproducibleRule {
                dir_name: ".dart_tool",
                is_root_only: false,
                required_markers: &["pubspec.lock", "pubspec.yaml"],
                rebuild_command: "dart pub get",
            },
            ReproducibleRule {
                dir_name: ".pub-cache",
                is_root_only: false,
                required_markers: &["pubspec.lock", "pubspec.yaml"],
                rebuild_command: "dart pub get",
            },
            ReproducibleRule {
                dir_name: "dist",
                is_root_only: true,
                required_markers: &["package.json", "Cargo.toml", "pyproject.toml"],
                rebuild_command: "npm run build",
            },
            ReproducibleRule {
                dir_name: "build",
                is_root_only: true,
                required_markers: &["package.json", "CMakeLists.txt", "Makefile", "Cargo.toml"],
                rebuild_command: "make",
            },
            ReproducibleRule {
                dir_name: "out",
                is_root_only: true,
                required_markers: &["package.json", "Cargo.toml"],
                rebuild_command: "npm run build",
            },
        ]
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReproducibleRule {
    pub dir_name: &'static str,
    pub is_root_only: bool,
    pub required_markers: &'static [&'static str],
    pub rebuild_command: &'static str,
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn test_ephemeral_paths() {
        let filter = DevFilter::new();
        assert_eq!(
            filter.categorize(&PathBuf::from("node_modules/express/index.js")),
            FileCategory::Ephemeral
        );
        assert_eq!(
            filter.categorize(&PathBuf::from("target/debug/app")),
            FileCategory::Ephemeral
        );
        assert_eq!(
            filter.categorize(&PathBuf::from(".venv/lib/python3.12/site-packages/fastapi")),
            FileCategory::Ephemeral
        );
        assert_eq!(
            filter.categorize(&PathBuf::from(".next/static/chunks/main.js")),
            FileCategory::Ephemeral
        );
        assert_eq!(
            filter.categorize(&PathBuf::from(".git/objects/da/35f0805d")),
            FileCategory::Ephemeral
        );
        assert_eq!(
            filter.categorize(&PathBuf::from(".git/refs/heads/master")),
            FileCategory::Ephemeral
        );
        assert_eq!(
            filter.categorize(&PathBuf::from(".gitignore")),
            FileCategory::Source
        );
    }

    #[test]
    fn test_transient_locks() {
        let filter = DevFilter::new();
        assert_eq!(
            filter.categorize(&PathBuf::from(".DS_Store")),
            FileCategory::TransientLock
        );
        assert_eq!(
            filter.categorize(&PathBuf::from(".git/index.lock")),
            FileCategory::TransientLock
        );
        assert_eq!(
            filter.categorize(&PathBuf::from("server.tmp")),
            FileCategory::TransientLock
        );
        assert_eq!(
            filter.categorize(&PathBuf::from(".devdrive-showcase.json")),
            FileCategory::TransientLock
        );
        assert_eq!(
            filter.categorize(&PathBuf::from("📋 BULUT_VITRINI.html")),
            FileCategory::TransientLock
        );
        assert_eq!(
            filter.categorize(&PathBuf::from("Projeyi_Uyandir.command")),
            FileCategory::TransientLock
        );
    }

    #[test]
    fn test_source_files() {
        let filter = DevFilter::new();
        assert_eq!(
            filter.categorize(&PathBuf::from("src/main.rs")),
            FileCategory::Source
        );
        assert_eq!(
            filter.categorize(&PathBuf::from("Cargo.lock")),
            FileCategory::Source
        );
        assert_eq!(
            filter.categorize(&PathBuf::from("package-lock.json")),
            FileCategory::Source
        );
        assert_eq!(
            filter.categorize(&PathBuf::from(".env.example")),
            FileCategory::Source
        );
        assert_eq!(
            filter.categorize(&PathBuf::from(".env.sample")),
            FileCategory::Source
        );
        assert_eq!(
            filter.categorize(&PathBuf::from("uv.lock")),
            FileCategory::Source
        );
        assert_eq!(
            filter.categorize(&PathBuf::from("Pipfile.lock")),
            FileCategory::Source
        );
        assert_eq!(
            filter.categorize(&PathBuf::from("Podfile.lock")),
            FileCategory::Source
        );
        assert_eq!(
            filter.categorize(&PathBuf::from("deno.lock")),
            FileCategory::Source
        );
        assert_eq!(
            filter.categorize(&PathBuf::from("src/env/config.rs")),
            FileCategory::Source
        );
        assert_eq!(
            filter.categorize(&PathBuf::from("foo.DS_Store")),
            FileCategory::Source
        );
        assert_eq!(
            filter.categorize(&PathBuf::from("myThumbs.txt")),
            FileCategory::Source
        );
        assert_eq!(
            filter.categorize(&PathBuf::from("database.bak")),
            FileCategory::Source
        );
    }

    #[test]
    fn test_env_protection_and_locks() {
        let filter = DevFilter::new();
        assert_eq!(
            filter.categorize(&PathBuf::from(".env")),
            FileCategory::TransientLock
        );
        assert_eq!(
            filter.categorize(&PathBuf::from(".env.local")),
            FileCategory::TransientLock
        );
        assert_eq!(
            filter.categorize(&PathBuf::from(".env.production")),
            FileCategory::TransientLock
        );
        assert_eq!(
            filter.categorize(&PathBuf::from("config/.env.secrets")),
            FileCategory::TransientLock
        );
        assert_eq!(
            filter.categorize(&PathBuf::from("main.rs.bak")),
            FileCategory::TransientLock
        );
        assert_eq!(
            filter.categorize(&PathBuf::from(".foo.lock")),
            FileCategory::TransientLock
        );
    }

    #[test]
    fn test_custom_ephemeral_dirs() {
        let mut filter = DevFilter::new();
        filter.add_custom_ephemeral_dir("my_custom_cache");
        filter.add_custom_ephemeral_dir("temp_output");

        assert_eq!(
            filter.categorize(&PathBuf::from("my_custom_cache/data.bin")),
            FileCategory::Ephemeral
        );
        assert_eq!(
            filter.categorize(&PathBuf::from("subdir/temp_output/bundle.js")),
            FileCategory::Ephemeral
        );
        assert_eq!(
            filter.categorize(&PathBuf::from("src/main.rs")),
            FileCategory::Source
        );
        assert!(filter.is_anywhere_ephemeral_dir("my_custom_cache"));
    }

    #[test]
    fn test_devdriveignore_rules() {
        let temp = tempfile::tempdir().unwrap();
        let ignore_content = "\
# Ignore AI models and datasets
models/
datasets/
*.mp4
*.onnx
weights.bin
";
        std::fs::write(temp.path().join(".devdriveignore"), ignore_content).unwrap();

        let filter = DevFilter::for_project(temp.path());

        assert_eq!(
            filter.categorize(&PathBuf::from("models/llama3.bin")),
            FileCategory::Ephemeral
        );
        assert_eq!(
            filter.categorize(&PathBuf::from("sub/datasets/train.csv")),
            FileCategory::Ephemeral
        );
        assert_eq!(
            filter.categorize(&PathBuf::from("video.mp4")),
            FileCategory::Ephemeral
        );
        assert_eq!(
            filter.categorize(&PathBuf::from("sub/deep/model.onnx")),
            FileCategory::Ephemeral
        );
        assert_eq!(
            filter.categorize(&PathBuf::from("weights.bin")),
            FileCategory::Ephemeral
        );
        // Regular source files must still remain Source
        assert_eq!(
            filter.categorize(&PathBuf::from("src/models.rs")),
            FileCategory::Source
        );
        assert_eq!(
            filter.categorize(&PathBuf::from("src/main.rs")),
            FileCategory::Source
        );
    }

    #[test]
    fn test_subproject_build_directories() {
        let filter = DevFilter::new();
        // Subdirectory build folders must be Ephemeral
        assert_eq!(
            filter.categorize(&PathBuf::from("mobile/build/ios/iphoneos/Runner.app/Info.plist")),
            FileCategory::Ephemeral
        );
        assert_eq!(
            filter.categorize(&PathBuf::from("test/kora/mobile/build/Flutter.framework")),
            FileCategory::Ephemeral
        );
        assert_eq!(
            filter.categorize(&PathBuf::from("packages/frontend/dist/bundle.js")),
            FileCategory::Ephemeral
        );
        assert_eq!(
            filter.categorize(&PathBuf::from("crates/engine/target/debug/lib.rlib")),
            FileCategory::Ephemeral
        );

        // Folders inside src/ or lib/ must remain Source
        assert_eq!(
            filter.categorize(&PathBuf::from("src/target/mod.rs")),
            FileCategory::Source
        );
        assert_eq!(
            filter.categorize(&PathBuf::from("lib/build/generator.py")),
            FileCategory::Source
        );
    }

    #[test]
    fn test_lockfiles_in_ephemeral_dirs() {
        let filter = DevFilter::new();
        // Lockfiles inside node_modules or .venv MUST be Ephemeral, never Source
        assert_eq!(
            filter.categorize(&PathBuf::from("node_modules/.pnpm/pkg/yarn.lock")),
            FileCategory::Ephemeral
        );
        assert_eq!(
            filter.categorize(&PathBuf::from("node_modules/lodash/package-lock.json")),
            FileCategory::Ephemeral
        );
        assert_eq!(
            filter.categorize(&PathBuf::from("sub/node_modules/foo/Cargo.lock")),
            FileCategory::Ephemeral
        );
        assert_eq!(
            filter.categorize(&PathBuf::from(".venv/packages/poetry.lock")),
            FileCategory::Ephemeral
        );
        // Valid lockfiles at project root must still be Source
        assert_eq!(
            filter.categorize(&PathBuf::from("yarn.lock")),
            FileCategory::Source
        );
        assert_eq!(
            filter.categorize(&PathBuf::from("Cargo.lock")),
            FileCategory::Source
        );
        assert_eq!(
            filter.categorize(&PathBuf::from("package-lock.json")),
            FileCategory::Source
        );
    }

    #[test]
    fn test_heavy_extensions_shield() {
        let filter = DevFilter::new();

        // 1. AI / ML Models
        assert_eq!(filter.categorize(&PathBuf::from("models/model.onnx")), FileCategory::Ephemeral);
        assert_eq!(filter.categorize(&PathBuf::from("deep/model_int8.onnx")), FileCategory::Ephemeral);
        assert_eq!(filter.categorize(&PathBuf::from("weights.pt")), FileCategory::Ephemeral);
        assert_eq!(filter.categorize(&PathBuf::from("model.safetensors")), FileCategory::Ephemeral);
        assert_eq!(filter.categorize(&PathBuf::from("quantized.gguf")), FileCategory::Ephemeral);
        assert_eq!(filter.categorize(&PathBuf::from("embeddings.npz")), FileCategory::Ephemeral);
        assert_eq!(filter.categorize(&PathBuf::from("detection.task")), FileCategory::Ephemeral);
        assert_eq!(filter.categorize(&PathBuf::from("mobile.tflite")), FileCategory::Ephemeral);

        // 2. Binary and Build Outputs
        assert_eq!(filter.categorize(&PathBuf::from("lib/libfoo.a")), FileCategory::Ephemeral);
        assert_eq!(filter.categorize(&PathBuf::from("android/library.aar")), FileCategory::Ephemeral);
        assert_eq!(filter.categorize(&PathBuf::from("bin/library.dylib")), FileCategory::Ephemeral);
        assert_eq!(filter.categorize(&PathBuf::from("bin/library.so")), FileCategory::Ephemeral);
        assert_eq!(filter.categorize(&PathBuf::from("release/app.apk")), FileCategory::Ephemeral);
        assert_eq!(filter.categorize(&PathBuf::from("flutter/app.dill")), FileCategory::Ephemeral);
        assert_eq!(filter.categorize(&PathBuf::from("build/output.app")), FileCategory::Ephemeral);
        assert_eq!(filter.categorize(&PathBuf::from("build/App.framework")), FileCategory::Ephemeral);

        // 3. Audio & Video Media
        assert_eq!(filter.categorize(&PathBuf::from("audio/test.wav")), FileCategory::Ephemeral);
        assert_eq!(filter.categorize(&PathBuf::from("video/clip.mp4")), FileCategory::Ephemeral);
        assert_eq!(filter.categorize(&PathBuf::from("media/video.mov")), FileCategory::Ephemeral);
        assert_eq!(filter.categorize(&PathBuf::from("media/video.avi")), FileCategory::Ephemeral);
        assert_eq!(filter.categorize(&PathBuf::from("media/video.mkv")), FileCategory::Ephemeral);
        assert_eq!(filter.categorize(&PathBuf::from("sound/voice.mp3")), FileCategory::Ephemeral);
        assert_eq!(filter.categorize(&PathBuf::from("sound/song.flac")), FileCategory::Ephemeral);
        assert_eq!(filter.categorize(&PathBuf::from("sound/record.m4a")), FileCategory::Ephemeral);

        // 4. Databases
        assert_eq!(filter.categorize(&PathBuf::from("app.db")), FileCategory::Ephemeral);
        assert_eq!(filter.categorize(&PathBuf::from("data.sqlite")), FileCategory::Ephemeral);
        assert_eq!(filter.categorize(&PathBuf::from("db/store.sqlite3")), FileCategory::Ephemeral);
        assert_eq!(filter.categorize(&PathBuf::from("state.redb")), FileCategory::Ephemeral);

        // 5. Maps and intermediate outputs
        assert_eq!(filter.categorize(&PathBuf::from("dist/bundle.js.map")), FileCategory::Ephemeral);
        assert_eq!(filter.categorize(&PathBuf::from("cache.unlinked2")), FileCategory::Ephemeral);

        // 6. Archives & tree.tar.gz exception
        assert_eq!(filter.categorize(&PathBuf::from("backup.tar")), FileCategory::Ephemeral);
        assert_eq!(filter.categorize(&PathBuf::from("backup.7z")), FileCategory::Ephemeral);
        assert_eq!(filter.categorize(&PathBuf::from("archive.gz")), FileCategory::Ephemeral);
        assert_eq!(filter.categorize(&PathBuf::from("tree.tar.gz")), FileCategory::Source);
    }

    #[test]
    fn test_ui_assets_preserved_as_source() {
        let filter = DevFilter::new();
        // UI assets must be Source
        assert_eq!(filter.categorize(&PathBuf::from("src/assets/logo.png")), FileCategory::Source);
        assert_eq!(filter.categorize(&PathBuf::from("public/icon.svg")), FileCategory::Source);
        assert_eq!(filter.categorize(&PathBuf::from("assets/favicon.ico")), FileCategory::Source);
        assert_eq!(filter.categorize(&PathBuf::from("src/images/banner.webp")), FileCategory::Source);

        // Normal source code must be Source
        assert_eq!(filter.categorize(&PathBuf::from("src/main.rs")), FileCategory::Source);
        assert_eq!(filter.categorize(&PathBuf::from("src/components/button.tsx")), FileCategory::Source);
        assert_eq!(filter.categorize(&PathBuf::from("lib/index.ts")), FileCategory::Source);
        assert_eq!(filter.categorize(&PathBuf::from("scripts/train.py")), FileCategory::Source);
        assert_eq!(filter.categorize(&PathBuf::from("Cargo.toml")), FileCategory::Source);
        assert_eq!(filter.categorize(&PathBuf::from("package.json")), FileCategory::Source);
    }

    #[test]
    fn test_developer_cache_dirs() {
        let filter = DevFilter::new();
        assert_eq!(
            filter.categorize(&PathBuf::from(".dartServer/.analysis-driver/cache.unlinked2")),
            FileCategory::Ephemeral
        );
        assert_eq!(
            filter.categorize(&PathBuf::from(".dart-tool/package_config.json")),
            FileCategory::Ephemeral
        );
        assert_eq!(
            filter.categorize(&PathBuf::from(".cursor/extensions/plugin.json")),
            FileCategory::Ephemeral
        );
        assert_eq!(
            filter.categorize(&PathBuf::from(".expo/settings.json")),
            FileCategory::Ephemeral
        );
        assert_eq!(
            filter.categorize(&PathBuf::from(".agents/state.json")),
            FileCategory::Ephemeral
        );
        assert_eq!(
            filter.categorize(&PathBuf::from(".junie/cache.json")),
            FileCategory::Ephemeral
        );
        assert_eq!(
            filter.categorize(&PathBuf::from(".codeium/config.json")),
            FileCategory::Ephemeral
        );
        assert_eq!(
            filter.categorize(&PathBuf::from(".zap/history.log")),
            FileCategory::Ephemeral
        );
        assert_eq!(
            filter.categorize(&PathBuf::from(".continue/config.json")),
            FileCategory::Ephemeral
        );
        assert_eq!(
            filter.categorize(&PathBuf::from(".kutu/metadata")),
            FileCategory::Ephemeral
        );
        assert_eq!(
            filter.categorize(&PathBuf::from(".kotlin/sessions/s1")),
            FileCategory::Ephemeral
        );
        assert_eq!(
            filter.categorize(&PathBuf::from(".wrongstack/dump.txt")),
            FileCategory::Ephemeral
        );
        assert_eq!(
            filter.categorize(&PathBuf::from(".cortex_test_db/index.db")),
            FileCategory::Ephemeral
        );
        assert_eq!(
            filter.categorize(&PathBuf::from("browser_profile/Default/History")),
            FileCategory::Ephemeral
        );
    }

    #[test]
    fn test_max_file_size_guard() {
        let filter = DevFilter::new();
        // Over 25 MB (25 * 1024 * 1024 + 1 bytes)
        let large_size = 25 * 1024 * 1024 + 100;
        let normal_size = 1024 * 1024; // 1 MB

        assert_eq!(
            filter.categorize_with_size(&PathBuf::from("src/huge_dataset.txt"), Some(large_size)),
            FileCategory::Ephemeral
        );
        assert_eq!(
            filter.categorize_with_size(&PathBuf::from("src/normal_file.txt"), Some(normal_size)),
            FileCategory::Source
        );
    }

    #[test]
    fn test_signature_based_venv_detection() {
        let temp = tempfile::tempdir().unwrap();
        let mlx_env = temp.path().join("mlx-env");
        let site_pkg = mlx_env.join("lib").join("python3.14").join("site-packages");
        std::fs::create_dir_all(&site_pkg).unwrap();
        std::fs::write(mlx_env.join("pyvenv.cfg"), "home = /opt/homebrew/bin\ninclude-system-site-packages = false").unwrap();
        let torch_file = site_pkg.join("torch.py");
        std::fs::write(&torch_file, "# Dummy PyTorch").unwrap();

        let filter = DevFilter::for_project(temp.path());

        // 1. Directory entry should be skipped immediately by should_skip_dir_entry
        assert!(filter.should_skip_dir_entry(&mlx_env, false));
        assert!(filter.should_skip_dir_entry(&site_pkg, false));

        // 2. File inside mlx-env should be classified as Ephemeral
        let rel_torch = Path::new("mlx-env/lib/python3.14/site-packages/torch.py");
        assert_eq!(filter.categorize(rel_torch), FileCategory::Ephemeral);

        // 3. Standalone signature check
        assert!(is_ephemeral_dir_by_signature(&mlx_env));
    }
}

