//! Minimal logger: timestamped lines to a log file (and stderr in debug
//! builds). The file is truncated at startup, so it never grows unbounded.
//! Level comes from `PERK_LOG` (error, warn, info, debug, trace; default info).

use std::fs::{self, File};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

use log::{LevelFilter, Log, Metadata, Record};

struct Logger {
    file: Option<Mutex<File>>,
    level: LevelFilter,
}

impl Log for Logger {
    fn enabled(&self, metadata: &Metadata) -> bool {
        metadata.level() <= self.level
    }

    fn log(&self, record: &Record) {
        if !self.enabled(record.metadata()) {
            return;
        }
        let secs = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        let (h, m, s) = ((secs / 3600) % 24, (secs / 60) % 60, secs % 60);
        let line = format!(
            "{h:02}:{m:02}:{s:02}Z {:<5} {}: {}\n",
            record.level(),
            record.target(),
            record.args()
        );
        if let Some(file) = &self.file {
            if let Ok(mut f) = file.lock() {
                let _ = f.write_all(line.as_bytes());
            }
        }
        if cfg!(debug_assertions) || self.file.is_none() {
            eprint!("{line}");
        }
    }

    fn flush(&self) {
        if let Some(Ok(mut f)) = self.file.as_ref().map(|f| f.lock()) {
            let _ = f.flush();
        }
    }
}

pub fn default_path() -> Option<PathBuf> {
    dirs::state_dir()
        .or_else(dirs::data_local_dir)
        .map(|d| d.join("perk").join("perk.log"))
}

pub fn init(path: Option<&Path>) {
    let level = std::env::var("PERK_LOG")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(LevelFilter::Info);
    let file = path.and_then(|p| {
        p.parent().map(fs::create_dir_all);
        File::create(p).ok().map(Mutex::new)
    });
    let logger = Box::new(Logger { file, level });
    if log::set_boxed_logger(logger).is_ok() {
        log::set_max_level(level);
    }
}
