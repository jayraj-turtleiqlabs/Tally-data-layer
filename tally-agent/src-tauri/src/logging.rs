//! Redacted rotating file logger.

use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::PathBuf;
use std::sync::Mutex;

use once_cell::sync::Lazy;

use crate::redact::redact;

static LOG_FILE: Lazy<Mutex<Option<PathBuf>>> = Lazy::new(|| Mutex::new(None));

const MAX_LOG_BYTES: u64 = 5 * 1024 * 1024; // 5 MB

struct RotatingWriter {
    path: PathBuf,
}

impl Write for RotatingWriter {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        rotate_if_needed(&self.path);
        let mut file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.path)?;
        file.write(buf)
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

pub fn init_logging() -> Result<(), Box<dyn std::error::Error>> {
    let log_dir = dirs::data_local_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("FinInsight")
        .join("TallyAgent")
        .join("logs");
    fs::create_dir_all(&log_dir)?;
    let log_path = log_dir.join("agent.log");
    *LOG_FILE.lock().unwrap() = Some(log_path.clone());

    env_logger::Builder::from_default_env()
        .format(|buf, record| {
            let msg = redact(&format!("{}", record.args()));
            writeln!(buf, "[{} {}] {}", record.level(), record.target(), msg)
        })
        .target(env_logger::Target::Pipe(Box::new(RotatingWriter {
            path: log_path,
        })))
        .init();

    Ok(())
}

pub fn write_redacted(level: &str, message: &str) {
    let redacted = redact(message);
    log::log!(target: "agent", log::Level::Info, "[{}] {}", level, redacted);

    if let Some(path) = LOG_FILE.lock().unwrap().as_ref() {
        rotate_if_needed(path);
        if let Ok(mut file) = OpenOptions::new().create(true).append(true).open(path) {
            let _ = writeln!(file, "[{}] {}", level, redacted);
        }
    }
}

fn rotate_if_needed(path: &PathBuf) {
    if let Ok(meta) = fs::metadata(path) {
        if meta.len() > MAX_LOG_BYTES {
            let backup = path.with_extension("log.1");
            let _ = fs::rename(path, backup);
        }
    }
}
