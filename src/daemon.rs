use crate::activity::ProcessInspector;
use crate::cache::Cache;
use crate::cleaner::{Cleaner, CommandRunner};
use crate::logging::Logger;
use crate::safety::{review_project, review_summary, CleanDecision, SafetyOptions};
use crate::scanner::Scanner;
use crate::store::{CleanEvent, ErrorRecord, SchedulerStatus, Store};
use anyhow::Result;
use serde_json::{Map, Value};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;
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
    pub target_quiet_period: Duration,
}

impl Default for DaemonOptions {
    fn default() -> Self {
        Self {
            clean_interval: Duration::from_secs(24 * 60 * 60),
            scan_interval: Duration::from_secs(7 * 24 * 60 * 60),
            target_quiet_period: Duration::from_secs(2 * 60 * 60),
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
    logger: Option<Logger>,
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
            logger: None,
        }
    }

    pub fn with_logger(mut self, logger: Logger) -> Self {
        self.logger = Some(logger);
        self
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
            target_quiet_period: self.opts.target_quiet_period,
            include_managed_cache: false,
            include_active: false,
            force: false,
        };
        self.run_cycle_with_safety(opts, &crate::activity::SysinfoProcessInspector)?;
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
            if review.decision == CleanDecision::Skipped(crate::safety::SkipReason::TargetReadError)
            {
                self.store.record_error(&ErrorRecord {
                    id: 0,
                    ts: SystemTime::now(),
                    category: "review".to_string(),
                    path: Some(review.target_path.to_string_lossy().into_owned()),
                    message: "target read error: unable to read direct target directory"
                        .to_string(),
                })?;
            }
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

        let summary = review_summary(&reviews);
        let skipped = summary.skipped_projects as i64 + cleaner_skipped;
        self.store.record_review_status(started, "run", &summary)?;
        self.store.finish_run(
            run_id,
            SystemTime::now(),
            projects_cleaned,
            bytes_recovered,
            errors_count,
        )?;
        let result = RunCycleResult {
            run_id,
            cleaned: projects_cleaned,
            skipped,
            bytes_recovered,
            errors: errors_count,
        };
        self.log_run_cycle(&result);
        Ok(result)
    }

    fn log_run_cycle(&self, result: &RunCycleResult) {
        let Some(logger) = &self.logger else {
            return;
        };

        let mut fields = Map::new();
        fields.insert("run_id".to_string(), Value::from(result.run_id));
        fields.insert("cleaned".to_string(), Value::from(result.cleaned));
        fields.insert("skipped".to_string(), Value::from(result.skipped));
        fields.insert(
            "bytes_recovered".to_string(),
            Value::from(result.bytes_recovered),
        );
        fields.insert("errors".to_string(), Value::from(result.errors));
        logger.info_fields("clean cycle complete", fields);
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
        let mut schedule = self.scheduler_status_or_initialize()?;
        while !shutdown.is_requested() {
            let next_due = if schedule.next_clean_at <= schedule.next_scan_at {
                schedule.next_clean_at
            } else {
                schedule.next_scan_at
            };
            if wait_until_or_shutdown(next_due, shutdown) {
                break;
            }

            let now = SystemTime::now();
            if now >= schedule.next_clean_at {
                self.run_cycle()?;
                schedule.next_clean_at = SystemTime::now() + self.opts.clean_interval;
            }
            if now >= schedule.next_scan_at {
                self.scan_cycle()?;
                schedule.next_scan_at = SystemTime::now() + self.opts.scan_interval;
            }
            self.store.record_scheduler_status(
                SystemTime::now(),
                schedule.next_clean_at,
                schedule.next_scan_at,
            )?;
        }
        Ok(())
    }

    fn scheduler_status_or_initialize(&self) -> Result<SchedulerStatus> {
        if let Some(status) = self.store.scheduler_status()? {
            return Ok(status);
        }

        let now = SystemTime::now();
        let next_clean_at = self
            .store
            .last_run()
            .ok()
            .and_then(|run| run.finished_at)
            .map(|finished_at| finished_at + self.opts.clean_interval)
            .unwrap_or(now + self.opts.clean_interval);
        let status = SchedulerStatus {
            updated_at: now,
            next_clean_at,
            next_scan_at: now + self.opts.scan_interval,
        };
        self.store.record_scheduler_status(
            status.updated_at,
            status.next_clean_at,
            status.next_scan_at,
        )?;
        Ok(status)
    }
}

fn wait_until_or_shutdown(deadline: SystemTime, shutdown: &ShutdownFlag) -> bool {
    loop {
        if shutdown.is_requested() {
            return true;
        }
        let Some(wait_for) = wall_clock_wait_chunk(deadline, SystemTime::now()) else {
            return false;
        };
        thread::sleep(wait_for);
    }
}

fn wall_clock_wait_chunk(deadline: SystemTime, now: SystemTime) -> Option<Duration> {
    let remaining = deadline.duration_since(now).ok()?;
    if remaining.is_zero() {
        None
    } else {
        Some(remaining.min(Duration::from_millis(250)))
    }
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wall_clock_wait_chunk_polls_until_deadline_is_reached() {
        let now = SystemTime::UNIX_EPOCH + Duration::from_secs(100);
        let deadline = now + Duration::from_secs(1);

        assert_eq!(
            wall_clock_wait_chunk(deadline, now),
            Some(Duration::from_millis(250))
        );
        assert_eq!(
            wall_clock_wait_chunk(deadline, now + Duration::from_millis(900)),
            Some(Duration::from_millis(100))
        );
        assert_eq!(wall_clock_wait_chunk(deadline, deadline), None);
        assert_eq!(
            wall_clock_wait_chunk(deadline, deadline + Duration::from_secs(1)),
            None
        );
    }
}
