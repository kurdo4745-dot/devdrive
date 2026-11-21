use anyhow::{Context, Result};
use sha2::{Digest, Sha256};
use std::ffi::CString;
use std::fs::File;
use std::io::{Read, Write};
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};

pub struct CacheManager {
    cache_root: PathBuf,
    max_cache_bytes: u64,
}

impl CacheManager {
    pub fn new<P: AsRef<Path>>(cache_root: P, max_cache_bytes: u64) -> Result<Self> {
        let root = cache_root.as_ref().to_path_buf();
        std::fs::create_dir_all(&root)?;
        Ok(Self {
            cache_root: root,
            max_cache_bytes,
        })
    }

    pub fn default_cache_dir() -> PathBuf {
        dirs::home_dir()
            .unwrap_or_else(|| PathBuf::from("."))
            .join(".cache")
            .join("devdrive")
    }

    pub fn cache_root(&self) -> &Path {
        &self.cache_root
    }

    pub fn max_cache_bytes(&self) -> u64 {
        self.max_cache_bytes
    }

    /// APFS instant zero-copy clone on macOS via atomic temp file replacement
    pub fn clone_file<P: AsRef<Path>, Q: AsRef<Path>>(src: P, dst: Q) -> Result<()> {
        let src = src.as_ref();
        let dst = dst.as_ref();

        let parent = dst.parent().context("Destination has no parent directory")?;
        std::fs::create_dir_all(parent)?;

        // If destination already exists:
        if dst.exists() {
            // Check if size and content are already identical to avoid unnecessary I/O & notifications
            if let (Ok(meta_src), Ok(meta_dst)) = (src.metadata(), dst.metadata())
                && meta_src.len() == meta_dst.len()
                    && let (Ok(hash_src), Ok(hash_dst)) = (Self::compute_sha256(src), Self::compute_sha256(dst))
                        && hash_src == hash_dst {
                            // Perfectly identical, no-op
                            return Ok(());
                        }
        }

        // Write to temporary file in the same parent directory and atomically rename to dst
        let tmp_name = format!(
            ".{}.tmp.{}.{}",
            dst.file_name().and_then(|f| f.to_str()).unwrap_or("tmp"),
            std::process::id(),
            chrono::Utc::now().timestamp_nanos_opt().unwrap_or(0)
        );
        let tmp_dst = parent.join(tmp_name);

        // Attempt APFS zero-copy clonefile on macOS first to the tmp file
        let mut cloned = false;
        #[cfg(target_os = "macos")]
        {
            let src_c = CString::new(src.as_os_str().as_bytes())
                .context("Failed to convert src path to CString")?;
            let tmp_dst_c = CString::new(tmp_dst.as_os_str().as_bytes())
                .context("Failed to convert dst path to CString")?;

            let ret = unsafe { libc::clonefile(src_c.as_ptr(), tmp_dst_c.as_ptr(), 0) };
            if ret == 0 {
                cloned = true;
            }
        }

        if !cloned {
            std::fs::copy(src, &tmp_dst)?;
        }

        // Atomically rename temporary file to destination
        std::fs::rename(&tmp_dst, dst)?;
        Ok(())
    }

    /// Computes streaming SHA256 of a file
    pub fn compute_sha256<P: AsRef<Path>>(path: P) -> Result<String> {
        let mut file = File::open(path)?;
        let mut hasher = Sha256::new();
        let mut buffer = [0u8; 65536]; // 64KB chunks

        loop {
            let n = file.read(&mut buffer)?;
            if n == 0 {
                break;
            }
            hasher.update(&buffer[..n]);
        }

        Ok(hex::encode(hasher.finalize()))
    }

    /// Saves content to global content-addressable storage (CAS) by hash using atomic temporary write
    pub fn store_content(&self, content: &[u8]) -> Result<String> {
        let mut hasher = Sha256::new();
        hasher.update(content);
        let hash = hex::encode(hasher.finalize());

        let prefix = &hash[..2];
        let rest = &hash[2..];
        let dir = self.cache_root.join("objects").join(prefix);
        std::fs::create_dir_all(&dir)?;

        let object_path = dir.join(rest);
        if !object_path.exists() {
            let tmp_name = format!(
                ".{}.tmp.{}.{}",
                rest,
                std::process::id(),
                chrono::Utc::now().timestamp_nanos_opt().unwrap_or(0)
            );
            let tmp_path = dir.join(tmp_name);
            {
                let mut file = File::create(&tmp_path)?;
                file.write_all(content)?;
                file.sync_all()?;
            }
            std::fs::rename(&tmp_path, &object_path)?;
        }

        Ok(hash)
    }

    /// Saves a file directly to CAS using streaming I/O to avoid holding large files in memory.
    /// Computes SHA256 while streaming into an atomic temporary file.
    pub fn store_file<P: AsRef<Path>>(&self, path: P) -> Result<(String, u64)> {
        let path = path.as_ref();
        let mut src_file = File::open(path)?;

        let objects_dir = self.cache_root.join("objects");
        std::fs::create_dir_all(&objects_dir)?;

        let tmp_name = format!(
            ".cas_stream_tmp_{}_{}",
            std::process::id(),
            chrono::Utc::now().timestamp_nanos_opt().unwrap_or(0)
        );
        let tmp_path = objects_dir.join(tmp_name);

        let mut hasher = Sha256::new();
        let mut total_size = 0u64;

        {
            let mut tmp_file = File::create(&tmp_path)?;
            let mut buffer = [0u8; 65536]; // 64KB buffer
            loop {
                let n = src_file.read(&mut buffer)?;
                if n == 0 {
                    break;
                }
                hasher.update(&buffer[..n]);
                tmp_file.write_all(&buffer[..n])?;
                total_size += n as u64;
            }
            tmp_file.sync_all()?;
        }

        let hash = hex::encode(hasher.finalize());
        let prefix = &hash[..2];
        let rest = &hash[2..];
        let target_dir = objects_dir.join(prefix);
        std::fs::create_dir_all(&target_dir)?;

        let final_path = target_dir.join(rest);
        if final_path.exists() {
            let _ = std::fs::remove_file(&tmp_path);
        } else {
            std::fs::rename(&tmp_path, &final_path)?;
        }

        Ok((hash, total_size))
    }

    /// Retrieves content from global content-addressable storage (CAS) by hash
    pub fn get_content(&self, hash: &str) -> Result<Option<Vec<u8>>> {
        if hash.len() < 4 {
            return Ok(None);
        }
        let prefix = &hash[..2];
        let rest = &hash[2..];
        let object_path = self.cache_root.join("objects").join(prefix).join(rest);

        if !object_path.exists() {
            return Ok(None);
        }

        let mut file = File::open(object_path)?;
        let mut buf = Vec::new();
        file.read_to_end(&mut buf)?;
        Ok(Some(buf))
    }

    /// Returns the total size in bytes of the local cache directory
    pub fn current_cache_usage(&self) -> Result<u64> {
        let mut total = 0u64;
        for entry in walkdir::WalkDir::new(&self.cache_root)
            .into_iter()
            .filter_map(|e| e.ok())
        {
            if entry.file_type().is_file() {
                total += entry.metadata()?.len();
            }
        }
        Ok(total)
    }

    /// Evaluates if cache clean is needed based on configured threshold
    pub fn is_over_capacity(&self) -> Result<bool> {
        Ok(self.current_cache_usage()? > self.max_cache_bytes)
    }

    /// Evicts oldest accessed CAS objects (LRU) until cache usage falls below max_cache_bytes.
    /// Returns the total bytes freed.
    pub fn evict_lru(&self) -> Result<u64> {
        let objects_dir = self.cache_root.join("objects");
        if !objects_dir.exists() {
            return Ok(0);
        }

        struct CacheObject {
            path: PathBuf,
            size: u64,
            last_accessed: std::time::SystemTime,
        }

        let mut objects: Vec<CacheObject> = Vec::new();
        let mut current_size = 0u64;

        for entry in walkdir::WalkDir::new(&objects_dir)
            .into_iter()
            .filter_map(|e| e.ok())
        {
            if entry.file_type().is_file()
                && let Ok(meta) = entry.metadata() {
                    let len = meta.len();
                    current_size += len;
                    let last_accessed = meta
                        .accessed()
                        .or_else(|_| meta.modified())
                        .unwrap_or(std::time::SystemTime::UNIX_EPOCH);
                    objects.push(CacheObject {
                        path: entry.path().to_path_buf(),
                        size: len,
                        last_accessed,
                    });
                }
        }

        if current_size <= self.max_cache_bytes {
            return Ok(0);
        }

        // Sort by last accessed ascending (oldest first)
        objects.sort_by_key(|o| o.last_accessed);

        let mut evicted_bytes = 0u64;
        let target_size = (self.max_cache_bytes as f64 * 0.85) as u64;

        for obj in objects {
            if current_size <= target_size {
                break;
            }
            if std::fs::remove_file(&obj.path).is_ok() {
                current_size = current_size.saturating_sub(obj.size);
                evicted_bytes += obj.size;
            }
        }

        Ok(evicted_bytes)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn test_cas_store_and_retrieve() -> Result<()> {
        let dir = tempdir()?;
        let mgr = CacheManager::new(dir.path(), 10 * 1024 * 1024 * 1024)?;

        let data = b"Hello DevDrive APFS Cache!";
        let hash = mgr.store_content(data)?;
        let retrieved = mgr.get_content(&hash)?.expect("Content should be found");

        assert_eq!(retrieved, data);
        Ok(())
    }

    #[test]
    fn test_clone_file() -> Result<()> {
        let dir = tempdir()?;
        let src = dir.path().join("source.txt");
        let dst = dir.path().join("clone.txt");

        std::fs::write(&src, "Important code line")?;
        CacheManager::clone_file(&src, &dst)?;

        assert!(dst.exists());
        let content = std::fs::read_to_string(&dst)?;
        assert_eq!(content, "Important code line");
        Ok(())
    }

    #[test]
    fn test_store_file_and_eviction() -> Result<()> {
        let dir = tempdir()?;
        let test_file = dir.path().join("sample.dat");
        std::fs::write(&test_file, b"Large streaming file data test payload")?;

        let mgr = CacheManager::new(dir.path().join("cache"), 30)?; // very small max cache
        let (hash, size) = mgr.store_file(&test_file)?;
        assert_eq!(size, 38);

        let retrieved = mgr.get_content(&hash)?.expect("Content should exist in CAS");
        assert_eq!(retrieved, b"Large streaming file data test payload");

        assert!(mgr.is_over_capacity()?);
        let freed = mgr.evict_lru()?;
        assert!(freed > 0);
        assert!(!mgr.is_over_capacity()?);

        Ok(())
    }
}
