//! Local ALTERID checkpoint persistence.
//! Checkpoint advances only after backend HTTP 200 acknowledgment.

use std::fs;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::errors::CheckpointError;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Checkpoint {
    pub last_known_alter_id: u64,
    pub last_successful_sync: Option<String>,
    pub backfill_complete: bool,
}

impl Default for Checkpoint {
    fn default() -> Self {
        Self {
            last_known_alter_id: 0,
            last_successful_sync: None,
            backfill_complete: false,
        }
    }
}

pub struct CheckpointStore {
    path: PathBuf,
}

impl Clone for CheckpointStore {
    fn clone(&self) -> Self {
        Self {
            path: self.path.clone(),
        }
    }
}

impl CheckpointStore {
    pub fn new(path: PathBuf) -> Self {
        Self { path }
    }

    pub fn default_path() -> PathBuf {
        dirs::data_local_dir()
            .unwrap_or_else(|| PathBuf::from("."))
            .join("FinInsight")
            .join("TallyAgent")
            .join("checkpoint.json")
    }

    pub fn load(&self) -> Result<Checkpoint, CheckpointError> {
        if !self.path.exists() {
            return Ok(Checkpoint::default());
        }
        let data = fs::read_to_string(&self.path)
            .map_err(|e| CheckpointError::Read(e.to_string()))?;
        serde_json::from_str(&data).map_err(|e| CheckpointError::Read(e.to_string()))
    }

    pub fn save(&self, checkpoint: &Checkpoint) -> Result<(), CheckpointError> {
        if let Some(parent) = self.path.parent() {
            fs::create_dir_all(parent)
                .map_err(|e| CheckpointError::Write(e.to_string()))?;
        }
        let data = serde_json::to_string_pretty(checkpoint)
            .map_err(|e| CheckpointError::Write(e.to_string()))?;
        fs::write(&self.path, data).map_err(|e| CheckpointError::Write(e.to_string()))
    }

    /// Advance checkpoint only after confirmed backend receipt.
    pub fn advance_on_ack(
        &self,
        new_alter_id: u64,
        mark_backfill_complete: bool,
    ) -> Result<Checkpoint, CheckpointError> {
        let mut cp = self.load()?;
        cp.last_known_alter_id = new_alter_id;
        cp.last_successful_sync = Some(chrono::Utc::now().to_rfc3339());
        if mark_backfill_complete {
            cp.backfill_complete = true;
        }
        self.save(&cp)?;
        Ok(cp)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn checkpoint_unchanged_until_advance() {
        let dir = tempdir().unwrap();
        let store = CheckpointStore::new(dir.path().join("cp.json"));
        let cp = store.load().unwrap();
        assert_eq!(cp.last_known_alter_id, 0);

        store.advance_on_ack(500, false).unwrap();
        let cp2 = store.load().unwrap();
        assert_eq!(cp2.last_known_alter_id, 500);
    }
}
