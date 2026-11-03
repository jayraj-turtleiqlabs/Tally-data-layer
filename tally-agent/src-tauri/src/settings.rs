//! Application Settings persistence and configuration.
//! Stores user preferences (autostart, auto-sync timer, snooze state) in Local AppData.

use std::fs;
use std::path::{Path, PathBuf};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct AppSettings {
    pub autostart_enabled: bool,
    pub auto_sync_enabled: bool,
    pub auto_sync_interval_mins: u32,
    #[serde(default)]
    pub snooze_until: Option<String>,
}

impl Default for AppSettings {
    fn default() -> Self {
        Self {
            autostart_enabled: true,
            auto_sync_enabled: true,
            auto_sync_interval_mins: 60,
            snooze_until: None,
        }
    }
}

#[derive(Debug, Clone)]
pub struct SettingsStore {
    path: PathBuf,
}

impl SettingsStore {
    pub fn new(path: PathBuf) -> Self {
        Self { path }
    }

    pub fn default_path() -> PathBuf {
        dirs::data_local_dir()
            .unwrap_or_else(|| PathBuf::from("."))
            .join("FinInsight")
            .join("TallyAgent")
            .join("settings.json")
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn load(&self) -> Result<AppSettings, String> {
        if !self.path.exists() {
            let default_settings = AppSettings::default();
            let _ = self.save(&default_settings);
            return Ok(default_settings);
        }

        let contents = fs::read_to_string(&self.path)
            .map_err(|e| format!("Failed to read settings: {e}"))?;

        match serde_json::from_str::<AppSettings>(&contents) {
            Ok(s) => Ok(s),
            Err(e) => {
                log::warn!("Corrupt settings.json at {:?}: {e}. Resetting to defaults.", self.path);
                let default_settings = AppSettings::default();
                let _ = self.save(&default_settings);
                Ok(default_settings)
            }
        }
    }

    pub fn save(&self, settings: &AppSettings) -> Result<(), String> {
        let json = serde_json::to_string_pretty(settings)
            .map_err(|e| format!("Failed to serialize settings: {e}"))?;

        if let Some(parent) = self.path.parent() {
            fs::create_dir_all(parent)
                .map_err(|e| format!("Failed to create settings directory: {e}"))?;
        }

        let file_stem = self.path.file_name().and_then(|n| n.to_str()).unwrap_or("settings");
        let temp_name = format!(".{}.tmp.{}", file_stem, std::process::id());
        let temp_path = self.path.with_file_name(temp_name);

        fs::write(&temp_path, &json)
            .map_err(|e| format!("Failed to write temporary settings file: {e}"))?;

        fs::rename(&temp_path, &self.path)
            .map_err(|e| format!("Failed to commit settings file: {e}"))?;

        Ok(())
    }

    /// Check if reminder snooze is active.
    pub fn is_snoozed(&self, settings: &AppSettings) -> bool {
        if let Some(ref until_str) = settings.snooze_until {
            if let Ok(until_dt) = DateTime::parse_from_rfc3339(until_str) {
                return Utc::now() < until_dt.with_timezone(&Utc);
            }
        }
        false
    }

    /// Snoozes the reminder for the specified number of minutes.
    pub fn snooze_for_minutes(&self, minutes: i64) -> Result<(), String> {
        let mut settings = self.load()?;
        let snooze_dt = Utc::now() + chrono::Duration::minutes(minutes);
        settings.snooze_until = Some(snooze_dt.to_rfc3339());
        self.save(&settings)
    }

    /// Clears any active snooze.
    pub fn clear_snooze(&self) -> Result<(), String> {
        let mut settings = self.load()?;
        if settings.snooze_until.is_some() {
            settings.snooze_until = None;
            self.save(&settings)?;
        }
        Ok(())
    }
}
