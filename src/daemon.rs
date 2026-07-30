use crate::activity::{ActivitySampler, ActivitySignal, ProcessInspector};
use crate::cache::Cache;
use crate::cleaner::{CleanAttemptOutcome, Cleaner, CommandRunner};
use crate::identity::{compare_persisted, IdentityComparison};
use crate::logging::Logger;
use crate::safety::{
    bind_review_to_observation, revalidate_before_clean, review_project_with_identity_provider,
    review_summary, CleanDecision, ObservationIdentityStatus, ProjectReview, SafetyOptions,
    SkipReason,
};
use crate::scanner::{
    DiscoveryOriginKind as ScannerOriginKind, DiscoveryOriginResult, ScanErrorKind, Scanner,
    WorktreeDiscovery,
};
use crate::store::{
    CleanEvent, DiscoveryGeneration, DiscoveryOriginKind, ErrorRecord, GenerationReconciliation,
    ObservationReconciliation, OriginReconciliation, ProjectObservation, ScanPublication,
    SchedulerStatus, Store, WorktreeReconciliation,
};
use anyhow::{Context, Result};
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

#[derive(Clone)]
pub struct DaemonCycleSnapshot {
    scanner: Scanner,
    options: DaemonOptions,
}

impl DaemonCycleSnapshot {
    pub fn new(scanner: Scanner, options: DaemonOptions) -> Self {
        Self { scanner, options }
    }

    pub fn scanner(&self) -> &Scanner {
        &self.scanner
    }

    pub fn options(&self) -> DaemonOptions {
        self.options
    }
}

pub trait DaemonCycleFactory: Send + Sync {
    fn snapshot(&self) -> Result<DaemonCycleSnapshot>;
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunCycleResult {
    pub run_id: i64,
    pub cleaned: i64,
    pub skipped: i64,
    pub bytes_recovered: i64,
    pub errors: i64,
    pub cargo_failures: i64,
    pub measurement_failures: i64,
    pub cleanup_failures: i64,
    pub coverage_incomplete: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScanCycleResult {
    pub errors: usize,
    pub generation: i64,
    pub policy_hash: String,
    pub origins: Vec<DiscoveryOriginResult>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RunSource {
    Dynamic,
    Reviewed,
}

impl RunSource {
    fn label(self) -> &'static str {
        match self {
            Self::Dynamic => "run",
            Self::Reviewed => "reviewed-run",
        }
    }
}

struct PreparedReview {
    review: ProjectReview,
    reverified_across_boot: bool,
}

type TargetReporter = dyn Fn(&ProjectReview) + Send + Sync;

fn enforce_current_policy(scanner: &Scanner, review: &mut ProjectReview) {
    if review.decision != CleanDecision::Cleanable {
        return;
    }
    match scanner.policy() {
        Some(policy) if !policy.contains_project(&review.path) => {
            review.decision = CleanDecision::Skipped(SkipReason::OutOfScope);
        }
        Some(policy)
            if policy.is_excluded(&review.path) || policy.is_excluded(&review.target_path) =>
        {
            review.decision = CleanDecision::Skipped(SkipReason::Excluded);
        }
        Some(_) => {}
        None => {
            review.decision = CleanDecision::Skipped(SkipReason::OutOfScope);
        }
    }
}

pub struct Daemon<'a, R: CommandRunner> {
    store: &'a Store,
    cache: Cache<'a>,
    scanner: Scanner,
    cleaner: Cleaner<R>,
    opts: DaemonOptions,
    logger: Option<Logger>,
    clock: Arc<dyn Clock>,
    cycle_factory: Option<Arc<dyn DaemonCycleFactory>>,
    target_reporter: Option<Arc<TargetReporter>>,
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
            cycle_factory: None,
            target_reporter: None,
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

    pub fn with_cycle_factory(mut self, factory: Arc<dyn DaemonCycleFactory>) -> Self {
        self.cycle_factory = Some(factory);
        self
    }

    pub fn with_target_reporter(
        mut self,
        reporter: impl Fn(&ProjectReview) + Send + Sync + 'static,
    ) -> Self {
        self.target_reporter = Some(Arc::new(reporter));
        self
    }

    fn cycle_snapshot(&self) -> Result<DaemonCycleSnapshot> {
        match &self.cycle_factory {
            Some(factory) => factory.snapshot(),
            None => Ok(DaemonCycleSnapshot::new(self.scanner.clone(), self.opts)),
        }
    }

    pub fn reconcile_cached_state(&self) -> Result<Vec<PathBuf>> {
        let snapshot = self.cycle_snapshot()?;
        self.reconcile_cached_state_with(snapshot.scanner())
    }

    fn reconcile_cached_state_with(&self, scanner: &Scanner) -> Result<Vec<PathBuf>> {
        self.cache
            .reconcile_for_review(|path| scanner.is_excluded(path))
    }

    pub fn scan_cycle(&self) -> Result<ScanCycleResult> {
        let snapshot = self.cycle_snapshot()?;
        self.scan_cycle_with_snapshot(&snapshot)
    }

    fn scan_cycle_with_snapshot(&self, snapshot: &DaemonCycleSnapshot) -> Result<ScanCycleResult> {
        let now = self.clock.now();
        let report = snapshot.scanner.scan_with_errors()?;
        let error_count = report.errors.len();
        let diagnostics = report
            .errors
            .iter()
            .map(|error| ErrorRecord {
                id: 0,
                ts: now,
                category: match error.kind {
                    ScanErrorKind::Scan => "scan",
                    ScanErrorKind::WorktreeDiscovery => "worktree_discovery",
                }
                .to_string(),
                path: error.path.to_str().map(str::to_owned),
                message: error.message.clone(),
            })
            .collect();
        let worktrees = report
            .worktree_discoveries
            .iter()
            .map(|discovery| match discovery {
                WorktreeDiscovery::Success {
                    primary,
                    linked,
                    excluded,
                    out_of_scope,
                } => WorktreeReconciliation::Success {
                    primary: primary.clone(),
                    linked: linked.clone(),
                    excluded: excluded.clone(),
                    out_of_scope: out_of_scope.clone(),
                },
                WorktreeDiscovery::Failure { primary, message } => {
                    WorktreeReconciliation::Failure {
                        primary: primary.clone(),
                        message: message.clone(),
                    }
                }
            })
            .collect();
        let generation = self.store.publish_scan(
            now,
            &ScanPublication {
                generation: generation_reconciliation(&report, now),
                worktrees,
                diagnostics,
            },
        )?;
        Ok(ScanCycleResult {
            errors: error_count,
            generation: generation.id,
            policy_hash: generation.policy_hash,
            origins: report.origins,
        })
    }

    pub fn run_cycle(&self) -> Result<()> {
        let snapshot = self.cycle_snapshot()?;
        let opts = SafetyOptions {
            target_quiet_period: snapshot.options.target_quiet_period,
            include_managed_cache: false,
            include_active: false,
            force: false,
        };
        self.run_cycle_with_snapshot(&snapshot, opts, &crate::activity::SysinfoProcessInspector)?;
        Ok(())
    }

    pub fn run_cycle_with_safety(
        &self,
        safety: SafetyOptions,
        inspector: &impl ProcessInspector,
    ) -> Result<RunCycleResult> {
        let snapshot = self.cycle_snapshot()?;
        self.run_cycle_with_snapshot(&snapshot, safety, inspector)
    }

    fn run_cycle_with_snapshot(
        &self,
        snapshot: &DaemonCycleSnapshot,
        safety: SafetyOptions,
        inspector: &impl ProcessInspector,
    ) -> Result<RunCycleResult> {
        self.reconcile_cached_state_with(&snapshot.scanner)?;
        let reviewed_at = self.clock.now();
        let (observations, generation) = self.authorized_observations(&snapshot.scanner)?;
        let generation_missing = generation.is_none();
        let project_paths: Vec<PathBuf> = observations
            .iter()
            .map(|observation| observation.project_path.clone())
            .collect();
        let scan_error_since = reviewed_at
            .checked_sub(snapshot.options.scan_interval)
            .unwrap_or(SystemTime::UNIX_EPOCH);
        let scan_errors = self.store.scan_error_paths_since(scan_error_since)?;
        let scan_coverage_incomplete = self
            .store
            .scan_coverage_incomplete_since(scan_error_since)?;
        let discovery_blocks = self.store.blocked_worktree_discovery_paths()?;
        let durable_generation_incomplete = self
            .store
            .current_generation_coverage_incomplete(snapshot.scanner.policy_hash())?;
        let coverage_incomplete = generation_missing
            || durable_generation_incomplete
            || scan_coverage_incomplete
            || !discovery_blocks.is_empty();
        let mut activity_sampler = ActivitySampler::new(inspector);
        let activity =
            activity_signals(activity_sampler.active_projects_at(&project_paths, reviewed_at)?);
        let mut reviews = Vec::with_capacity(observations.len());

        for observation in &observations {
            let path = observation.project_path.clone();
            let mut review = review_project_with_identity_provider(
                &path,
                &scan_errors,
                &discovery_blocks,
                &activity,
                reviewed_at,
                &safety,
                snapshot.scanner.identity_provider(),
            )?;

            enforce_current_policy(&snapshot.scanner, &mut review);

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
            reviews.push(PreparedReview {
                review,
                reverified_across_boot,
            });
        }

        self.execute_prepared_reviews(
            snapshot,
            reviews,
            generation.as_ref().map(|generation| generation.id),
            coverage_incomplete,
            safety,
            &mut activity_sampler,
            RunSource::Dynamic,
        )
    }

    pub fn execute_reviews_with_safety(
        &self,
        reviews: Vec<ProjectReview>,
        coverage_incomplete: bool,
        safety: SafetyOptions,
        inspector: &impl ProcessInspector,
        source: RunSource,
    ) -> Result<RunCycleResult> {
        let snapshot = self.cycle_snapshot()?;
        let persisted_reviews = reviews
            .into_iter()
            .filter(|review| review.decision == CleanDecision::Cleanable)
            .collect::<Vec<_>>();
        let project_paths = persisted_reviews
            .iter()
            .map(|review| review.path.clone())
            .collect::<Vec<_>>();
        let reviewed_at = self.clock.now();
        let scan_error_since = reviewed_at
            .checked_sub(snapshot.options.scan_interval)
            .unwrap_or(SystemTime::UNIX_EPOCH);
        let scan_errors = self.store.scan_error_paths_since(scan_error_since)?;
        let discovery_blocks = self.store.blocked_worktree_discovery_paths()?;
        let mut activity_sampler = ActivitySampler::new(inspector);
        let activity =
            activity_signals(activity_sampler.active_projects_at(&project_paths, reviewed_at)?);
        let reviews = persisted_reviews
            .into_iter()
            .map(|persisted| {
                let mut fresh = review_project_with_identity_provider(
                    &persisted.path,
                    &scan_errors,
                    &discovery_blocks,
                    &activity,
                    reviewed_at,
                    &safety,
                    snapshot.scanner.identity_provider(),
                )?;
                enforce_current_policy(&snapshot.scanner, &mut fresh);
                if fresh.decision == CleanDecision::Cleanable
                    && (fresh.path != persisted.path
                        || fresh.target_path != persisted.target_path
                        || fresh.canonical_path != persisted.canonical_path)
                {
                    fresh.decision = CleanDecision::Skipped(SkipReason::ProjectIdentityChanged);
                }
                if fresh.decision == CleanDecision::Cleanable {
                    match (
                        persisted.reviewed_identity.as_ref(),
                        fresh.reviewed_identity.as_ref(),
                    ) {
                        (Some(persisted_identity), Some(fresh_identity)) => {
                            if compare_persisted(
                                persisted_identity.boot_session.as_ref(),
                                fresh_identity.boot_session.as_ref(),
                                &persisted_identity.project,
                                &fresh_identity.project,
                            ) == IdentityComparison::Replaced
                            {
                                fresh.decision =
                                    CleanDecision::Skipped(SkipReason::ProjectIdentityChanged);
                            } else if compare_persisted(
                                persisted_identity.boot_session.as_ref(),
                                fresh_identity.boot_session.as_ref(),
                                &persisted_identity.target,
                                &fresh_identity.target,
                            ) == IdentityComparison::Replaced
                            {
                                fresh.decision =
                                    CleanDecision::Skipped(SkipReason::TargetIdentityChanged);
                            }
                        }
                        _ => {
                            fresh.decision =
                                CleanDecision::Skipped(SkipReason::ProjectIdentityUnavailable);
                        }
                    }
                }
                Ok(PreparedReview {
                    review: fresh,
                    reverified_across_boot: false,
                })
            })
            .collect::<Result<Vec<_>>>()?;
        self.execute_prepared_reviews(
            &snapshot,
            reviews,
            None,
            coverage_incomplete,
            safety,
            &mut activity_sampler,
            source,
        )
    }

    #[allow(clippy::too_many_arguments)]
    fn execute_prepared_reviews<I: ProcessInspector + ?Sized>(
        &self,
        snapshot: &DaemonCycleSnapshot,
        mut reviews: Vec<PreparedReview>,
        generation_id: Option<i64>,
        mut coverage_incomplete: bool,
        safety: SafetyOptions,
        activity_sampler: &mut ActivitySampler<'_, I>,
        source: RunSource,
    ) -> Result<RunCycleResult> {
        let started = self.clock.now();
        let run_id = self.store.start_run(started)?;
        let project_paths = reviews
            .iter()
            .map(|prepared| prepared.review.path.clone())
            .collect::<Vec<_>>();
        let scan_error_since = started
            .checked_sub(snapshot.options.scan_interval)
            .unwrap_or(SystemTime::UNIX_EPOCH);
        let scan_errors = self.store.scan_error_paths_since(scan_error_since)?;
        let scan_coverage_incomplete = self
            .store
            .scan_coverage_incomplete_since(scan_error_since)?;
        let discovery_blocks = self.store.blocked_worktree_discovery_paths()?;
        let durable_generation_incomplete = self
            .store
            .current_generation_coverage_incomplete(snapshot.scanner.policy_hash())?;
        coverage_incomplete = coverage_incomplete
            || durable_generation_incomplete
            || scan_coverage_incomplete
            || !discovery_blocks.is_empty();
        let mut projects_cleaned = 0;
        let mut cleaner_skipped = 0;
        let mut bytes_recovered = 0;
        let mut errors_count = 0;
        let mut cargo_failures = 0;
        let mut measurement_failures = 0;
        let mut cleanup_failures = 0;

        for prepared in &mut reviews {
            let review = &mut prepared.review;
            let path = review.path.clone();
            if review.decision == CleanDecision::Cleanable {
                // Reject stale authority before Cleaner measures or reports the
                // target. Cleaner invokes the same full validation again at its
                // final pre-spawn boundary.
                let revalidation_now = self.clock.now();
                review.decision = match snapshot.scanner.policy() {
                    Some(policy) => revalidate_before_clean(
                        review,
                        policy,
                        snapshot.scanner.identity_provider(),
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

            if review.decision == CleanDecision::Skipped(SkipReason::TargetReadError) {
                self.store.record_error(&ErrorRecord {
                    id: 0,
                    ts: self.clock.now(),
                    category: "review".to_string(),
                    path: review.target_path.to_str().map(str::to_owned),
                    message: "target read error: unable to read direct target directory"
                        .to_string(),
                })?;
            }
            if review.decision != CleanDecision::Cleanable {
                continue;
            }

            let reported_review = review.clone();
            match self
                .cleaner
                .clean_with_attempt_reporter_and_pre_spawn_validator(
                    &path,
                    |_, _| {
                        if let Some(reporter) = &self.target_reporter {
                            reporter(&reported_review);
                        }
                    },
                    |_, _| {
                        // This is the last validation boundary before CommandRunner.
                        // The filesystem can still change after this returns, so it
                        // narrows but cannot eliminate the residual TOCTOU window.
                        let revalidation_now = self.clock.now();
                        review.decision = match snapshot.scanner.policy() {
                            Some(policy) => revalidate_before_clean(
                                review,
                                policy,
                                snapshot.scanner.identity_provider(),
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
                        Ok(review.decision == CleanDecision::Cleanable)
                    },
                ) {
                Ok(result) if result.skipped => {
                    if review.decision == CleanDecision::Skipped(SkipReason::TargetReadError) {
                        self.store.record_error(&ErrorRecord {
                            id: 0,
                            ts: self.clock.now(),
                            category: "review".to_string(),
                            path: review.target_path.to_str().map(str::to_owned),
                            message: "target read error: unable to read direct target directory"
                                .to_string(),
                        })?;
                    }
                    if review.decision == CleanDecision::Cleanable {
                        cleaner_skipped += 1;
                    }
                }
                Ok(result) => {
                    let now = self.clock.now();
                    let outcome = result
                        .outcome
                        .context("non-skipped clean result is missing its attempt outcome")?;
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
                        outcome,
                        measurement_failed: result.measurement_error.is_some(),
                    })?;
                    if prepared.reverified_across_boot
                        && outcome != CleanAttemptOutcome::RunnerFailure
                    {
                        if let (Some(generation_id), Some(identity)) =
                            (generation_id, review.reviewed_identity.as_ref())
                        {
                            self.store.mark_observation_reverified(
                                generation_id,
                                &path,
                                identity,
                            )?;
                        }
                    }
                    match outcome {
                        CleanAttemptOutcome::Success => {
                            if result.measurement_error.is_none() {
                                projects_cleaned += 1;
                                bytes_recovered +=
                                    (result.bytes_before - result.bytes_after).max(0);
                                self.store.mark_project_cleaned(&path, now)?;
                            }
                        }
                        CleanAttemptOutcome::CargoNonzero => {
                            errors_count += 1;
                            cargo_failures += 1;
                            let exit_code = result
                                .exit_code
                                .context("nonzero Cargo attempt is missing its exit code")?;
                            let detail = if result.stderr_excerpt.is_empty() {
                                format!("cargo clean exited {exit_code}")
                            } else {
                                format!("cargo clean exited {exit_code}: {}", result.stderr_excerpt)
                            };
                            self.store.record_error(&ErrorRecord {
                                id: 0,
                                ts: now,
                                category: "clean".to_string(),
                                path: path.to_str().map(str::to_owned),
                                message: detail,
                            })?;
                        }
                        CleanAttemptOutcome::RunnerFailure => {
                            errors_count += 1;
                            cleanup_failures += 1;
                            self.store.record_error(&ErrorRecord {
                                id: 0,
                                ts: now,
                                category: "clean".to_string(),
                                path: path.to_str().map(str::to_owned),
                                message: result
                                    .attempt_error
                                    .clone()
                                    .context("runner failure is missing its audit message")?,
                            })?;
                        }
                    }
                    if let Some(measurement_error) = &result.measurement_error {
                        errors_count += 1;
                        measurement_failures += 1;
                        self.store.record_error(&ErrorRecord {
                            id: 0,
                            ts: now,
                            category: "clean".to_string(),
                            path: path.to_str().map(str::to_owned),
                            message: measurement_error.clone(),
                        })?;
                    }
                }
                Err(err) => {
                    errors_count += 1;
                    cleanup_failures += 1;
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

        let final_reviews = reviews
            .into_iter()
            .map(|prepared| prepared.review)
            .collect::<Vec<_>>();
        let summary = review_summary(&final_reviews);
        let skipped = summary.skipped_projects as i64 + cleaner_skipped;
        self.store
            .record_review_status(started, source.label(), &summary)?;
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
            cargo_failures,
            measurement_failures,
            cleanup_failures,
            coverage_incomplete,
        };
        self.log_run_cycle(&result);
        Ok(result)
    }

    fn authorized_observations(
        &self,
        scanner: &Scanner,
    ) -> Result<(Vec<ProjectObservation>, Option<DiscoveryGeneration>)> {
        if scanner.policy().is_none() {
            return Ok((Vec::new(), None));
        }
        let Some(generation) = self.store.current_generation(scanner.policy_hash())? else {
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
            "cargo_failures".to_string(),
            Value::from(result.cargo_failures),
        );
        fields.insert(
            "measurement_failures".to_string(),
            Value::from(result.measurement_failures),
        );
        fields.insert(
            "cleanup_failures".to_string(),
            Value::from(result.cleanup_failures),
        );
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
        let mut scheduler_snapshot = self.cycle_snapshot()?;
        let mut schedule = self.scheduler_status_or_initialize(&scheduler_snapshot)?;
        self.schedule_missing_generation_scan(&mut schedule, &scheduler_snapshot)?;
        if scheduler_snapshot.scanner.policy().is_some()
            && !self
                .store
                .has_matching_generation(scheduler_snapshot.scanner.policy_hash())?
            && self.clock.now() >= schedule.next_scan_at
        {
            if let Err(err) = self.scan_cycle() {
                self.defer_after_scan_failure(&mut schedule, &err, &scheduler_snapshot)?;
            } else {
                self.store.clear_scan_retry_at()?;
                schedule.next_scan_at = self.clock.now() + scheduler_snapshot.options.scan_interval;
                self.store.record_scheduler_status(
                    self.clock.now(),
                    schedule.next_clean_at,
                    schedule.next_scan_at,
                )?;
            }
        }
        while !shutdown.is_requested() {
            scheduler_snapshot = self.cycle_snapshot()?;
            self.schedule_missing_generation_scan(&mut schedule, &scheduler_snapshot)?;
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
                    self.defer_after_scan_failure(&mut schedule, &err, &scheduler_snapshot)?;
                    continue;
                }
                self.store.clear_scan_retry_at()?;
                schedule.next_scan_at = self.clock.now() + scheduler_snapshot.options.scan_interval;
            }
            if now >= schedule.next_clean_at {
                self.run_cycle()?;
                schedule.next_clean_at =
                    self.clock.now() + scheduler_snapshot.options.clean_interval;
            }
            self.store.record_scheduler_status(
                self.clock.now(),
                schedule.next_clean_at,
                schedule.next_scan_at,
            )?;
        }
        Ok(())
    }

    fn schedule_missing_generation_scan(
        &self,
        schedule: &mut SchedulerStatus,
        snapshot: &DaemonCycleSnapshot,
    ) -> Result<()> {
        if snapshot.scanner.policy().is_none()
            || self
                .store
                .has_matching_generation(snapshot.scanner.policy_hash())?
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
        snapshot: &DaemonCycleSnapshot,
    ) -> Result<()> {
        let retry_delay = snapshot.options.scan_interval.max(Duration::from_secs(1));
        let now = self.clock.now();
        let mut retry_at = now + retry_delay;
        if !self
            .store
            .has_matching_generation(snapshot.scanner.policy_hash())?
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

    fn scheduler_status_or_initialize(
        &self,
        snapshot: &DaemonCycleSnapshot,
    ) -> Result<SchedulerStatus> {
        if let Some(mut status) = self.store.scheduler_status()? {
            let next_scan_at = clamp_next_scan_at(
                status.next_scan_at,
                self.clock.now(),
                snapshot.options.scan_interval,
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
            .map(|finished_at| finished_at + snapshot.options.clean_interval)
            .unwrap_or(now + snapshot.options.clean_interval);
        let status = SchedulerStatus {
            updated_at: now,
            next_clean_at,
            next_scan_at: now + snapshot.options.scan_interval,
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
