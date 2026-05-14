use anyhow::Result;
use serde::Serialize;
use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

#[derive(Debug, Clone, Copy)]
pub struct LoggerOptions {
    pub max_bytes: u64,
    pub max_files: usize,
}

impl Default for LoggerOptions {
    fn default() -> Self {
        Self {
            max_bytes: 10 * 1024 * 1024,
            max_files: 5,
        }
    }
}

#[derive(Clone)]
pub struct Logger {
    path: Arc<PathBuf>,
    options: LoggerOptions,
    file: Arc<Mutex<Option<File>>>,
}

#[derive(Serialize)]
struct LogEvent<'a> {
    ts: u64,
    level: &'a str,
    message: &'a str,
}

impl Logger {
    pub fn new(path: impl AsRef<Path>) -> Result<Self> {
        Self::with_options(path, LoggerOptions::default())
    }

    pub fn with_options(path: impl AsRef<Path>, options: LoggerOptions) -> Result<Self> {
        let path = path.as_ref();
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        let file = OpenOptions::new().create(true).append(true).open(path)?;
        Ok(Self {
            path: Arc::new(path.to_path_buf()),
            options,
            file: Arc::new(Mutex::new(Some(file))),
        })
    }

    pub fn info(&self, message: impl AsRef<str>) {
        self.write("INFO", message.as_ref());
    }

    pub fn error(&self, message: impl AsRef<str>) {
        self.write("ERROR", message.as_ref());
    }

    fn write(&self, level: &str, message: &str) {
        let ts = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or_default();
        let event = LogEvent { ts, level, message };
        let Ok(mut line) = serde_json::to_string(&event) else {
            return;
        };
        line.push('\n');

        if let Ok(mut file) = self.file.lock() {
            if self.should_rotate(file.as_ref(), line.len() as u64) {
                let _ = file.take();
                let _ = self.rotate_files();
                if let Ok(reopened) = OpenOptions::new()
                    .create(true)
                    .append(true)
                    .open(self.path.as_path())
                {
                    *file = Some(reopened);
                }
            }
            if let Some(file) = file.as_mut() {
                let _ = file.write_all(line.as_bytes());
                let _ = file.flush();
            }
        }
    }

    fn should_rotate(&self, file: Option<&File>, pending_bytes: u64) -> bool {
        if self.options.max_bytes == 0 || self.options.max_files < 2 {
            return false;
        }
        file.and_then(|file| file.metadata().ok())
            .map(|meta| meta.len() + pending_bytes > self.options.max_bytes)
            .unwrap_or(false)
    }

    fn rotate_files(&self) -> Result<()> {
        for idx in (1..self.options.max_files).rev() {
            let from = if idx == 1 {
                self.path.as_ref().clone()
            } else {
                rotated_path(self.path.as_path(), idx - 1)
            };
            let to = rotated_path(self.path.as_path(), idx);
            if from.exists() {
                if to.exists() {
                    fs::remove_file(&to)?;
                }
                fs::rename(from, to)?;
            }
        }
        Ok(())
    }
}

fn rotated_path(path: &Path, idx: usize) -> PathBuf {
    let Some(extension) = path.extension() else {
        return path.with_file_name(format!(
            "{}.{idx}",
            path.file_name()
                .and_then(|name| name.to_str())
                .unwrap_or("car-go-clean.log")
        ));
    };
    let mut extension = extension.to_os_string();
    extension.push(format!(".{idx}"));
    path.with_extension(extension)
}
