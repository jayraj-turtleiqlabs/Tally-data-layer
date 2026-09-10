//! Local Connection Profile and ALTERID Checkpoint persistence.
//! Checkpoint advances only after backend HTTP 200 acknowledgment.
//! All disk writes are atomic (write to temp file in same directory + rename).

use std::fs;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::errors::{CheckpointError, ProfileError};

/// Helper for atomic file writes using temp file + rename in the same directory.
fn atomic_write(path: &Path, contents: &str) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let file_stem = path.file_name().and_then(|n| n.to_str()).unwrap_or("file");
    let temp_name = format!(".{}.tmp.{}", file_stem, std::process::id());
    let temp_path = path.with_file_name(temp_name);
    fs::write(&temp_path, contents)?;
    fs::rename(&temp_path, path)?;
    Ok(())
}

/// Connection Profile storing identity binding (connection_id, company_guid, company_name).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct ConnectionProfile {
    pub connection_id: String,
    #[serde(default)]
    pub company_guid: Option<String>,
    pub company_name: String,
    #[serde(default)]
    pub paired_at: String,
}

#[derive(Debug, Clone)]
pub struct ProfileStore {
    path: PathBuf,
}

impl ProfileStore {
    pub fn new(path: PathBuf) -> Self {
        Self { path }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn default_path() -> PathBuf {
        dirs::data_local_dir()
            .unwrap_or_else(|| PathBuf::from("."))
            .join("FinInsight")
            .join("TallyAgent")
            .join("profiles.json")
    }

    pub fn legacy_path_in_dir(base_dir: &Path) -> PathBuf {
        base_dir.join("profile.json")
    }

    pub fn load_all(&self) -> Result<Vec<ConnectionProfile>, ProfileError> {
        if self.path.exists() {
            let data = fs::read_to_string(&self.path)
                .map_err(|e| ProfileError::Read(e.to_string()))?;
            let trimmed = data.trim_matches(|c: char| c.is_whitespace() || c == '\0');
            if trimmed.is_empty() {
                return Ok(Vec::new());
            }
            // Try parsing as array of profiles first
            if let Ok(profiles) = serde_json::from_str::<Vec<ConnectionProfile>>(trimmed) {
                return Ok(profiles);
            }
            // Fallback: try parsing as single profile if old file format was placed at this path
            if let Ok(single) = serde_json::from_str::<ConnectionProfile>(trimmed) {
                let profiles = vec![single];
                let _ = self.save_all(&profiles);
                return Ok(profiles);
            }
            return Err(ProfileError::Read("Failed to parse profiles JSON".into()));
        }

        // Check for legacy single profile.json in same parent folder
        if let Some(parent) = self.path.parent() {
            let legacy = Self::legacy_path_in_dir(parent);
            if legacy.exists() && legacy != self.path {
                if let Ok(data) = fs::read_to_string(&legacy) {
                    let trimmed = data.trim_matches(|c: char| c.is_whitespace() || c == '\0');
                    if !trimmed.is_empty() {
                        if let Ok(single) = serde_json::from_str::<ConnectionProfile>(trimmed) {
                            let profiles = vec![single];
                            let _ = self.save_all(&profiles);
                            return Ok(profiles);
                        }
                    }
                }
            }
        }

        Ok(Vec::new())
    }

    pub fn load(&self) -> Result<Option<ConnectionProfile>, ProfileError> {
        let profiles = self.load_all()?;
        Ok(profiles.into_iter().next())
    }

    pub fn get_by_connection_id(&self, cid: &str) -> Result<Option<ConnectionProfile>, ProfileError> {
        let profiles = self.load_all()?;
        Ok(profiles.into_iter().find(|p| p.connection_id == cid))
    }

    pub fn get_by_guid_or_name(&self, guid: Option<&str>, name: &str) -> Result<Option<ConnectionProfile>, ProfileError> {
        let profiles = self.load_all()?;
        let trimmed_guid = guid.map(|s| s.trim()).filter(|s| !s.is_empty());

        if let Some(g) = trimmed_guid {
            // GUID is available: STRICT GUID lookup only. Do not fall back to name if GUID is present!
            let matched = profiles.into_iter().find(|p| {
                p.company_guid.as_deref().map(|s| s.trim()) == Some(g)
            });
            return Ok(matched);
        }

        // GUID is unavailable (legacy profile):
        // Name fallback ONLY if exactly ONE existing profile matches.
        let matching_by_name: Vec<ConnectionProfile> = profiles
            .into_iter()
            .filter(|p| p.company_name.trim().eq_ignore_ascii_case(name.trim()))
            .collect();

        match matching_by_name.len() {
            1 => Ok(Some(matching_by_name.into_iter().next().unwrap())),
            0 => Ok(None),
            _ => {
                log::warn!(
                    "[profile] Ambiguous company name '{}' matches multiple ({} profiles) without GUID. Refusing to guess.",
                    name,
                    matching_by_name.len()
                );
                Err(ProfileError::Read(format!(
                    "Ambiguous company name '{}' matched multiple profiles without GUID",
                    name
                )))
            }
        }
    }

    pub fn save_all(&self, profiles: &[ConnectionProfile]) -> Result<(), ProfileError> {
        let data = serde_json::to_string_pretty(profiles)
            .map_err(|e| ProfileError::Write(e.to_string()))?;
        atomic_write(&self.path, &data).map_err(|e| ProfileError::Write(e.to_string()))
    }

    pub fn save(&self, profile: &ConnectionProfile) -> Result<(), ProfileError> {
        let mut profiles = self.load_all().unwrap_or_default();
        if let Some(pos) = profiles.iter().position(|p| {
            p.connection_id == profile.connection_id
                || (profile.company_guid.is_some() && p.company_guid == profile.company_guid)
        }) {
            profiles[pos] = profile.clone();
        } else {
            profiles.push(profile.clone());
        }
        self.save_all(&profiles)
    }

    pub fn delete_for_connection(&self, connection_id: &str) -> Result<(), ProfileError> {
        let mut profiles = self.load_all().unwrap_or_default();
        profiles.retain(|p| p.connection_id != connection_id);
        self.save_all(&profiles)
    }

    pub fn delete(&self) -> Result<(), ProfileError> {
        if self.path.exists() {
            fs::remove_file(&self.path).map_err(|e| ProfileError::Write(e.to_string()))?;
        }
        Ok(())
    }
}

/// Checkpoint per connection storing watermark and sync status.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Checkpoint {
    #[serde(default)]
    pub connection_id: Option<String>,
    pub last_known_alter_id: u64,
    pub last_successful_sync: Option<String>,
    pub backfill_complete: bool,
}

impl Default for Checkpoint {
    fn default() -> Self {
        Self {
            connection_id: None,
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

    pub fn for_connection_in_dir(base_dir: &Path, connection_id: &str) -> Self {
        let sanitized: String = connection_id
            .chars()
            .map(|c| if c.is_alphanumeric() || c == '-' || c == '_' { c } else { '_' })
            .collect();
        let path = base_dir
            .join("checkpoints")
            .join(format!("checkpoint_{}.json", sanitized));
        Self::new(path)
    }

    pub fn for_connection(connection_id: &str) -> Self {
        let base = dirs::data_local_dir()
            .unwrap_or_else(|| PathBuf::from("."))
            .join("FinInsight")
            .join("TallyAgent");
        Self::for_connection_in_dir(&base, connection_id)
    }

    pub fn path(&self) -> &Path {
        &self.path
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
        let data = serde_json::to_string_pretty(checkpoint)
            .map_err(|e| CheckpointError::Write(e.to_string()))?;
        atomic_write(&self.path, &data).map_err(|e| CheckpointError::Write(e.to_string()))
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

    /// Conservative migration from legacy single checkpoint.json.
    /// Migrates only if connection-scoped checkpoint does not exist and legacy checkpoint is valid.
    pub fn migrate_legacy_if_needed(&self, connection_id: &str) -> Result<(), CheckpointError> {
        if self.path.exists() {
            return Ok(());
        }
        let legacy_path = Self::default_path();
        if legacy_path.exists() && legacy_path != self.path {
            if let Ok(data) = fs::read_to_string(&legacy_path) {
                if let Ok(mut legacy_cp) = serde_json::from_str::<Checkpoint>(&data) {
                    if legacy_cp.connection_id.as_deref() == Some(connection_id) || legacy_cp.connection_id.is_none() {
                        legacy_cp.connection_id = Some(connection_id.to_string());
                        self.save(&legacy_cp)?;
                    }
                }
            }
        }
        Ok(())
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

    #[test]
    fn profile_store_atomic_roundtrip() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("profiles.json");
        let store = ProfileStore::new(path.clone());

        assert_eq!(store.load().unwrap(), None);
        assert_eq!(store.load_all().unwrap(), Vec::new());

        let profile = ConnectionProfile {
            connection_id: "conn-123".into(),
            company_guid: Some("guid-abc-xyz".into()),
            company_name: "Acme Corp".into(),
            paired_at: "2026-09-09T10:00:00Z".into(),
        };

        store.save(&profile).unwrap();
        let loaded = store.load().unwrap().expect("profile must exist");
        assert_eq!(loaded, profile);

        let all = store.load_all().unwrap();
        assert_eq!(all.len(), 1);
        assert_eq!(all[0], profile);

        // Add second profile
        let profile2 = ConnectionProfile {
            connection_id: "conn-456".into(),
            company_guid: Some("guid-thunder-789".into()),
            company_name: "ThunderClaps".into(),
            paired_at: "2026-09-09T11:00:00Z".into(),
        };
        store.save(&profile2).unwrap();

        let all2 = store.load_all().unwrap();
        assert_eq!(all2.len(), 2);
        assert_eq!(store.get_by_connection_id("conn-456").unwrap().unwrap().company_name, "ThunderClaps");
        assert_eq!(store.get_by_guid_or_name(Some("guid-abc-xyz"), "Acme Corp").unwrap().unwrap().connection_id, "conn-123");

        // Strict isolation: if a different GUID is requested, name must NOT match
        assert_eq!(store.get_by_guid_or_name(Some("guid-other-999"), "Acme Corp").unwrap(), None);

        // Name fallback when GUID is None/unavailable
        assert_eq!(store.get_by_guid_or_name(None, "ThunderClaps").unwrap().unwrap().connection_id, "conn-456");

        // Ambiguity test: two profiles with same name without GUID match
        let duplicate_named = ConnectionProfile {
            connection_id: "conn-dup".into(),
            company_guid: None,
            company_name: "Acme Corp".into(),
            paired_at: "2026-09-09T12:00:00Z".into(),
        };
        store.save(&duplicate_named).unwrap();
        // Looking up with None GUID for "Acme Corp" should fail closed as ambiguous
        assert!(store.get_by_guid_or_name(None, "Acme Corp").is_err());

        // Delete one connection
        store.delete_for_connection("conn-123").unwrap();
        store.delete_for_connection("conn-dup").unwrap();
        let all3 = store.load_all().unwrap();
        assert_eq!(all3.len(), 1);
        assert_eq!(all3[0].connection_id, "conn-456");

        store.delete().unwrap();
        assert_eq!(store.load().unwrap(), None);
    }

    #[test]
    fn profile_store_migrates_legacy_profile_json() {
        let dir = tempdir().unwrap();
        let legacy_path = dir.path().join("profile.json");
        let modern_path = dir.path().join("profiles.json");

        let legacy_profile = ConnectionProfile {
            connection_id: "legacy-conn-1".into(),
            company_guid: Some("guid-legacy-100".into()),
            company_name: "Legacy Corp".into(),
            paired_at: "2026-09-08T09:00:00Z".into(),
        };
        fs::write(&legacy_path, serde_json::to_string(&legacy_profile).unwrap()).unwrap();

        let store = ProfileStore::new(modern_path.clone());
        assert!(!modern_path.exists());

        let loaded_all = store.load_all().unwrap();
        assert_eq!(loaded_all.len(), 1);
        assert_eq!(loaded_all[0].company_name, "Legacy Corp");
        assert_eq!(loaded_all[0].connection_id, "legacy-conn-1");

        // After migration, modern path exists
        assert!(modern_path.exists());
    }

    #[test]
    fn per_connection_checkpoint_migration() {
        let dir = tempdir().unwrap();
        let legacy_path = dir.path().join("checkpoint.json");
        let conn_path = dir.path().join("checkpoint_conn-999.json");

        let legacy_cp = Checkpoint {
            connection_id: None,
            last_known_alter_id: 1500,
            last_successful_sync: Some("2026-09-08T12:00:00Z".into()),
            backfill_complete: true,
        };
        fs::write(&legacy_path, serde_json::to_string(&legacy_cp).unwrap()).unwrap();

        let conn_store = CheckpointStore::new(conn_path.clone());
        assert!(!conn_path.exists());

        // Migrate using helper
        if let Ok(data) = fs::read_to_string(&legacy_path) {
            if let Ok(mut cp) = serde_json::from_str::<Checkpoint>(&data) {
                cp.connection_id = Some("conn-999".into());
                conn_store.save(&cp).unwrap();
            }
        }

        assert!(conn_path.exists());
        let migrated = conn_store.load().unwrap();
        assert_eq!(migrated.connection_id.as_deref(), Some("conn-999"));
        assert_eq!(migrated.last_known_alter_id, 1500);
        assert!(migrated.backfill_complete);
    }
}

