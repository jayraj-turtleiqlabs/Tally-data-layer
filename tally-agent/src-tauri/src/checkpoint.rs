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
        let data = match fs::read_to_string(&self.path) {
            Ok(d) => d,
            Err(e) => {
                log::warn!("[checkpoint] Failed to read checkpoint file ({e}). Resetting to default.");
                let default_cp = Checkpoint::default();
                let _ = self.save(&default_cp);
                return Ok(default_cp);
            }
        };

        let trimmed = data.trim_matches(|c: char| c.is_whitespace() || c == '\0');
        if trimmed.is_empty() {
            let default_cp = Checkpoint::default();
            let _ = self.save(&default_cp);
            return Ok(default_cp);
        }

        match serde_json::from_str(trimmed) {
            Ok(cp) => Ok(cp),
            Err(e) => {
                log::warn!("[checkpoint] Failed to parse checkpoint JSON ({e}). Resetting to default.");
                let default_cp = Checkpoint::default();
                let _ = self.save(&default_cp);
                Ok(default_cp)
            }
        }
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

    #[test]
    fn checkpoint_handles_empty_and_corrupt_files() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("empty_cp.json");

        // Empty file
        fs::write(&path, "").unwrap();
        let store = CheckpointStore::new(path.clone());
        let cp = store.load().unwrap();
        assert_eq!(cp, Checkpoint::default());

        // Whitespace-only file
        fs::write(&path, "   \n\t  ").unwrap();
        let cp = store.load().unwrap();
        assert_eq!(cp, Checkpoint::default());

        // Null-byte padded file
        fs::write(&path, "\0\0\0\0\0\0\0\0").unwrap();
        let cp = store.load().unwrap();
        assert_eq!(cp, Checkpoint::default());

        // Malformed JSON
        fs::write(&path, "{invalid-json").unwrap();
        let cp = store.load().unwrap();
        assert_eq!(cp, Checkpoint::default());
    }
}
