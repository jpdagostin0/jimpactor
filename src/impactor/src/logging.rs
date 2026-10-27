//! Logger that keeps recent records in memory for the in-app log viewer, writes them to a
//! daily log file, and still prints to stderr through `env_logger` (honoring `RUST_LOG`).

use std::collections::VecDeque;
use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::path::PathBuf;
use std::sync::{Mutex, OnceLock};

use log::{Level, LevelFilter, Log, Metadata, Record};

const MAX_ENTRIES: usize = 2000;
const MAX_LOG_FILES: usize = 3;
// Crates whose debug output is useful; everything else (iced, wgpu, ...) only from warn up.
const DEBUG_TARGETS: &[&str] = &["plume", "omnisette", "idevice", "panic"];

#[derive(Debug, Clone)]
pub(crate) struct LogEntry {
    pub time: chrono::DateTime<chrono::Local>,
    pub level: Level,
    pub target: String,
    pub message: String,
}

impl std::fmt::Display for LogEntry {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{} {:<5} [{}] {}",
            self.time.format("%H:%M:%S"),
            self.level,
            self.target,
            self.message
        )
    }
}

struct AppLogger {
    stderr: env_logger::Logger,
    entries: Mutex<VecDeque<LogEntry>>,
    file: Mutex<Option<File>>,
}

static LOGGER: OnceLock<AppLogger> = OnceLock::new();

pub(crate) fn log_dir() -> PathBuf {
    crate::defaults::get_data_path().join("logs")
}

fn captured(metadata: &Metadata) -> bool {
    metadata.level() <= Level::Warn
        || DEBUG_TARGETS
            .iter()
            .any(|t| metadata.target().starts_with(t))
}

impl Log for AppLogger {
    fn enabled(&self, metadata: &Metadata) -> bool {
        captured(metadata) || self.stderr.enabled(metadata)
    }

    fn log(&self, record: &Record) {
        if self.stderr.enabled(record.metadata()) {
            self.stderr.log(record);
        }
        if !captured(record.metadata()) || record.level() == Level::Trace {
            return;
        }

        let entry = LogEntry {
            time: chrono::Local::now(),
            level: record.level(),
            target: record.target().to_string(),
            message: record.args().to_string(),
        };

        if let Ok(mut file) = self.file.lock() {
            if let Some(file) = file.as_mut() {
                let _ = writeln!(file, "{} {}", entry.time.format("%Y-%m-%d"), entry);
            }
        }

        if let Ok(mut entries) = self.entries.lock() {
            if entries.len() == MAX_ENTRIES {
                entries.pop_front();
            }
            entries.push_back(entry);
        }
    }

    fn flush(&self) {
        if let Ok(mut file) = self.file.lock() {
            if let Some(file) = file.as_mut() {
                let _ = file.flush();
            }
        }
    }
}

fn open_log_file() -> Option<File> {
    let dir = log_dir();
    fs::create_dir_all(&dir).ok()?;

    // Keep only the most recent files; names sort by date.
    if let Ok(read_dir) = fs::read_dir(&dir) {
        let mut files: Vec<_> = read_dir
            .filter_map(|e| e.ok().map(|e| e.path()))
            .filter(|p| p.extension().is_some_and(|ext| ext == "log"))
            .collect();
        files.sort();
        let excess = (files.len() + 1).saturating_sub(MAX_LOG_FILES);
        for old in files.into_iter().take(excess) {
            let _ = fs::remove_file(old);
        }
    }

    let name = format!("jimpactor-{}.log", chrono::Local::now().format("%Y-%m-%d"));
    OpenOptions::new()
        .create(true)
        .append(true)
        .open(dir.join(name))
        .ok()
}

pub(crate) fn init() {
    let logger = LOGGER.get_or_init(|| AppLogger {
        stderr: env_logger::Builder::from_default_env().build(),
        entries: Mutex::new(VecDeque::with_capacity(MAX_ENTRIES)),
        file: Mutex::new(open_log_file()),
    });

    if log::set_logger(logger).is_ok() {
        log::set_max_level(LevelFilter::Debug.max(logger.stderr.filter()));
    }

    let default_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        log::error!(
            target: "panic",
            "{info}\n{}",
            std::backtrace::Backtrace::force_capture()
        );
        if let Some(logger) = LOGGER.get() {
            logger.flush();
        }
        default_hook(info);
    }));
}

/// Recent entries at `level` or more severe, oldest first.
pub(crate) fn entries(level: LevelFilter) -> Vec<LogEntry> {
    LOGGER
        .get()
        .and_then(|l| l.entries.lock().ok())
        .map(|entries| {
            entries
                .iter()
                .filter(|e| e.level <= level)
                .cloned()
                .collect()
        })
        .unwrap_or_default()
}

pub(crate) fn clear() {
    if let Some(mut entries) = LOGGER.get().and_then(|l| l.entries.lock().ok()) {
        entries.clear();
    }
}
