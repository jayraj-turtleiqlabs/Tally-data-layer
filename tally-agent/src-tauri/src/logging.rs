//! Redacted rotating file logger.
//!
//! Dual-dispatch architecture:
//! - Full detailed diagnostic logs (Trace, Debug, Info, Warn, Error) are written to `%LOCALAPPDATA%\FinInsight\TallyAgent\logs\tally-agent.log`.
//! - Clean, important lifecycle info and errors (Info, Warn, Error) are printed to terminal.
//! - Routine/continuous debug logs (heartbeats, CompanyInfo pings, raw XML responses, poll ticks) go to file only.

use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::PathBuf;
use std::sync::Mutex;

use chrono::Local;
use log::{Level, LevelFilter, Metadata, Record};

use crate::redact::redact;

const MAX_LOG_BYTES: u64 = 5 * 1024 * 1024; // 5 MB
const MAX_BACKUP_FILES: usize = 3;

pub struct AgentLogger {
    log_path: Option<PathBuf>,
    file_mutex: Mutex<()>,
}

impl AgentLogger {
    pub fn new(log_path: Option<PathBuf>) -> Self {
        Self {
            log_path,
            file_mutex: Mutex::new(()),
        }
    }

    fn write_to_file(&self, path: &PathBuf, formatted: &str) {
        if let Ok(_guard) = self.file_mutex.lock() {
            Self::rotate_if_needed(path);
            if let Ok(mut file) = OpenOptions::new().create(true).append(true).open(path) {
                let _ = writeln!(file, "{}", formatted);
            }
        }
    }

    fn rotate_if_needed(path: &PathBuf) {
        if let Ok(meta) = fs::metadata(path) {
            if meta.len() >= MAX_LOG_BYTES {
                for i in (1..MAX_BACKUP_FILES).rev() {
                    let src = path.with_extension(format!("log.{}", i));
                    let dst = path.with_extension(format!("log.{}", i + 1));
                    if src.exists() {
                        let _ = fs::rename(&src, &dst);
                    }
                }
                let backup_1 = path.with_extension("log.1");
                let _ = fs::rename(path, backup_1);
            }
        }
    }
}

impl log::Log for AgentLogger {
    fn enabled(&self, _metadata: &Metadata) -> bool {
        true
    }

    fn log(&self, record: &Record) {
        if !self.enabled(record.metadata()) {
            return;
        }

        let now = Local::now().format("%Y-%m-%d %H:%M:%S").to_string();
        let raw_msg = format!("{}", record.args());
        let redacted_msg = redact(&raw_msg);
        let level = record.level();
        let target = record.target();

        // 1. Full diagnostic format for log file (all levels)
        let file_formatted = format!("{} [{}] {}: {}", now, level, target, redacted_msg);
        if let Some(ref path) = self.log_path {
            self.write_to_file(path, &file_formatted);
        }

        // 2. Terminal output: important lifecycle and errors only
        // DEBUG and TRACE remain in log file only, preventing terminal flooding
        match level {
            Level::Error => {
                eprintln!("{} [ERROR] {}", now, redacted_msg);
            }
            Level::Warn => {
                eprintln!("{} [WARN] {}", now, redacted_msg);
            }
            Level::Info => {
                println!("{} [INFO] {}", now, redacted_msg);
            }
            Level::Debug | Level::Trace => {
                // Written to file only
            }
        }
    }

    fn flush(&self) {}
}

pub fn default_log_path() -> Option<PathBuf> {
    dirs::data_local_dir().map(|d| {
        d.join("FinInsight")
            .join("TallyAgent")
            .join("logs")
            .join("tally-agent.log")
    })
}

pub fn init_logging() -> Result<(), Box<dyn std::error::Error>> {
    let log_path = default_log_path();
    if let Some(ref path) = log_path {
        if let Some(parent) = path.parent() {
            let _ = fs::create_dir_all(parent);
        }
    }

    let logger = AgentLogger::new(log_path);
    let _ = log::set_boxed_logger(Box::new(logger));
    log::set_max_level(LevelFilter::Debug);

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn test_default_log_path_ends_with_tally_agent_log() {
        let path = default_log_path().expect("default log path");
        assert!(path.ends_with("tally-agent.log"));
    }

    #[test]
    fn test_logger_writes_to_file() {
        let dir = tempdir().unwrap();
        let log_file = dir.path().join("tally-agent.log");
        let logger = AgentLogger::new(Some(log_file.clone()));

        logger.write_to_file(&log_file, "2026-09-07 15:00:00 [INFO] test: Agent started");
        assert!(log_file.exists());
        let contents = fs::read_to_string(&log_file).unwrap();
        assert!(contents.contains("[INFO] test: Agent started"));
    }

    #[test]
    fn test_logger_rotation_preserves_backups() {
        let dir = tempdir().unwrap();
        let log_file = dir.path().join("tally-agent.log");
        let logger = AgentLogger::new(Some(log_file.clone()));

        // Write a file that exceeds MAX_LOG_BYTES
        let large_chunk = vec![b'a'; (MAX_LOG_BYTES + 10) as usize];
        fs::write(&log_file, large_chunk).unwrap();

        // Writing again should trigger rotation to .1
        logger.write_to_file(&log_file, "New line after rotation");

        let backup = dir.path().join("tally-agent.log.1");
        assert!(backup.exists(), "Backup file .1 must exist after rotation");
        let new_contents = fs::read_to_string(&log_file).unwrap();
        assert!(new_contents.contains("New line after rotation"));
    }
}
