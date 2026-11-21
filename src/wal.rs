use anyhow::Result;
use std::collections::HashMap;
use std::path::Path;
use tracing::info;

use crate::db::{Database, WalEventRecord};
use crate::filter::{DevFilter, FileCategory};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ChangeType {
    Created,
    Modified,
    Deleted,
}

impl ChangeType {
    pub fn as_str(&self) -> &'static str {
        match self {
            ChangeType::Created => "created",
            ChangeType::Modified => "modified",
            ChangeType::Deleted => "deleted",
        }
    }

    #[allow(clippy::should_implement_trait)]
    pub fn from_str(s: &str) -> Self {
        match s {
            "created" => ChangeType::Created,
            "deleted" => ChangeType::Deleted,
            _ => ChangeType::Modified,
        }
    }
}

pub struct WalManager<'a> {
    db: &'a mut Database,
    filter: DevFilter,
}

impl<'a> WalManager<'a> {
    pub fn new(db: &'a mut Database) -> Self {
        Self {
            db,
            filter: DevFilter::new(),
        }
    }

    /// Records an event to the persistent WAL if the file is syncable
    pub fn record_change<P: AsRef<Path>>(
        &mut self,
        project_id: i64,
        change: ChangeType,
        relative_path: P,
    ) -> Result<bool> {
        let relative_path = relative_path.as_ref();
        if self.filter.categorize(relative_path) != FileCategory::Source {
            // Ignore ephemeral or transient locks from WAL
            return Ok(false);
        }

        let now = chrono::Utc::now().timestamp_millis();
        let rel_str = relative_path.to_string_lossy().to_string();

        self.db
            .record_wal_event(project_id, change.as_str(), &rel_str, now)?;

        Ok(true)
    }

    /// Fetches pending unsynced events collapsed/debounced by relative path for a specific project,
    /// tracking all crushed previous event IDs so they can all be marked completed.
    pub fn get_debounced_pending(&self, project_id: i64) -> Result<Vec<WalEventRecord>> {
        let raw_events = self.db.get_pending_wal_events(project_id)?;
        let mut map: HashMap<String, (WalEventRecord, Vec<i64>)> = HashMap::new();

        for event in raw_events {
            let path = event.relative_path.clone();
            if let Some((survivor, superseded)) = map.get_mut(&path) {
                superseded.push(survivor.id);
                superseded.append(&mut survivor.superseded_ids);
                *survivor = event;
            } else {
                map.insert(path, (event, Vec::new()));
            }
        }

        let mut list: Vec<WalEventRecord> = map
            .into_values()
            .map(|(mut survivor, superseded)| {
                survivor.superseded_ids = superseded;
                survivor
            })
            .collect();
        list.sort_by_key(|e| e.timestamp);
        Ok(list)
    }

    /// Marks a batch of event IDs as successfully synchronized
    pub fn mark_completed(&mut self, event_ids: &[i64]) -> Result<()> {
        self.db.mark_wal_events_synced(event_ids)?;
        info!("Flushed and marked {} WAL events as synced", event_ids.len());
        Ok(())
    }

    /// Prunes all synchronized WAL events from database (DELETE FROM wal_events WHERE synced = 1)
    pub fn prune_synced_wal_events(&mut self) -> Result<usize> {
        self.db.prune_synced_wal_events()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_wal_debouncing() -> Result<()> {
        let mut db = Database::open(":memory:")?;
        let proj = db.register_project("test", "/tmp/test", "drive://test")?;

        let mut wal = WalManager::new(&mut db);
        wal.record_change(proj.id, ChangeType::Created, Path::new("src/main.rs"))?;
        wal.record_change(proj.id, ChangeType::Modified, Path::new("src/main.rs"))?;
        wal.record_change(proj.id, ChangeType::Modified, Path::new("src/main.rs"))?;

        // Also test ignored file
        let recorded =
            wal.record_change(proj.id, ChangeType::Created, Path::new("node_modules/foo.js"))?;
        assert!(!recorded);

        let pending = wal.get_debounced_pending(proj.id)?;
        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0].relative_path, "src/main.rs");
        assert_eq!(pending[0].superseded_ids.len(), 2);
        assert_eq!(pending[0].all_ids().len(), 3);

        wal.mark_completed(&pending[0].all_ids())?;
        let remaining = wal.get_debounced_pending(proj.id)?;
        assert_eq!(remaining.len(), 0);

        Ok(())
    }

    #[test]
    fn test_wal_manager_prune_synced_events() -> Result<()> {
        let mut db = Database::open(":memory:")?;
        let proj = db.register_project("test", "/tmp/test", "drive://test")?;

        let mut wal = WalManager::new(&mut db);
        wal.record_change(proj.id, ChangeType::Created, Path::new("file1.rs"))?;
        wal.record_change(proj.id, ChangeType::Modified, Path::new("file2.rs"))?;

        let pending = wal.get_debounced_pending(proj.id)?;
        assert_eq!(pending.len(), 2);

        // Mark first completed -> mark_wal_events_synced sets synced=1 and prunes
        wal.mark_completed(&[pending[0].id])?;

        let remaining = wal.get_debounced_pending(proj.id)?;
        assert_eq!(remaining.len(), 1);
        assert_eq!(remaining[0].id, pending[1].id);

        // Manual prune returns 0 since already pruned
        let pruned = wal.prune_synced_wal_events()?;
        assert_eq!(pruned, 0);

        Ok(())
    }
}
