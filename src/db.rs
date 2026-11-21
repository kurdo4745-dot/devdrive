use anyhow::{Context, Result};
use rusqlite::{params, Connection};
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct ProjectRecord {
    pub id: i64,
    pub name: String,
    pub local_path: String,
    pub remote_path: String,
    pub created_at: String,
    pub last_synced_at: Option<String>,
    pub status: String,
    pub archived_at: Option<String>,
    pub archive_remote_path: Option<String>,
    pub quarantine_path: Option<String>,
    pub quarantine_until: Option<String>,
    pub manifest_sha256: Option<String>,
    pub freed_bytes: i64,
    pub pinned: bool,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct OffloadJournalRecord {
    pub id: i64,
    pub project_id: i64,
    pub step: String,
    pub state: String,
    pub ts: i64,
    pub detail: String,
}

#[derive(Debug, Clone)]
pub struct FileMetadataRecord {
    pub id: i64,
    pub project_id: i64,
    pub relative_path: String,
    pub size_bytes: i64,
    pub sha256: String,
    pub category: String,
    pub last_modified: i64,
    pub last_accessed: i64,
    pub is_synced: bool,
}

#[derive(Debug, Clone)]
pub struct SnapshotRecord {
    pub id: String,
    pub project_id: i64,
    pub message: String,
    pub created_at: String,
    pub manifest_json: String,
}

#[derive(Debug, Clone)]
pub struct WalEventRecord {
    pub id: i64,
    pub project_id: i64,
    pub event_type: String,
    pub relative_path: String,
    pub timestamp: i64,
    pub synced: bool,
    pub superseded_ids: Vec<i64>,
}

impl WalEventRecord {
    pub fn all_ids(&self) -> Vec<i64> {
        let mut ids = vec![self.id];
        ids.extend_from_slice(&self.superseded_ids);
        ids
    }
}

pub struct Database {
    conn: Connection,
}

impl Database {
    /// Opens or creates the SQLite database at the specified path and initializes WAL mode
    pub fn open<P: AsRef<Path>>(path: P) -> Result<Self> {
        if let Some(parent) = path.as_ref().parent() {
            std::fs::create_dir_all(parent)?;
        }
        let conn = Connection::open(path)?;

        // Performance & concurrency pragmas
        conn.execute_batch(
            "PRAGMA journal_mode = WAL;
             PRAGMA busy_timeout = 5000;
             PRAGMA synchronous = NORMAL;
             PRAGMA foreign_keys = ON;
             PRAGMA cache_size = -64000; -- 64MB cache",
        )?;

        let db = Self { conn };
        db.init_schema()?;
        Ok(db)
    }

    /// Default database location at ~/.config/devdrive/devdrive.db
    pub fn default_path() -> PathBuf {
        dirs::home_dir()
            .unwrap_or_else(|| PathBuf::from("."))
            .join(".config")
            .join("devdrive")
            .join("devdrive.db")
    }

    fn init_schema(&self) -> Result<()> {
        self.conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS projects (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                name TEXT NOT NULL,
                local_path TEXT NOT NULL UNIQUE,
                remote_path TEXT NOT NULL,
                created_at TEXT NOT NULL,
                last_synced_at TEXT,
                status TEXT NOT NULL DEFAULT 'active',
                archived_at TEXT,
                archive_remote_path TEXT,
                quarantine_path TEXT,
                quarantine_until TEXT,
                manifest_sha256 TEXT,
                freed_bytes INTEGER NOT NULL DEFAULT 0,
                pinned INTEGER NOT NULL DEFAULT 0
            );

            CREATE TABLE IF NOT EXISTS files_metadata (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                project_id INTEGER NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
                relative_path TEXT NOT NULL,
                size_bytes INTEGER NOT NULL,
                sha256 TEXT NOT NULL,
                category TEXT NOT NULL,
                last_modified INTEGER NOT NULL,
                last_accessed INTEGER NOT NULL,
                is_synced INTEGER NOT NULL DEFAULT 0,
                UNIQUE(project_id, relative_path)
            );

            CREATE TABLE IF NOT EXISTS snapshots (
                id TEXT PRIMARY KEY,
                project_id INTEGER NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
                message TEXT NOT NULL,
                created_at TEXT NOT NULL,
                manifest_json TEXT NOT NULL
            );

            CREATE TABLE IF NOT EXISTS wal_events (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                project_id INTEGER NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
                event_type TEXT NOT NULL,
                relative_path TEXT NOT NULL,
                timestamp INTEGER NOT NULL,
                synced INTEGER NOT NULL DEFAULT 0
            );

            CREATE TABLE IF NOT EXISTS offload_journal (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                project_id INTEGER NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
                step TEXT NOT NULL,
                state TEXT NOT NULL,
                ts INTEGER NOT NULL,
                detail TEXT NOT NULL DEFAULT ''
            );

            CREATE INDEX IF NOT EXISTS idx_files_project_accessed 
                ON files_metadata(project_id, last_accessed);
            CREATE INDEX IF NOT EXISTS idx_wal_synced 
                ON wal_events(synced, timestamp);
            CREATE INDEX IF NOT EXISTS idx_journal_proj 
                ON offload_journal(project_id, ts);",
        )?;

        // Safe column migrations for existing databases
        let columns: std::collections::HashSet<String> = {
            let mut stmt = self.conn.prepare("PRAGMA table_info(projects)")?;
            let rows = stmt.query_map([], |row| row.get(1))?;
            rows.filter_map(|r| r.ok()).collect()
        };
        if !columns.contains("status") {
            let _ = self.conn.execute("ALTER TABLE projects ADD COLUMN status TEXT NOT NULL DEFAULT 'active'", []);
        }
        if !columns.contains("archived_at") {
            let _ = self.conn.execute("ALTER TABLE projects ADD COLUMN archived_at TEXT", []);
        }
        if !columns.contains("archive_remote_path") {
            let _ = self.conn.execute("ALTER TABLE projects ADD COLUMN archive_remote_path TEXT", []);
        }
        if !columns.contains("quarantine_path") {
            let _ = self.conn.execute("ALTER TABLE projects ADD COLUMN quarantine_path TEXT", []);
        }
        if !columns.contains("quarantine_until") {
            let _ = self.conn.execute("ALTER TABLE projects ADD COLUMN quarantine_until TEXT", []);
        }
        if !columns.contains("manifest_sha256") {
            let _ = self.conn.execute("ALTER TABLE projects ADD COLUMN manifest_sha256 TEXT", []);
        }
        if !columns.contains("freed_bytes") {
            let _ = self.conn.execute("ALTER TABLE projects ADD COLUMN freed_bytes INTEGER NOT NULL DEFAULT 0", []);
        }
        if !columns.contains("pinned") {
            let _ = self.conn.execute("ALTER TABLE projects ADD COLUMN pinned INTEGER NOT NULL DEFAULT 0", []);
        }

        Ok(())
    }

    fn map_project_row(row: &rusqlite::Row) -> rusqlite::Result<ProjectRecord> {
        Ok(ProjectRecord {
            id: row.get(0)?,
            name: row.get(1)?,
            local_path: row.get(2)?,
            remote_path: row.get(3)?,
            created_at: row.get(4)?,
            last_synced_at: row.get(5)?,
            status: row.get(6)?,
            archived_at: row.get(7)?,
            archive_remote_path: row.get(8)?,
            quarantine_path: row.get(9)?,
            quarantine_until: row.get(10)?,
            manifest_sha256: row.get(11)?,
            freed_bytes: row.get(12)?,
            pinned: row.get::<_, i64>(13)? != 0,
        })
    }

    const PROJECT_SELECT_COLS: &'static str = 
        "id, name, local_path, remote_path, created_at, last_synced_at, status, archived_at, archive_remote_path, quarantine_path, quarantine_until, manifest_sha256, freed_bytes, pinned";

    pub fn set_project_pinned(&mut self, project_id: i64, pinned: bool) -> Result<()> {
        let val = if pinned { 1 } else { 0 };
        self.conn.execute(
            "UPDATE projects SET pinned = ?1 WHERE id = ?2",
            params![val, project_id],
        )?;
        Ok(())
    }

    pub fn register_project(
        &mut self,
        name: &str,
        local_path: &str,
        remote_path: &str,
    ) -> Result<ProjectRecord> {
        let now = chrono::Utc::now().to_rfc3339();
        let default_pinned = if name == "adventurous-hypatia" { 1 } else { 0 };
        self.conn.execute(
            "INSERT INTO projects (name, local_path, remote_path, created_at, status, pinned)
             VALUES (?1, ?2, ?3, ?4, 'active', ?5)
             ON CONFLICT(local_path) DO UPDATE SET name = ?1, remote_path = ?3",
            params![name, local_path, remote_path, now, default_pinned],
        )?;

        if name == "adventurous-hypatia"
            && let Ok(Some(proj)) = self.get_project_by_local_path(local_path) {
                let _ = self.set_project_pinned(proj.id, true);
            }

        self.get_project_by_local_path(local_path)?
            .context("Project could not be found after insertion")
    }

    pub fn unregister_project(&mut self, project_id: i64) -> Result<()> {
        self.conn.execute("DELETE FROM projects WHERE id = ?1", params![project_id])?;
        Ok(())
    }

    pub fn unregister_all_projects(&mut self) -> Result<usize> {
        let count = self.conn.execute("DELETE FROM projects", [])?;
        Ok(count)
    }

    pub fn get_project_by_id(&self, id: i64) -> Result<Option<ProjectRecord>> {
        let query = format!("SELECT {} FROM projects WHERE id = ?1", Self::PROJECT_SELECT_COLS);
        let mut stmt = self.conn.prepare(&query)?;
        let mut rows = stmt.query(params![id])?;
        if let Some(row) = rows.next()? {
            Ok(Some(Self::map_project_row(row)?))
        } else {
            Ok(None)
        }
    }

    pub fn get_project_by_local_path(&self, local_path: &str) -> Result<Option<ProjectRecord>> {
        let query = format!("SELECT {} FROM projects WHERE local_path = ?1", Self::PROJECT_SELECT_COLS);
        let mut stmt = self.conn.prepare(&query)?;
        let mut rows = stmt.query(params![local_path])?;
        if let Some(row) = rows.next()? {
            Ok(Some(Self::map_project_row(row)?))
        } else {
            Ok(None)
        }
    }

    pub fn get_project_by_remote_path(&self, remote_path: &str) -> Result<Option<ProjectRecord>> {
        let query = format!("SELECT {} FROM projects WHERE remote_path = ?1", Self::PROJECT_SELECT_COLS);
        let mut stmt = self.conn.prepare(&query)?;
        let mut rows = stmt.query(params![remote_path])?;
        if let Some(row) = rows.next()? {
            Ok(Some(Self::map_project_row(row)?))
        } else {
            Ok(None)
        }
    }

    pub fn get_project_by_name(&self, name: &str) -> Result<Option<ProjectRecord>> {
        let query = format!("SELECT {} FROM projects WHERE name = ?1", Self::PROJECT_SELECT_COLS);
        let mut stmt = self.conn.prepare(&query)?;
        let mut rows = stmt.query(params![name])?;
        if let Some(row) = rows.next()? {
            Ok(Some(Self::map_project_row(row)?))
        } else {
            Ok(None)
        }
    }

    pub fn list_projects(&self) -> Result<Vec<ProjectRecord>> {
        let query = format!("SELECT {} FROM projects", Self::PROJECT_SELECT_COLS);
        let mut stmt = self.conn.prepare(&query)?;
        let rows = stmt.query_map([], Self::map_project_row)?;

        let mut list = Vec::new();
        for r in rows {
            list.push(r?);
        }
        Ok(list)
    }

    pub fn update_project_last_synced(&mut self, project_id: i64, timestamp: &str) -> Result<()> {
        self.conn.execute(
            "UPDATE projects SET last_synced_at = ?1 WHERE id = ?2",
            params![timestamp, project_id],
        )?;
        Ok(())
    }

    pub fn update_project_status(&mut self, project_id: i64, status: &str) -> Result<()> {
        self.conn.execute(
            "UPDATE projects SET status = ?1 WHERE id = ?2",
            params![status, project_id],
        )?;
        Ok(())
    }

    #[allow(clippy::too_many_arguments)]
    pub fn update_project_offload_info(
        &mut self,
        project_id: i64,
        status: &str,
        archived_at: Option<&str>,
        archive_remote_path: Option<&str>,
        quarantine_path: Option<&str>,
        quarantine_until: Option<&str>,
        manifest_sha256: Option<&str>,
        freed_bytes: i64,
    ) -> Result<()> {
        self.conn.execute(
            "UPDATE projects SET 
                status = ?1,
                archived_at = ?2,
                archive_remote_path = ?3,
                quarantine_path = ?4,
                quarantine_until = ?5,
                manifest_sha256 = ?6,
                freed_bytes = ?7
             WHERE id = ?8",
            params![
                status,
                archived_at,
                archive_remote_path,
                quarantine_path,
                quarantine_until,
                manifest_sha256,
                freed_bytes,
                project_id,
            ],
        )?;
        Ok(())
    }

    pub fn mark_file_synced(&mut self, project_id: i64, relative_path: &str) -> Result<()> {
        self.conn.execute(
            "UPDATE files_metadata SET is_synced = 1 WHERE project_id = ?1 AND relative_path = ?2",
            params![project_id, relative_path],
        )?;
        Ok(())
    }

    pub fn record_offload_journal(
        &mut self,
        project_id: i64,
        step: &str,
        state: &str,
        detail: &str,
    ) -> Result<()> {
        let ts = chrono::Utc::now().timestamp();
        self.conn.execute(
            "INSERT INTO offload_journal (project_id, step, state, ts, detail)
             VALUES (?1, ?2, ?3, ?4, ?5)",
            params![project_id, step, state, ts, detail],
        )?;
        Ok(())
    }

    pub fn get_offload_journal(&self, project_id: i64) -> Result<Vec<OffloadJournalRecord>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, project_id, step, state, ts, detail 
             FROM offload_journal WHERE project_id = ?1 ORDER BY ts ASC, id ASC",
        )?;
        let rows = stmt.query_map(params![project_id], |row| {
            Ok(OffloadJournalRecord {
                id: row.get(0)?,
                project_id: row.get(1)?,
                step: row.get(2)?,
                state: row.get(3)?,
                ts: row.get(4)?,
                detail: row.get(5)?,
            })
        })?;
        let mut list = Vec::new();
        for r in rows {
            list.push(r?);
        }
        Ok(list)
    }

    pub fn clear_offload_journal(&mut self, project_id: i64) -> Result<()> {
        self.conn.execute(
            "DELETE FROM offload_journal WHERE project_id = ?1",
            params![project_id],
        )?;
        Ok(())
    }

    #[allow(clippy::too_many_arguments)]
    pub fn upsert_file_metadata(
        &mut self,
        project_id: i64,
        relative_path: &str,
        size_bytes: i64,
        sha256: &str,
        category: &str,
        last_modified: i64,
        last_accessed: i64,
        is_synced: bool,
    ) -> Result<()> {
        self.conn.execute(
            "INSERT INTO files_metadata 
                (project_id, relative_path, size_bytes, sha256, category, last_modified, last_accessed, is_synced)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)
             ON CONFLICT(project_id, relative_path) DO UPDATE SET
                size_bytes = ?3,
                sha256 = ?4,
                category = ?5,
                last_modified = ?6,
                last_accessed = ?7,
                is_synced = ?8",
            params![
                project_id,
                relative_path,
                size_bytes,
                sha256,
                category,
                last_modified,
                last_accessed,
                if is_synced { 1 } else { 0 }
            ],
        )?;
        Ok(())
    }

    pub fn get_total_tracked_size(&self) -> Result<i64> {
        let mut stmt = self.conn.prepare("SELECT COALESCE(SUM(size_bytes), 0) FROM files_metadata")?;
        let sum: i64 = stmt.query_row([], |r| r.get(0))?;
        Ok(sum)
    }

    pub fn record_wal_event(
        &mut self,
        project_id: i64,
        event_type: &str,
        relative_path: &str,
        timestamp: i64,
    ) -> Result<()> {
        self.conn.execute(
            "INSERT INTO wal_events (project_id, event_type, relative_path, timestamp, synced)
             VALUES (?1, ?2, ?3, ?4, 0)",
            params![project_id, event_type, relative_path, timestamp],
        )?;
        Ok(())
    }

    pub fn delete_file_metadata(&mut self, project_id: i64, relative_path: &str) -> Result<()> {
        self.conn.execute(
            "DELETE FROM files_metadata WHERE project_id = ?1 AND relative_path = ?2",
            params![project_id, relative_path],
        )?;
        Ok(())
    }

    pub fn is_tracked_file(&self, project_id: i64, relative_path: &str) -> Result<bool> {
        let mut stmt = self.conn.prepare(
            "SELECT 1 FROM files_metadata WHERE project_id = ?1 AND relative_path = ?2",
        )?;
        let exists = stmt.exists(params![project_id, relative_path])?;
        Ok(exists)
    }

    pub fn get_pending_wal_events(&self, project_id: i64) -> Result<Vec<WalEventRecord>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, project_id, event_type, relative_path, timestamp, synced
             FROM wal_events WHERE project_id = ?1 AND synced = 0 ORDER BY timestamp ASC",
        )?;
        let rows = stmt.query_map(params![project_id], |row| {
            Ok(WalEventRecord {
                id: row.get(0)?,
                project_id: row.get(1)?,
                event_type: row.get(2)?,
                relative_path: row.get(3)?,
                timestamp: row.get(4)?,
                synced: row.get::<_, i64>(5)? == 1,
                superseded_ids: Vec::new(),
            })
        })?;

        let mut list = Vec::new();
        for r in rows {
            list.push(r?);
        }
        Ok(list)
    }

    pub fn get_all_pending_wal_events(&self) -> Result<Vec<WalEventRecord>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, project_id, event_type, relative_path, timestamp, synced
             FROM wal_events WHERE synced = 0 ORDER BY timestamp ASC",
        )?;
        let rows = stmt.query_map([], |row| {
            Ok(WalEventRecord {
                id: row.get(0)?,
                project_id: row.get(1)?,
                event_type: row.get(2)?,
                relative_path: row.get(3)?,
                timestamp: row.get(4)?,
                synced: row.get::<_, i64>(5)? == 1,
                superseded_ids: Vec::new(),
            })
        })?;

        let mut list = Vec::new();
        for r in rows {
            list.push(r?);
        }
        Ok(list)
    }

    pub fn mark_wal_events_synced(&mut self, ids: &[i64]) -> Result<()> {
        if ids.is_empty() {
            return Ok(());
        }
        let id_strs: Vec<String> = ids.iter().map(|id| id.to_string()).collect();
        let query = format!(
            "UPDATE wal_events SET synced = 1 WHERE id IN ({})",
            id_strs.join(",")
        );
        self.conn.execute(&query, [])?;
        self.prune_synced_wal_events()?;
        Ok(())
    }

    /// Prunes all synchronized events from the WAL table (DELETE FROM wal_events WHERE synced = 1)
    pub fn prune_synced_wal_events(&mut self) -> Result<usize> {
        let deleted = self.conn.execute("DELETE FROM wal_events WHERE synced = 1", [])?;
        Ok(deleted)
    }

    /// Returns the total count of rows in the wal_events table
    pub fn get_total_wal_events_count(&self) -> Result<usize> {
        let count: i64 = self.conn.query_row("SELECT COUNT(*) FROM wal_events", [], |r| r.get(0))?;
        Ok(count as usize)
    }

    pub fn save_snapshot(
        &mut self,
        id: &str,
        project_id: i64,
        message: &str,
        manifest_json: &str,
    ) -> Result<()> {
        let now = chrono::Utc::now().to_rfc3339();
        self.conn.execute(
            "INSERT INTO snapshots (id, project_id, message, created_at, manifest_json)
             VALUES (?1, ?2, ?3, ?4, ?5)",
            params![id, project_id, message, now, manifest_json],
        )?;
        Ok(())
    }

    pub fn list_snapshots(&self, project_id: i64) -> Result<Vec<SnapshotRecord>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, project_id, message, created_at, manifest_json
             FROM snapshots WHERE project_id = ?1 ORDER BY created_at DESC",
        )?;
        let rows = stmt.query_map(params![project_id], |row| {
            Ok(SnapshotRecord {
                id: row.get(0)?,
                project_id: row.get(1)?,
                message: row.get(2)?,
                created_at: row.get(3)?,
                manifest_json: row.get(4)?,
            })
        })?;

        let mut list = Vec::new();
        for r in rows {
            list.push(r?);
        }
        Ok(list)
    }

    pub fn get_snapshot(&self, snapshot_id: &str) -> Result<Option<SnapshotRecord>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, project_id, message, created_at, manifest_json
             FROM snapshots WHERE id = ?1",
        )?;
        let mut rows = stmt.query(params![snapshot_id])?;
        if let Some(row) = rows.next()? {
            Ok(Some(SnapshotRecord {
                id: row.get(0)?,
                project_id: row.get(1)?,
                message: row.get(2)?,
                created_at: row.get(3)?,
                manifest_json: row.get(4)?,
            }))
        } else {
            Ok(None)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_db_init_and_project_registration() -> Result<()> {
        let mut db = Database::open(":memory:")?;
        let proj = db.register_project("my-saas", "/Users/serhat/my-saas", "drive://my-saas")?;
        assert_eq!(proj.name, "my-saas");
        assert_eq!(proj.local_path, "/Users/serhat/my-saas");
        assert!(!proj.pinned);

        let found = db.get_project_by_local_path("/Users/serhat/my-saas")?.unwrap();
        assert_eq!(found.id, proj.id);
        assert!(!found.pinned);

        // Test manual pin
        db.set_project_pinned(proj.id, true)?;
        let pinned_proj = db.get_project_by_id(proj.id)?.unwrap();
        assert!(pinned_proj.pinned);

        // Test adventurous-hypatia auto-pin
        let self_proj = db.register_project("adventurous-hypatia", "/Users/serhat/adventurous-hypatia", "drive://adventurous-hypatia")?;
        assert!(self_proj.pinned);

        Ok(())
    }

    #[test]
    fn test_wal_events() -> Result<()> {
        let mut db = Database::open(":memory:")?;
        let proj = db.register_project("test", "/tmp/test", "drive://test")?;
        db.record_wal_event(proj.id, "modify", "src/main.rs", 123456789)?;

        let pending = db.get_pending_wal_events(proj.id)?;
        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0].relative_path, "src/main.rs");

        db.mark_wal_events_synced(&[pending[0].id])?;
        let pending_after = db.get_pending_wal_events(proj.id)?;
        assert_eq!(pending_after.len(), 0);
        Ok(())
    }

    #[test]
    fn test_prune_synced_wal_events() -> Result<()> {
        let mut db = Database::open(":memory:")?;
        let proj = db.register_project("test", "/tmp/test", "drive://test")?;

        db.record_wal_event(proj.id, "create", "file1.rs", 100)?;
        db.record_wal_event(proj.id, "modify", "file2.rs", 200)?;

        let pending = db.get_pending_wal_events(proj.id)?;
        assert_eq!(pending.len(), 2);

        // Manually mark one as synced=1
        db.conn.execute("UPDATE wal_events SET synced = 1 WHERE id = ?1", params![pending[0].id])?;

        // Pruning should remove exactly 1 synced record
        let pruned = db.prune_synced_wal_events()?;
        assert_eq!(pruned, 1);

        // Verify remaining
        let remaining_pending = db.get_pending_wal_events(proj.id)?;
        assert_eq!(remaining_pending.len(), 1);
        assert_eq!(remaining_pending[0].id, pending[1].id);

        // Total count in table must be 1
        let total: i64 = db.conn.query_row("SELECT COUNT(*) FROM wal_events", [], |r| r.get(0))?;
        assert_eq!(total, 1);

        Ok(())
    }
}
