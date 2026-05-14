use crate::activity::ProcessInspector;
use crate::cache::Cache;
use crate::cleaner::{Cleaner, CommandRunner};
use crate::safety::{review_project, review_summary, CleanDecision, SafetyOptions};
use crate::scanner::Scanner;
use crate::store::{CleanEvent, ErrorRecord, Store};
use anyhow::Result;
use std::path::PathBuf;
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

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunCycleResult {
    pub run_id: i64,
    pub cleaned: i64,
    pub skipped: i64,
    pub bytes_recovered: i64,
    pub errors: i64,
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
        let report = self.scanner.scan_with_errors()?;
        for error in report.errors {
            self.store.record_error(&ErrorRecord {
                id: 0,
                ts: now,
                category: "scan".to_string(),
                path: Some(error.path.to_string_lossy().into_owned()),
                message: error.message,
            })?;
        }
        for path in report.projects {
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
        let opts = SafetyOptions {
            target_quiet_period: Duration::ZERO,
            include_managed_cache: true,
            include_active: true,
            force: true,
        };
        self.run_cycle_with_safety(opts, &crate::activity::NoopProcessInspector)?;
        Ok(())
    }

    pub fn run_cycle_with_safety(
        &self,
        safety: SafetyOptions,
        inspector: &impl ProcessInspector,
    ) -> Result<RunCycleResult> {
        self.cache.sync_on_disk()?;
        let started = SystemTime::now();
        let run_id = self.store.start_run(started)?;
        let projects = self.store.all_projects()?;
        let project_paths: Vec<PathBuf> = projects
            .iter()
            .map(|project| PathBuf::from(&project.path))
            .collect();
        let scan_error_since = started
            .checked_sub(self.opts.scan_interval)
            .unwrap_or(SystemTime::UNIX_EPOCH);
        let scan_errors = self.store.scan_error_paths_since(scan_error_since)?;
        let activity = inspector.active_projects(&project_paths)?;
        let mut reviews = Vec::with_capacity(projects.len());

        let mut projects_cleaned = 0;
        let mut cleaner_skipped = 0;
        let mut bytes_recovered = 0;
        let mut errors_count = 0;

        for project in &projects {
            let path = PathBuf::from(&project.path);
            let review = review_project(&path, &scan_errors, &activity, started, &safety)?;
            let should_clean = review.decision == CleanDecision::Cleanable;
            reviews.push(review);
            if !should_clean {
                continue;
            }

            match self.cleaner.clean(&project.path) {
                Ok(result) if result.skipped => {
                    cleaner_skipped += 1;
                }
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

        let skipped = review_summary(&reviews).skipped_projects as i64 + cleaner_skipped;
        self.store.finish_run(
            run_id,
            SystemTime::now(),
            projects_cleaned,
            bytes_recovered,
            errors_count,
        )?;
        Ok(RunCycleResult {
            run_id,
            cleaned: projects_cleaned,
            skipped,
            bytes_recovered,
            errors: errors_count,
        })
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
