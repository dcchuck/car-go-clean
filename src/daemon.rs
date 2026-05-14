use crate::cache::Cache;
use crate::cleaner::{Cleaner, CommandRunner};
use crate::scanner::Scanner;
use crate::store::{CleanEvent, ErrorRecord, Store};
use anyhow::Result;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;
use std::time::Instant;
use std::time::{Duration, SystemTime};

static SHUTDOWN_REQUESTED: AtomicBool = AtomicBool::new(false);

#[derive(Debug, Clone, Copy)]
pub struct ShutdownFlag;

impl ShutdownFlag {
    pub fn new() -> Self {
        SHUTDOWN_REQUESTED.store(false, Ordering::SeqCst);
        Self
    }

    pub fn request(&self) {
        SHUTDOWN_REQUESTED.store(true, Ordering::SeqCst);
    }

    pub fn is_requested(&self) -> bool {
        SHUTDOWN_REQUESTED.load(Ordering::SeqCst)
    }

    pub fn install_signal_handlers(&self) -> Result<()> {
        install_signal_handlers()
    }
}

impl Default for ShutdownFlag {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Debug, Clone, Copy)]
pub struct DaemonOptions {
    pub clean_interval: Duration,
    pub scan_interval: Duration,
}

impl Default for DaemonOptions {
    fn default() -> Self {
        Self {
            clean_interval: Duration::from_secs(24 * 60 * 60),
            scan_interval: Duration::from_secs(7 * 24 * 60 * 60),
        }
    }
}

pub struct Daemon<'a, R: CommandRunner> {
    store: &'a Store,
    cache: Cache<'a>,
    scanner: Scanner,
    cleaner: Cleaner<R>,
    opts: DaemonOptions,
}

impl<'a, R: CommandRunner> Daemon<'a, R> {
    pub fn new(
        store: &'a Store,
        cache: Cache<'a>,
        scanner: Scanner,
        cleaner: Cleaner<R>,
        opts: DaemonOptions,
    ) -> Self {
        Self {
            store,
            cache,
            scanner,
            cleaner,
            opts,
        }
    }

    pub fn scan_cycle(&self) -> Result<()> {
        let now = SystemTime::now();
        for path in self.scanner.scan()? {
            if let Err(err) = self.store.upsert_project(&path, now) {
                self.store.record_error(&ErrorRecord {
                    id: 0,
                    ts: now,
                    category: "cache".to_string(),
                    path: Some(path.to_string_lossy().into_owned()),
                    message: err.to_string(),
                })?;
            }
        }
        Ok(())
    }

    pub fn run_cycle(&self) -> Result<()> {
        self.cache.sync_on_disk()?;
        let started = SystemTime::now();
        let run_id = self.store.start_run(started)?;
        let projects = self.store.all_projects()?;

        let mut projects_cleaned = 0;
        let mut bytes_recovered = 0;
        let mut errors_count = 0;

        for project in projects {
            match self.cleaner.clean(&project.path) {
                Ok(result) if result.skipped => {}
                Ok(result) => {
                    projects_cleaned += 1;
                    bytes_recovered += (result.bytes_before - result.bytes_after).max(0);
                    let now = SystemTime::now();
                    self.store.record_clean_event(&CleanEvent {
                        id: 0,
                        run_id,
                        ts: now,
                        path: project.path.clone(),
                        bytes_before: result.bytes_before,
                        bytes_after: result.bytes_after,
                        duration_ms: result.duration.as_millis() as i64,
                        exit_code: result.exit_code,
                        stderr_excerpt: result.stderr_excerpt,
                    })?;
                    self.store.mark_project_cleaned(&project.path, now)?;
                }
                Err(err) => {
                    errors_count += 1;
                    self.store.record_error(&ErrorRecord {
                        id: 0,
                        ts: SystemTime::now(),
                        category: "clean".to_string(),
                        path: Some(project.path.clone()),
                        message: err.to_string(),
                    })?;
                }
            }
        }

        self.store.finish_run(
            run_id,
            SystemTime::now(),
            projects_cleaned,
            bytes_recovered,
            errors_count,
        )?;
        Ok(())
    }

    pub fn run_forever(&self) -> Result<()> {
        let shutdown = ShutdownFlag::new();
        shutdown.install_signal_handlers()?;
        self.run_until_shutdown(&shutdown)
    }

    pub fn run_until_shutdown(&self, shutdown: &ShutdownFlag) -> Result<()> {
        if self.store.all_projects()?.is_empty() {
            self.scan_cycle()?;
        }
        let mut last_scan = SystemTime::now();
        while !shutdown.is_requested() {
            if wait_for_interval_or_shutdown(self.opts.clean_interval, shutdown) {
                break;
            }
            self.run_cycle()?;
            if last_scan.elapsed().unwrap_or_default() >= self.opts.scan_interval {
                self.scan_cycle()?;
                last_scan = SystemTime::now();
            }
        }
        Ok(())
    }
}

fn wait_for_interval_or_shutdown(interval: Duration, shutdown: &ShutdownFlag) -> bool {
    if interval.is_zero() {
        return shutdown.is_requested();
    }
    let started = Instant::now();
    while started.elapsed() < interval {
        if shutdown.is_requested() {
            return true;
        }
        let remaining = interval.saturating_sub(started.elapsed());
        thread::sleep(remaining.min(Duration::from_millis(250)));
    }
    shutdown.is_requested()
}

#[cfg(unix)]
fn install_signal_handlers() -> Result<()> {
    unsafe extern "C" fn handle_signal(_: libc::c_int) {
        SHUTDOWN_REQUESTED.store(true, Ordering::SeqCst);
    }

    unsafe {
        if libc::signal(
            libc::SIGINT,
            handle_signal as *const () as libc::sighandler_t,
        ) == libc::SIG_ERR
        {
            anyhow::bail!("install SIGINT handler");
        }
        if libc::signal(
            libc::SIGTERM,
            handle_signal as *const () as libc::sighandler_t,
        ) == libc::SIG_ERR
        {
            anyhow::bail!("install SIGTERM handler");
        }
    }
    Ok(())
}

#[cfg(not(unix))]
fn install_signal_handlers() -> Result<()> {
    Ok(())
}
