use crate::activity::{ActivitySampler, ActivitySignal, ProcessInspector};
use crate::cache::Cache;
use crate::cleaner::{Cleaner, CommandRunner};
use crate::logging::Logger;
use crate::safety::{
    bind_review_to_observation, revalidate_before_clean, review_project_with_identity_provider,
    review_summary, CleanDecision, ObservationIdentityStatus, SafetyOptions, SkipReason,
};
use crate::scanner::{
    DiscoveryOriginKind as ScannerOriginKind, DiscoveryOriginResult, ScanErrorKind, Scanner,
    WorktreeDiscovery,
};
use crate::store::{
    CleanEvent, DiscoveryGeneration, DiscoveryOriginKind, ErrorRecord, GenerationReconciliation,
    ObservationReconciliation, OriginReconciliation, ProjectObservation, SchedulerStatus, Store,
};
use anyhow::Result;
use serde_json::{Map, Value};
use std::collections::BTreeSet;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread;
use std::time::{Duration, SystemTime};

static SHUTDOWN_REQUESTED: AtomicBool = AtomicBool::new(false);
const FORCED_SCAN_MIN_INTERVAL: Duration = Duration::from_secs(5 * 60);

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

pub trait Clock: Send + Sync {
    fn now(&self) -> SystemTime;

    fn wait_until_or_shutdown(&self, deadline: SystemTime, shutdown: &ShutdownFlag) -> bool {
        loop {
            if shutdown.is_requested() {
                return true;
            }
            let Some(wait_for) = wall_clock_wait_chunk(deadline, self.now()) else {
                return false;
            };
            thread::sleep(wait_for);
        }
    }
}

#[derive(Debug, Clone, Copy, Default)]
pub struct SystemClock;

impl Clock for SystemClock {
    fn now(&self) -> SystemTime {
        SystemTime::now()
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
            scan_interval: Duration::from_secs(24 * 60 * 60),
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
    pub coverage_incomplete: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScanCycleResult {
    pub errors: usize,
    pub generation: i64,
    pub policy_hash: String,
    pub origins: Vec<DiscoveryOriginResult>,
}

pub struct Daemon<'a, R: CommandRunner> {
    store: &'a Store,
    cache: Cache<'a>,
    scanner: Scanner,
    cleaner: Cleaner<R>,
    opts: DaemonOptions,
    logger: Option<Logger>,
    clock: Arc<dyn Clock>,
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
            clock: Arc::new(SystemClock),
        }
    }

    pub fn with_logger(mut self, logger: Logger) -> Self {
        self.logger = Some(logger);
        self
    }

    pub fn with_clock(mut self, clock: Arc<dyn Clock>) -> Self {
        self.clock = clock;
        self
    }

    pub fn reconcile_cached_state(&self) -> Result<Vec<PathBuf>> {
        self.cache
            .reconcile_for_review(|path| self.scanner.is_excluded(path))
    }

    pub fn scan_cycle(&self) -> Result<ScanCycleResult> {
        let now = self.clock.now();
        let report = self.scanner.scan_with_errors()?;
        let error_count = report.errors.len();
        for error in &report.errors {
            self.store.record_error(&ErrorRecord {
                id: 0,
                ts: now,
                category: match error.kind {
                    ScanErrorKind::Scan => "scan",
                    ScanErrorKind::WorktreeDiscovery => "worktree_discovery",
                }
                .to_string(),
                path: error.path.to_str().map(str::to_owned),
                message: error.message.clone(),
            })?;
        }
        self.reconcile_cached_state()?;
        for discovery in &report.worktree_discoveries {
            match discovery {
                WorktreeDiscovery::Success {
                    primary,
                    linked,
                    excluded,
                    out_of_scope,
                } => {
                    self.store.replace_linked_worktrees_with_reconciliation(
                        primary,
                        linked,
                        excluded,
                        out_of_scope,
                    )?;
                }
                WorktreeDiscovery::Failure { primary, message } => {
                    self.store
                        .mark_worktree_discovery_failed(primary, now, message)?;
                }
            }
        }
        let reconciliation = generation_reconciliation(&report, now);
        let generation = match self.store.reconcile_generation(now, &reconciliation) {
            Ok(generation) => generation,
            Err(error) => {
                let path = report
                    .origins
                    .iter()
                    .flat_map(|origin| &origin.projects)
                    .next()
                    .and_then(|project| project.path.to_str())
                    .map(str::to_owned);
                let _ = self.store.record_error(&ErrorRecord {
                    id: 0,
                    ts: now,
                    category: "cache".to_string(),
                    path,
                    message: error.to_string(),
                });
                return Err(error);
            }
        };
        Ok(ScanCycleResult {
            errors: error_count,
            generation: generation.id,
            policy_hash: generation.policy_hash,
            origins: report.origins,
        })
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
        self.reconcile_cached_state()?;
        let started = self.clock.now();
        let run_id = self.store.start_run(started)?;
        let (observations, generation) = self.authorized_observations()?;
        let generation_missing = generation.is_none();
        let project_paths: Vec<PathBuf> = observations
            .iter()
            .map(|observation| observation.project_path.clone())
            .collect();
        let scan_error_since = started
            .checked_sub(self.opts.scan_interval)
            .unwrap_or(SystemTime::UNIX_EPOCH);
        let scan_errors = self.store.scan_error_paths_since(scan_error_since)?;
        let scan_coverage_incomplete = self
            .store
            .scan_coverage_incomplete_since(scan_error_since)?;
        let discovery_blocks = self.store.blocked_worktree_discovery_paths()?;
        let coverage_incomplete =
            generation_missing || scan_coverage_incomplete || !discovery_blocks.is_empty();
        let mut activity_sampler = ActivitySampler::new(inspector);
        let activity =
            activity_signals(activity_sampler.active_projects_at(&project_paths, started)?);
        let mut reviews = Vec::with_capacity(observations.len());

        let mut projects_cleaned = 0;
        let mut cleaner_skipped = 0;
        let mut bytes_recovered = 0;
        let mut errors_count = 0;

        for observation in &observations {
            let path = observation.project_path.clone();
            let mut review = review_project_with_identity_provider(
                &path,
                &scan_errors,
                &discovery_blocks,
                &activity,
                started,
                &safety,
                self.scanner.identity_provider(),
            )?;

            if review.decision == CleanDecision::Cleanable {
                match self.scanner.policy() {
                    Some(policy) if !policy.contains_project(&path) => {
                        review.decision = CleanDecision::Skipped(SkipReason::OutOfScope);
                    }
                    Some(policy)
                        if policy.is_excluded(&path) || policy.is_excluded(&review.target_path) =>
                    {
                        review.decision = CleanDecision::Skipped(SkipReason::Excluded);
                    }
                    Some(_) => {}
                    None => {
                        review.decision = CleanDecision::Skipped(SkipReason::OutOfScope);
                    }
                }
            }

            let mut reverified_across_boot = false;
            if review.decision == CleanDecision::Cleanable {
                let observed_boot = observation
                    .boot_session_id
                    .as_ref()
                    .map(|boot| crate::identity::BootSessionId(boot.clone()));
                reverified_across_boot = bind_review_to_observation(
                    &mut review,
                    &observation.project_identity,
                    observation.target_identity.as_ref(),
                    observed_boot.as_ref(),
                ) == ObservationIdentityStatus::ReverifiedAcrossBoot;
            }
            if review.decision == CleanDecision::Cleanable {
                // This is the last validation boundary available before Cleaner
                // measures the target and spawns Cargo. The filesystem can still
                // change after this returns, so this narrows but cannot eliminate
                // the residual TOCTOU window.
                let revalidation_now = self.clock.now();
                review.decision = match self.scanner.policy() {
                    Some(policy) => revalidate_before_clean(
                        &review,
                        policy,
                        self.scanner.identity_provider(),
                        &activity_signals(
                            activity_sampler
                                .active_projects_at(&project_paths, revalidation_now)?,
                        ),
                        &scan_errors,
                        &discovery_blocks,
                        revalidation_now,
                        &safety,
                    )?,
                    None => CleanDecision::Skipped(SkipReason::OutOfScope),
                };
            }
            if review.decision == CleanDecision::Cleanable && reverified_across_boot {
                if let (Some(generation), Some(identity)) =
                    (generation.as_ref(), review.reviewed_identity.as_ref())
                {
                    self.store
                        .mark_observation_reverified(generation.id, &path, identity)?;
                }
            }

            if review.decision == CleanDecision::Skipped(crate::safety::SkipReason::TargetReadError)
            {
                self.store.record_error(&ErrorRecord {
                    id: 0,
                    ts: self.clock.now(),
                    category: "review".to_string(),
                    path: review.target_path.to_str().map(str::to_owned),
                    message: "target read error: unable to read direct target directory"
                        .to_string(),
                })?;
            }
            let should_clean = review.decision == CleanDecision::Cleanable;
            reviews.push(review);
            if !should_clean {
                continue;
            }

            match self.cleaner.clean(&path) {
                Ok(result) if result.skipped => {
                    cleaner_skipped += 1;
                }
                Ok(result) => {
                    let now = self.clock.now();
                    self.store.record_clean_event(&CleanEvent {
                        id: 0,
                        run_id,
                        ts: now,
                        path: path.to_string_lossy().into_owned(),
                        bytes_before: result.bytes_before,
                        bytes_after: result.bytes_after,
                        duration_ms: result.duration.as_millis() as i64,
                        exit_code: result.exit_code,
                        stderr_excerpt: result.stderr_excerpt.clone(),
                    })?;
                    let measurement_failed =
                        if let Some(measurement_error) = &result.measurement_error {
                            errors_count += 1;
                            self.store.record_error(&ErrorRecord {
                                id: 0,
                                ts: now,
                                category: "clean".to_string(),
                                path: path.to_str().map(str::to_owned),
                                message: measurement_error.clone(),
                            })?;
                            true
                        } else {
                            false
                        };
                    if result.exit_code == 0 && !measurement_failed {
                        projects_cleaned += 1;
                        bytes_recovered += (result.bytes_before - result.bytes_after).max(0);
                        self.store.mark_project_cleaned(&path, now)?;
                    } else if result.exit_code != 0 {
                        errors_count += 1;
                        let detail = if result.stderr_excerpt.is_empty() {
                            format!("cargo clean exited {}", result.exit_code)
                        } else {
                            format!(
                                "cargo clean exited {}: {}",
                                result.exit_code, result.stderr_excerpt
                            )
                        };
                        self.store.record_error(&ErrorRecord {
                            id: 0,
                            ts: now,
                            category: "clean".to_string(),
                            path: path.to_str().map(str::to_owned),
                            message: detail,
                        })?;
                    }
                }
                Err(err) => {
                    errors_count += 1;
                    self.store.record_error(&ErrorRecord {
                        id: 0,
                        ts: self.clock.now(),
                        category: "clean".to_string(),
                        path: path.to_str().map(str::to_owned),
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
            self.clock.now(),
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
            coverage_incomplete,
        };
        self.log_run_cycle(&result);
        Ok(result)
    }

    fn authorized_observations(
        &self,
    ) -> Result<(Vec<ProjectObservation>, Option<DiscoveryGeneration>)> {
        if self.scanner.policy().is_none() {
            return Ok((Vec::new(), None));
        }
        let Some(generation) = self.store.current_generation(self.scanner.policy_hash())? else {
            return Ok((Vec::new(), None));
        };
        let observations = self.store.authorized_observations(generation.id)?;
        Ok((observations, Some(generation)))
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
        fields.insert(
            "coverage_incomplete".to_string(),
            Value::from(result.coverage_incomplete),
        );
        logger.info_fields("clean cycle complete", fields);
    }

    pub fn run_forever(&self) -> Result<()> {
        let shutdown = ShutdownFlag::new();
        shutdown.install_signal_handlers()?;
        self.run_until_shutdown(&shutdown)
    }

    pub fn run_until_shutdown(&self, shutdown: &ShutdownFlag) -> Result<()> {
        let mut schedule = self.scheduler_status_or_initialize()?;
        self.schedule_missing_generation_scan(&mut schedule)?;
        if self.scanner.policy().is_some()
            && !self
                .store
                .has_matching_generation(self.scanner.policy_hash())?
            && self.clock.now() >= schedule.next_scan_at
        {
            if let Err(err) = self.scan_cycle() {
                self.defer_after_scan_failure(&mut schedule, &err)?;
            } else {
                self.store.clear_scan_retry_at()?;
                schedule.next_scan_at = self.clock.now() + self.opts.scan_interval;
                self.store.record_scheduler_status(
                    self.clock.now(),
                    schedule.next_clean_at,
                    schedule.next_scan_at,
                )?;
            }
        }
        while !shutdown.is_requested() {
            self.schedule_missing_generation_scan(&mut schedule)?;
            let next_due = if schedule.next_clean_at <= schedule.next_scan_at {
                schedule.next_clean_at
            } else {
                schedule.next_scan_at
            };
            if self.clock.wait_until_or_shutdown(next_due, shutdown) {
                break;
            }

            let now = self.clock.now();
            if now >= schedule.next_scan_at {
                if let Err(err) = self.scan_cycle() {
                    self.defer_after_scan_failure(&mut schedule, &err)?;
                    continue;
                }
                self.store.clear_scan_retry_at()?;
                schedule.next_scan_at = self.clock.now() + self.opts.scan_interval;
            }
            if now >= schedule.next_clean_at {
                self.run_cycle()?;
                schedule.next_clean_at = self.clock.now() + self.opts.clean_interval;
            }
            self.store.record_scheduler_status(
                self.clock.now(),
                schedule.next_clean_at,
                schedule.next_scan_at,
            )?;
        }
        Ok(())
    }

    fn schedule_missing_generation_scan(&self, schedule: &mut SchedulerStatus) -> Result<()> {
        if self.scanner.policy().is_none()
            || self
                .store
                .has_matching_generation(self.scanner.policy_hash())?
        {
            return Ok(());
        }

        let now = self.clock.now();
        if let Some(retry_at) = self.store.scan_retry_at()? {
            if retry_at > now {
                if schedule.next_scan_at != retry_at {
                    schedule.next_scan_at = retry_at;
                    self.store.record_scheduler_status(
                        now,
                        schedule.next_clean_at,
                        schedule.next_scan_at,
                    )?;
                }
                return Ok(());
            }
            self.store.clear_scan_retry_at()?;
        }
        let last_attempt = self.store.last_forced_scan_at()?;
        let rate_limit_elapsed = last_attempt.is_none_or(|last| {
            now.duration_since(last)
                .is_ok_and(|elapsed| elapsed >= FORCED_SCAN_MIN_INTERVAL)
        });
        let next_scan_at = if rate_limit_elapsed {
            self.store.record_forced_scan_at(now)?;
            now
        } else {
            last_attempt
                .and_then(|last| last.checked_add(FORCED_SCAN_MIN_INTERVAL))
                .unwrap_or_else(|| now.checked_add(FORCED_SCAN_MIN_INTERVAL).unwrap_or(now))
        };

        if schedule.next_scan_at != next_scan_at {
            schedule.next_scan_at = next_scan_at;
            self.store.record_scheduler_status(
                now,
                schedule.next_clean_at,
                schedule.next_scan_at,
            )?;
        }
        Ok(())
    }

    fn defer_after_scan_failure(
        &self,
        schedule: &mut SchedulerStatus,
        err: &anyhow::Error,
    ) -> Result<()> {
        let retry_delay = self.opts.scan_interval.max(Duration::from_secs(1));
        let now = self.clock.now();
        let mut retry_at = now + retry_delay;
        if !self
            .store
            .has_matching_generation(self.scanner.policy_hash())?
        {
            if let Some(forced_at) = self.store.last_forced_scan_at()? {
                retry_at = retry_at.max(
                    forced_at
                        .checked_add(FORCED_SCAN_MIN_INTERVAL)
                        .unwrap_or(forced_at),
                );
            }
        }
        schedule.next_scan_at = retry_at;
        schedule.next_clean_at = schedule.next_clean_at.max(retry_at);
        self.store.record_scan_retry_at(retry_at)?;
        self.store
            .record_scheduler_status(now, schedule.next_clean_at, schedule.next_scan_at)?;
        if let Some(logger) = &self.logger {
            logger.error(format!("scan cycle failed; retry scheduled: {err}"));
        }
        Ok(())
    }

    fn scheduler_status_or_initialize(&self) -> Result<SchedulerStatus> {
        if let Some(mut status) = self.store.scheduler_status()? {
            let next_scan_at = clamp_next_scan_at(
                status.next_scan_at,
                self.clock.now(),
                self.opts.scan_interval,
            );
            if next_scan_at != status.next_scan_at {
                status.next_scan_at = next_scan_at;
                self.store.record_scheduler_status(
                    status.updated_at,
                    status.next_clean_at,
                    status.next_scan_at,
                )?;
            }
            return Ok(status);
        }

        let now = self.clock.now();
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

fn activity_signals(active: &BTreeSet<PathBuf>) -> Vec<ActivitySignal> {
    active
        .iter()
        .cloned()
        .map(|project_path| ActivitySignal {
            pid: 0,
            project_path,
            reason: "bounded activity sample".to_string(),
        })
        .collect()
}

fn generation_reconciliation(
    report: &crate::scanner::ScanReport,
    observed_at: SystemTime,
) -> GenerationReconciliation {
    let mut authorized_projects = BTreeSet::new();
    let origins = report
        .origins
        .iter()
        .map(|origin| {
            let observations = origin
                .projects
                .iter()
                .map(|project| {
                    let authorized =
                        origin.completed && authorized_projects.insert(project.path.clone());
                    ObservationReconciliation {
                        project_path: project.path.clone(),
                        project_identity: project.project_identity.clone(),
                        target_identity: project.target_identity.clone(),
                        observed_at,
                        authorized,
                        blocked_reason: (!authorized && origin.completed)
                            .then(|| "duplicate observation from overlapping origin".to_string()),
                    }
                })
                .collect();
            OriginReconciliation {
                kind: match origin.kind {
                    ScannerOriginKind::ScanRoot => DiscoveryOriginKind::ScanRoot,
                    ScannerOriginKind::ExplicitProject => DiscoveryOriginKind::ExplicitProject,
                },
                configured_path: origin.configured_path.clone(),
                canonical_path: origin.canonical_path.clone(),
                completed: origin.completed,
                error: origin.error.clone(),
                observations,
            }
        })
        .collect();
    GenerationReconciliation {
        policy_hash: report.policy_hash.clone(),
        boot_session_id: report
            .boot_session_id
            .as_ref()
            .map(|boot_session| boot_session.0.clone()),
        origins,
    }
}

pub fn clamp_next_scan_at(
    persisted: SystemTime,
    now: SystemTime,
    interval: Duration,
) -> SystemTime {
    persisted.min(now + interval)
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
