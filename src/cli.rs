use crate::activity::ProcessInspector;
use crate::cache::Cache;
use crate::cleaner::{default_cargo_candidates, resolve_cargo_bin, Cleaner, RealRunner};
use crate::config::{default_path, load, paths, prepare_migration, Config, ConfigWarning, PathSet};
use crate::daemon::{Daemon, DaemonCycleFactory, DaemonCycleSnapshot, DaemonOptions, RunSource};
use crate::identity::{BootSessionId, SystemIdentityProvider};
use crate::lockfile;
use crate::logging::Logger;
use crate::outcome::{
    reason, CommandOutcome, CommandReport, CommandStatus, ScanErrorReport, StreamEvent,
};
use crate::policy::{ProcessEnvironment, ProtectedRootKind, RootProvenance, ScopePolicy};
use crate::safety::{
    bind_review_to_observation, review_project_with_identity_provider, review_summary,
    CleanDecision, ProjectClass, ProjectReview, SafetyOptions, SkipReason,
};
use crate::scanner::{Scanner, ScannerOptions};
use crate::service::{
    resolve_service_binary, ServiceAction, ServiceManager, ServicePlatform, SystemCommandRunner,
};
use crate::store::{DiscoveryOriginKind, ErrorRecord, PlanLoadError, ReviewPlan, Store};
use anyhow::{anyhow, bail, Context, Result};
use clap::{Parser, Subcommand};
use serde::Serialize;
use std::collections::BTreeSet;
use std::fs;
use std::io::{self, BufRead, IsTerminal};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, SystemTime};

const DEFAULT_PREVIEW_LIMIT: usize = 20;

fn print_heading(label: &str) {
    if color_enabled() {
        println!("\x1b[1;36m{label}\x1b[0m");
    } else {
        println!("{label}");
    }
}

fn print_section(label: &str) {
    if color_enabled() {
        println!("\n\x1b[1;34m{label}\x1b[0m");
    } else {
        println!("\n{label}");
    }
}

fn print_row(label: &str, value: impl AsRef<str>) {
    println!("  {label}: {}", value.as_ref());
}

fn color_enabled() -> bool {
    io::stdout().is_terminal() && std::env::var_os("NO_COLOR").is_none()
}

fn format_count(value: usize) -> String {
    format_unsigned(value as u128)
}

fn format_count_i64(value: i64) -> String {
    if value < 0 {
        format!("-{}", format_unsigned(value.unsigned_abs() as u128))
    } else {
        format_unsigned(value as u128)
    }
}

fn format_count_u64(value: u64) -> String {
    format_unsigned(value as u128)
}

fn format_unsigned(mut value: u128) -> String {
    if value == 0 {
        return "0".to_string();
    }

    let mut groups = Vec::new();
    loop {
        groups.push(format!("{:03}", value % 1000));
        value /= 1000;
        if value == 0 {
            break;
        }
    }
    let Some(last) = groups.last_mut() else {
        return "0".to_string();
    };
    *last = last.trim_start_matches('0').to_string();
    groups.reverse();
    groups.join(",")
}

fn format_bytes_i64(value: i64) -> String {
    if value < 0 {
        format!("-{}", format_bytes_u64(value.unsigned_abs()))
    } else {
        format_bytes_u64(value as u64)
    }
}

fn format_bytes_u64(bytes: u64) -> String {
    if bytes < 1024 {
        return format!("{} B", format_count_u64(bytes));
    }

    let mut amount = bytes as f64;
    let mut unit = "B";
    for candidate in ["KiB", "MiB", "GiB", "TiB"] {
        amount /= 1024.0;
        unit = candidate;
        if amount < 1024.0 {
            break;
        }
    }
    format!("{amount:.1} {unit}")
}

fn format_duration_display(duration: Duration) -> String {
    let mut remaining = duration.as_secs();
    if remaining == 0 {
        return if duration.subsec_millis() > 0 {
            format!("{} ms", duration.subsec_millis())
        } else {
            "0 seconds".to_string()
        };
    }

    let mut parts = Vec::new();
    for (unit, seconds) in [
        ("day", 24 * 60 * 60),
        ("hour", 60 * 60),
        ("minute", 60),
        ("second", 1),
    ] {
        let count = remaining / seconds;
        if count == 0 {
            continue;
        }
        remaining %= seconds;
        parts.push(format!(
            "{} {}{}",
            format_count_u64(count),
            unit,
            if count == 1 { "" } else { "s" }
        ));
        if parts.len() == 3 {
            break;
        }
    }

    parts.join(" ")
}

#[derive(Debug, Parser)]
#[command(name = "car-go-clean")]
#[command(about = "Periodically run cargo clean on Rust projects.")]
#[command(version)]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Debug, Subcommand)]
enum Commands {
    Version,
    Service {
        #[command(subcommand)]
        command: ServiceCommands,
    },
    Health {
        #[arg(long)]
        config: Option<PathBuf>,
        #[arg(long)]
        state_dir: Option<PathBuf>,
        #[arg(long)]
        skip_cargo: bool,
        #[arg(long)]
        json: bool,
    },
    Config {
        #[command(subcommand)]
        command: Option<ConfigCommands>,
        #[arg(long, global = true)]
        config: Option<PathBuf>,
    },
    Status {
        #[arg(long)]
        config: Option<PathBuf>,
        #[arg(long)]
        state_dir: Option<PathBuf>,
        #[arg(long)]
        refresh: bool,
        #[arg(long)]
        json: bool,
    },
    Projects {
        #[arg(long)]
        config: Option<PathBuf>,
        #[arg(long)]
        state_dir: Option<PathBuf>,
        #[arg(long)]
        risky: bool,
        #[arg(long)]
        active: bool,
        #[arg(long)]
        json: bool,
        #[arg(long)]
        all: bool,
    },
    /// Refresh the project cache.
    Scan {
        #[arg(long)]
        config: Option<PathBuf>,
        #[arg(long)]
        state_dir: Option<PathBuf>,
        #[arg(long)]
        json: bool,
    },
    /// Scan for projects, then run one cleanup review/cycle now.
    Run {
        #[arg(long)]
        config: Option<PathBuf>,
        #[arg(long)]
        state_dir: Option<PathBuf>,
        /// Show what would be cleaned without invoking Cargo.
        #[arg(long)]
        dry_run: bool,
        /// Use cached discovery state instead of scanning first.
        #[arg(long)]
        no_scan: bool,
        /// Include projects under managed cache or container storage.
        #[arg(long)]
        include_managed_cache: bool,
        /// Include projects used by running processes.
        #[arg(long)]
        include_active: bool,
        /// Bypass scan-error, activity, and quiet-period gates; managed storage still
        /// requires --include-managed-cache.
        #[arg(long)]
        force: bool,
        /// Show every cleanable target in dry-run output.
        #[arg(long, requires = "dry_run")]
        all: bool,
        /// Execute an exact persisted dry-run review.
        #[arg(
            long,
            value_name = "ID",
            conflicts_with_all = [
                "dry_run",
                "no_scan",
                "include_managed_cache",
                "include_active",
                "force",
                "all"
            ]
        )]
        review: Option<i64>,
        /// Emit machine-readable output.
        #[arg(long)]
        json: bool,
    },
    /// Run the long-lived scan and clean scheduler.
    Daemon {
        #[arg(long)]
        config: Option<PathBuf>,
        #[arg(long)]
        state_dir: Option<PathBuf>,
    },
    Stats {
        #[arg(long)]
        since: Option<String>,
        #[arg(long, default_value_t = 10)]
        top: usize,
        #[arg(long)]
        json: bool,
        #[arg(long)]
        state_dir: Option<PathBuf>,
    },
    Logs {
        #[arg(long)]
        errors_only: bool,
        #[arg(long, default_value_t = 100)]
        tail: usize,
        #[arg(long)]
        json: bool,
        #[arg(long)]
        state_dir: Option<PathBuf>,
    },
}

#[derive(Debug)]
struct RunOptions {
    config_path: Option<PathBuf>,
    state_dir: Option<PathBuf>,
    dry_run: bool,
    no_scan: bool,
    include_managed_cache: bool,
    include_active: bool,
    force: bool,
    all: bool,
    review: Option<i64>,
    json: bool,
}

#[derive(Debug, Subcommand)]
enum ServiceCommands {
    Install,
    Status,
    Start,
    Stop,
    Restart,
    Uninstall,
}

#[derive(Debug, Subcommand)]
enum ConfigCommands {
    /// Rename deprecated configuration keys in place.
    Migrate,
}

#[derive(Debug, Clone, Copy)]
struct JsonFailureContext {
    command: &'static str,
    review_id: Option<i64>,
}

impl Cli {
    fn json_failure_context(&self) -> Option<JsonFailureContext> {
        match &self.command {
            Commands::Health { json: true, .. } => Some(JsonFailureContext {
                command: "health",
                review_id: None,
            }),
            Commands::Status { json: true, .. } => Some(JsonFailureContext {
                command: "status",
                review_id: None,
            }),
            Commands::Projects { json: true, .. } => Some(JsonFailureContext {
                command: "projects",
                review_id: None,
            }),
            Commands::Scan { json: true, .. } => Some(JsonFailureContext {
                command: "scan",
                review_id: None,
            }),
            Commands::Run {
                json: true, review, ..
            } => Some(JsonFailureContext {
                command: "run",
                review_id: *review,
            }),
            Commands::Stats { json: true, .. } => Some(JsonFailureContext {
                command: "stats",
                review_id: None,
            }),
            Commands::Logs { json: true, .. } => Some(JsonFailureContext {
                command: "logs",
                review_id: None,
            }),
            _ => None,
        }
    }
}

pub fn run() -> std::process::ExitCode {
    let cli = match Cli::try_parse() {
        Ok(cli) => cli,
        Err(error) => {
            let code = match error.kind() {
                clap::error::ErrorKind::DisplayHelp | clap::error::ErrorKind::DisplayVersion => {
                    CommandOutcome::Complete.code()
                }
                _ => CommandOutcome::Failed.code(),
            };
            let _ = error.print();
            return std::process::ExitCode::from(code);
        }
    };
    let json_failure_context = cli.json_failure_context();
    match execute(cli) {
        Ok(outcome) => std::process::ExitCode::from(outcome.code()),
        Err(error) => {
            if let Some(context) = json_failure_context {
                let status = CommandStatus::failed(failure_reason(&error));
                let report = CommandReport::new(
                    context.command,
                    &status,
                    None,
                    None,
                    context.review_id,
                    Vec::new(),
                    serde_json::Value::Null,
                );
                if let Err(serialization_error) = print_json(&report) {
                    eprintln!("Error: could not serialize failure report: {serialization_error:#}");
                }
            }
            eprintln!("Error: {error:#}");
            std::process::ExitCode::from(CommandOutcome::Failed.code())
        }
    }
}

fn failure_reason(error: &anyhow::Error) -> &'static str {
    for cause in error.chain() {
        if let Some(plan_error) = cause.downcast_ref::<PlanLoadError>() {
            return match plan_error {
                PlanLoadError::Missing => reason::REVIEW_PLAN_MISSING,
                PlanLoadError::Expired => reason::REVIEW_PLAN_EXPIRED,
                PlanLoadError::PolicyMismatch => reason::REVIEW_POLICY_MISMATCH,
                PlanLoadError::GenerationMismatch => reason::REVIEW_GENERATION_MISMATCH,
                PlanLoadError::Storage(_) => reason::COMMAND_FAILED,
            };
        }
        let message = cause.to_string();
        if message.starts_with("another car-go-clean process is running")
            || message.starts_with("acquire lock ")
        {
            return reason::LOCK_UNAVAILABLE;
        }
    }
    reason::COMMAND_FAILED
}

fn print_json(value: &impl Serialize) -> Result<()> {
    println!("{}", serde_json::to_string(value)?);
    Ok(())
}

fn print_stream_event(event: &'static str, data: serde_json::Value) {
    match serde_json::to_string(&StreamEvent::new(event, data)) {
        Ok(serialized) => println!("{serialized}"),
        Err(_) => println!("{{\"format_version\":1,\"event\":\"{event}\",\"data\":null}}"),
    }
}

fn execute(cli: Cli) -> Result<CommandOutcome> {
    match cli.command {
        Commands::Version => {
            println!("{}", env!("CARGO_PKG_VERSION"));
            Ok(CommandOutcome::Complete)
        }
        Commands::Service { command } => service(command).map(|_| CommandOutcome::Complete),
        Commands::Health {
            config,
            state_dir,
            skip_cargo,
            json,
        } => health(config, state_dir, skip_cargo, json),
        Commands::Config { command, config } => match command {
            None => {
                let cfg = load_config(config)?;
                print!("{}", cfg.to_toml()?);
                Ok(CommandOutcome::Complete)
            }
            Some(ConfigCommands::Migrate) => {
                let path = config.unwrap_or_else(default_path);
                match prepare_migration(&path)? {
                    Some(migration) => {
                        print!("{}", migration.unified_diff());
                        migration.apply()?;
                        println!("Migrated {}", path.display());
                    }
                    None => println!("No migration needed for {}", path.display()),
                }
                Ok(CommandOutcome::Complete)
            }
        },
        Commands::Status {
            config,
            state_dir,
            refresh,
            json,
        } => status(config, state_dir, refresh, json),
        Commands::Projects {
            config,
            state_dir,
            risky,
            active,
            json,
            all,
        } => projects(config, state_dir, risky, active, json, all),
        Commands::Scan {
            config,
            state_dir,
            json,
        } => scan(config, state_dir, json),
        Commands::Run {
            config,
            state_dir,
            dry_run,
            no_scan,
            include_managed_cache,
            include_active,
            force,
            all,
            review,
            json,
        } => run_once(RunOptions {
            config_path: config,
            state_dir,
            dry_run,
            no_scan,
            include_managed_cache,
            include_active,
            force,
            all,
            review,
            json,
        }),
        Commands::Daemon { config, state_dir } => {
            daemon(config, state_dir).map(|_| CommandOutcome::Complete)
        }
        Commands::Stats {
            since,
            top,
            json,
            state_dir,
        } => stats(state_dir, since, top, json),
        Commands::Logs {
            errors_only,
            tail,
            json,
            state_dir,
        } => logs(state_dir, errors_only, tail, json),
    }
}

fn service(command: ServiceCommands) -> Result<()> {
    let (platform, platform_label) = match std::env::consts::OS {
        "macos" => (ServicePlatform::MacOs, "macOS (launchd)"),
        "linux" => (ServicePlatform::Linux, "Linux (systemd --user)"),
        _ => bail!("car-go-clean service is supported only on macOS and Linux"),
    };
    let home_dir = PathBuf::from(
        std::env::var_os("HOME").ok_or_else(|| anyhow!("could not determine home directory"))?,
    );
    let home_dir = if home_dir.is_absolute() {
        home_dir
    } else {
        std::env::current_dir()
            .context("could not determine current directory for home directory")?
            .join(home_dir)
    };
    let argv0 = std::env::args_os()
        .next()
        .ok_or_else(|| anyhow!("could not determine service executable name"))?;
    let binary = resolve_service_binary(
        &argv0,
        std::env::var_os("PATH").as_deref(),
        std::env::current_exe().context("could not determine current executable")?,
    )?;
    let definition = match platform {
        ServicePlatform::MacOs => home_dir
            .join("Library/LaunchAgents")
            .join("com.dcchuck.car-go-clean.plist"),
        ServicePlatform::Linux => home_dir.join(".config/systemd/user/car-go-clean.service"),
    };
    let action = match command {
        ServiceCommands::Install => ServiceAction::Install,
        ServiceCommands::Status => ServiceAction::Status,
        ServiceCommands::Start => ServiceAction::Start,
        ServiceCommands::Stop => ServiceAction::Stop,
        ServiceCommands::Restart => ServiceAction::Restart,
        ServiceCommands::Uninstall => ServiceAction::Uninstall,
    };
    let mut manager = ServiceManager::new(platform, home_dir, binary.clone(), SystemCommandRunner);
    let status = match action {
        ServiceAction::Install => manager.install()?,
        ServiceAction::Status => manager.status()?,
        ServiceAction::Start => manager.start()?,
        ServiceAction::Stop => manager.stop()?,
        ServiceAction::Restart => manager.restart()?,
        ServiceAction::Uninstall => manager.uninstall()?,
    };
    let installed_roots = manager.installed_protected_roots()?.map(|roots| {
        roots
            .into_iter()
            .map(|root| ProtectedRootDiagnostics {
                path: root.path.clone(),
                kind: protected_root_kind_label(&root.kind),
                provenance: root_provenance_label(&root.provenance),
            })
            .collect::<Vec<_>>()
    });
    let environment_divergence = manager.environment_divergence(&ProcessEnvironment)?;

    println!("Service");
    print_row("Platform", platform_label);
    print_row("Binary", binary.display().to_string());
    print_row("Definition", definition.display().to_string());
    print_row("Installed", yes_no(status.installed));
    print_row("Enabled", yes_no(status.enabled));
    print_row("Running", yes_no(status.active));
    print_installed_service_roots(installed_roots.as_deref());
    print_row(
        "Environment divergence",
        environment_divergence
            .map(|diverges| if diverges { "detected" } else { "none" })
            .unwrap_or("<unknown>"),
    );
    if environment_divergence == Some(true) {
        println!(
            "  Warning: protected-root inputs differ; run `car-go-clean service install` to recapture the current environment."
        );
    }
    Ok(())
}

fn yes_no(value: bool) -> &'static str {
    if value {
        "yes"
    } else {
        "no"
    }
}

fn health(
    config_path: Option<PathBuf>,
    state_dir: Option<PathBuf>,
    skip_cargo: bool,
    json: bool,
) -> Result<CommandOutcome> {
    let (cfg, config_source) = load_config_with_source(config_path)?;
    let policy = build_policy(&cfg, &config_source)?;
    for dir in &cfg.scan_dirs {
        if !dir.is_dir() {
            return Err(anyhow!("scan_dir {} does not exist", dir.display()));
        }
    }
    for dir in &cfg.project_dirs {
        if !dir.join("Cargo.toml").is_file() {
            return Err(anyhow!("project_dir {} missing Cargo.toml", dir.display()));
        }
    }
    if !skip_cargo {
        resolve_cargo_bin(&default_cargo_candidates())?;
    }

    let store = open_store(state_dir.as_deref())?;
    let since = SystemTime::now() - Duration::from_secs(24 * 60 * 60);
    let errors = store.errors_since(since)?;
    let diagnostics = cleanup_authority_diagnostics(&policy, &store, cfg.scan_interval)?;
    let status = cleanup_authority_status(&diagnostics);
    let report = CommandReport::new(
        "health",
        &status,
        Some(diagnostics.policy_hash.clone()),
        diagnostics
            .current_generation
            .as_ref()
            .map(|generation| generation.id),
        None,
        diagnostics_scan_errors(&diagnostics),
        diagnostics,
    );
    if json {
        print_json(&report)?;
    } else {
        println!("OK");
        if cfg.warnings().contains(&ConfigWarning::LegacyExcludes) {
            println!("WARN: legacy `excludes` is deprecated; run `car-go-clean config migrate`");
        }
        if !errors.is_empty() {
            println!("WARN: {} errors in last 24h", errors.len());
        }
        print_cleanup_authority_diagnostics(&report.data);
        print_text_outcome(&report);
    }
    Ok(status.outcome())
}

fn status(
    config_path: Option<PathBuf>,
    state_dir: Option<PathBuf>,
    refresh: bool,
    json: bool,
) -> Result<CommandOutcome> {
    let (cfg, config_source) = load_config_with_source(config_path)?;
    let policy = build_policy(&cfg, &config_source)?;
    let store = open_store(state_dir.as_deref())?;
    let mut status = CommandStatus::complete();
    if refresh {
        reconcile_review_state(&store, &cfg, &policy)?;
        let safety = SafetyOptions {
            target_quiet_period: cfg.target_quiet_period,
            include_managed_cache: false,
            include_active: false,
            force: false,
        };
        let batch = project_reviews(
            &store,
            &policy,
            &safety,
            cfg.scan_interval,
            "status --refresh",
        )?;
        if batch.coverage_incomplete {
            status = status.merge(review_batch_incomplete_status(&batch));
        }
    }

    let cached_projects = store.project_count()?;
    let total = store.total_bytes_recovered(SystemTime::UNIX_EPOCH)?;
    let diagnostics = cleanup_authority_diagnostics(&policy, &store, cfg.scan_interval)?;
    status = status.merge(cleanup_authority_status(&diagnostics));
    let report = CommandReport::new(
        "status",
        &status,
        Some(diagnostics.policy_hash.clone()),
        diagnostics
            .current_generation
            .as_ref()
            .map(|generation| generation.id),
        None,
        diagnostics_scan_errors(&diagnostics),
        diagnostics,
    );

    if json {
        print_json(&report)?;
        return Ok(status.outcome());
    }

    print_heading("Status");
    print_cleanup_authority_diagnostics(&report.data);
    print_section("Cache");
    print_row("Cached projects", format_count(cached_projects));

    print_section("Review");
    match store.last_review_status()? {
        Some(review_status) => {
            print_review_status(&review_status);
        }
        None => {
            print_row("Last review", "<none>");
            print_row(
                "Refresh",
                "run `car-go-clean run --dry-run` or `car-go-clean status --refresh`",
            );
        }
    }

    print_section("Recovery");
    print_row("Total bytes recovered (all time)", format_bytes_i64(total));
    match store.last_run() {
        Ok(run) => print_row(
            "Last run",
            format!(
                "id={} cleaned={} recovered={} errors={}",
                run.id,
                format_count_i64(run.projects_cleaned),
                format_bytes_i64(run.bytes_recovered),
                format_count_i64(run.errors_count)
            ),
        ),
        Err(_) => print_row("Last run", "<none>"),
    }

    print_section("Schedule");
    print_scheduler_status(&store, &cfg)?;
    print_text_outcome(&report);
    Ok(status.outcome())
}

#[derive(Debug, Serialize, PartialEq, Eq)]
struct CleanupAuthorityDiagnostics {
    config_source: PathBuf,
    canonical_scope_roots: CanonicalScopeRootsDiagnostics,
    policy_hash: String,
    generation_state: &'static str,
    current_generation: Option<CurrentGenerationDiagnostics>,
    protected_roots: Vec<ProtectedRootDiagnostics>,
    incomplete_origins: Vec<IncompleteOriginDiagnostics>,
    service: ServiceStateDiagnostics,
    service_environment_divergence: Option<bool>,
    #[serde(skip)]
    scan_coverage_incomplete: bool,
    #[serde(skip)]
    scan_error_paths: Vec<PathBuf>,
}

#[derive(Debug, Serialize, PartialEq, Eq)]
struct ServiceStateDiagnostics {
    installed: Option<bool>,
    enabled: Option<bool>,
    running: Option<bool>,
    protected_roots: Option<Vec<ProtectedRootDiagnostics>>,
    warning: Option<ServiceWarningDiagnostics>,
}

#[derive(Debug, Serialize, PartialEq, Eq)]
struct ServiceWarningDiagnostics {
    kind: &'static str,
    detail: String,
}

#[derive(Debug, Serialize, PartialEq, Eq)]
struct CanonicalScopeRootsDiagnostics {
    scan_dirs: Vec<PathBuf>,
    project_dirs: Vec<PathBuf>,
}

#[derive(Debug, Serialize, PartialEq, Eq)]
struct CurrentGenerationDiagnostics {
    id: i64,
    policy_hash: String,
    boot_session_id: Option<String>,
}

#[derive(Debug, Serialize, PartialEq, Eq)]
struct ProtectedRootDiagnostics {
    path: PathBuf,
    kind: &'static str,
    provenance: String,
}

#[derive(Debug, Serialize, PartialEq, Eq)]
struct IncompleteOriginDiagnostics {
    kind: &'static str,
    configured_path: PathBuf,
    canonical_path: Option<PathBuf>,
    error: Option<String>,
}

fn diagnostics_scan_errors(diagnostics: &CleanupAuthorityDiagnostics) -> Vec<ScanErrorReport> {
    let mut reports = diagnostics
        .incomplete_origins
        .iter()
        .map(|origin| ScanErrorReport {
            kind: origin.kind.to_string(),
            path: origin
                .canonical_path
                .clone()
                .or_else(|| Some(origin.configured_path.clone())),
            message: origin
                .error
                .clone()
                .unwrap_or_else(|| "origin incomplete".to_string()),
        })
        .collect::<Vec<_>>();
    for path in &diagnostics.scan_error_paths {
        if reports
            .iter()
            .any(|report| report.path.as_deref() == Some(path.as_path()))
        {
            continue;
        }
        reports.push(ScanErrorReport {
            kind: "scan".to_string(),
            path: Some(path.clone()),
            message: "recent scan or worktree discovery error".to_string(),
        });
    }
    if diagnostics.scan_coverage_incomplete && reports.is_empty() {
        reports.push(ScanErrorReport {
            kind: "scan".to_string(),
            path: None,
            message: "recent pathless scan or worktree discovery error".to_string(),
        });
    }
    normalize_scan_errors(&mut reports);
    reports
}

fn cleanup_authority_status(diagnostics: &CleanupAuthorityDiagnostics) -> CommandStatus {
    let mut status = CommandStatus::complete();
    match diagnostics.generation_state {
        "missing" => {
            status = status
                .merge(CommandStatus::incomplete(reason::GENERATION_MISSING))
                .merge_reason(CommandOutcome::Incomplete, reason::SCAN_INCOMPLETE);
        }
        "invalid" => {
            status = status
                .merge(CommandStatus::incomplete(reason::GENERATION_INVALID))
                .merge_reason(CommandOutcome::Incomplete, reason::SCAN_INCOMPLETE);
        }
        "current" => {}
        _ => unreachable!("generation state is internal and enumerated"),
    }
    if diagnostics.scan_coverage_incomplete || !diagnostics.incomplete_origins.is_empty() {
        status = status.merge_reason(CommandOutcome::Incomplete, reason::SCAN_INCOMPLETE);
    }
    if !diagnostics.incomplete_origins.is_empty() {
        status = status.merge_reason(CommandOutcome::Incomplete, reason::ORIGIN_INCOMPLETE);
    }
    status
}

fn cleanup_authority_diagnostics(
    policy: &ScopePolicy,
    store: &Store,
    scan_interval: Duration,
) -> Result<CleanupAuthorityDiagnostics> {
    let policy_diagnostics = policy.diagnostics();
    let current_generation = store.current_generation(policy.hash())?;
    let incomplete_origins = match current_generation.as_ref() {
        Some(generation) => store
            .discovery_origins(generation.id)?
            .into_iter()
            .filter(|origin| !origin.completed)
            .map(|origin| IncompleteOriginDiagnostics {
                kind: discovery_origin_kind_label(origin.kind),
                configured_path: origin.configured_path,
                canonical_path: origin.canonical_path,
                error: origin.error,
            })
            .collect(),
        None => Vec::new(),
    };
    let current_generation = current_generation.map(|generation| CurrentGenerationDiagnostics {
        id: generation.id,
        policy_hash: generation.policy_hash,
        boot_session_id: generation.boot_session_id,
    });
    let protected_roots = policy_diagnostics
        .protected_roots
        .iter()
        .map(|root| ProtectedRootDiagnostics {
            path: root.path.clone(),
            kind: protected_root_kind_label(&root.kind),
            provenance: root_provenance_label(&root.provenance),
        })
        .collect();
    let generation_state = if current_generation.is_some() {
        "current"
    } else if store.project_count()? > 0 {
        "invalid"
    } else {
        "missing"
    };
    let scan_error_since = SystemTime::now()
        .checked_sub(scan_interval)
        .unwrap_or(SystemTime::UNIX_EPOCH);
    let scan_coverage_incomplete = store.scan_coverage_incomplete_since(scan_error_since)?;
    let scan_error_paths = store.scan_error_paths_since(scan_error_since)?;
    let (service, service_environment_divergence) = service_diagnostics();

    Ok(CleanupAuthorityDiagnostics {
        config_source: policy_diagnostics.config_source.to_path_buf(),
        canonical_scope_roots: CanonicalScopeRootsDiagnostics {
            scan_dirs: policy_diagnostics.canonical_scan_roots.to_vec(),
            project_dirs: policy_diagnostics.canonical_project_paths.to_vec(),
        },
        policy_hash: policy.hash().to_string(),
        generation_state,
        current_generation,
        protected_roots,
        incomplete_origins,
        service,
        service_environment_divergence,
        scan_coverage_incomplete,
        scan_error_paths,
    })
}

fn service_diagnostics() -> (ServiceStateDiagnostics, Option<bool>) {
    let Some(home_dir) = std::env::var_os("HOME").map(PathBuf::from) else {
        return (
            ServiceStateDiagnostics {
                installed: None,
                enabled: None,
                running: None,
                protected_roots: None,
                warning: Some(ServiceWarningDiagnostics {
                    kind: "service_probe_failed",
                    detail: "HOME is unavailable; service definition location is unknown"
                        .to_string(),
                }),
            },
            None,
        );
    };
    let platform = match std::env::consts::OS {
        "macos" => ServicePlatform::MacOs,
        "linux" => ServicePlatform::Linux,
        _ => {
            return (
                ServiceStateDiagnostics {
                    installed: None,
                    enabled: None,
                    running: None,
                    protected_roots: None,
                    warning: Some(ServiceWarningDiagnostics {
                        kind: "service_probe_failed",
                        detail: "service management is unsupported on this platform".to_string(),
                    }),
                },
                None,
            );
        }
    };
    let mut manager = ServiceManager::new(
        platform,
        home_dir,
        PathBuf::from("/car-go-clean-service-status-only"),
        SystemCommandRunner,
    );
    let status = match manager.status() {
        Ok(status) => status,
        Err(error) => {
            return (
                ServiceStateDiagnostics {
                    installed: None,
                    enabled: None,
                    running: None,
                    protected_roots: None,
                    warning: Some(ServiceWarningDiagnostics {
                        kind: "service_probe_failed",
                        detail: format!("{error:#}"),
                    }),
                },
                None,
            );
        }
    };
    let installed_roots = match manager.installed_protected_roots() {
        Ok(roots) => roots.map(|roots| {
            roots
                .into_iter()
                .map(|root| ProtectedRootDiagnostics {
                    path: root.path.clone(),
                    kind: protected_root_kind_label(&root.kind),
                    provenance: root_provenance_label(&root.provenance),
                })
                .collect()
        }),
        Err(error) => {
            return (
                ServiceStateDiagnostics {
                    installed: Some(status.installed),
                    enabled: Some(status.enabled),
                    running: Some(status.active),
                    protected_roots: None,
                    warning: Some(ServiceWarningDiagnostics {
                        kind: "service_definition_unreadable",
                        detail: format!("{error:#}"),
                    }),
                },
                None,
            );
        }
    };
    let divergence = match manager.environment_divergence(&ProcessEnvironment) {
        Ok(divergence) => divergence,
        Err(error) => {
            return (
                ServiceStateDiagnostics {
                    installed: Some(status.installed),
                    enabled: Some(status.enabled),
                    running: Some(status.active),
                    protected_roots: installed_roots,
                    warning: Some(ServiceWarningDiagnostics {
                        kind: "service_definition_unreadable",
                        detail: format!("{error:#}"),
                    }),
                },
                None,
            );
        }
    };
    (
        ServiceStateDiagnostics {
            installed: Some(status.installed),
            enabled: Some(status.enabled),
            running: Some(status.active),
            protected_roots: installed_roots,
            warning: None,
        },
        divergence,
    )
}

fn print_cleanup_authority_diagnostics(diagnostics: &CleanupAuthorityDiagnostics) {
    print_section("Cleanup authority");
    print_row(
        "Config source",
        diagnostics.config_source.display().to_string(),
    );
    print_row(
        "Canonical scan roots",
        format_paths(&diagnostics.canonical_scope_roots.scan_dirs),
    );
    print_row(
        "Canonical project roots",
        format_paths(&diagnostics.canonical_scope_roots.project_dirs),
    );
    print_row("Policy hash", &diagnostics.policy_hash);
    print_row("Generation state", diagnostics.generation_state);
    print_row(
        "Current generation",
        diagnostics
            .current_generation
            .as_ref()
            .map(|generation| {
                format!(
                    "id={} boot_session={}",
                    generation.id,
                    generation.boot_session_id.as_deref().unwrap_or("<unknown>")
                )
            })
            .unwrap_or_else(|| "<none>".to_string()),
    );
    print_row(
        "Protected roots",
        format_count(diagnostics.protected_roots.len()),
    );
    for root in &diagnostics.protected_roots {
        println!(
            "    {} ({}, {})",
            root.path.display(),
            root.kind,
            root.provenance
        );
    }
    print_row(
        "Incomplete origins",
        format_count(diagnostics.incomplete_origins.len()),
    );
    for origin in &diagnostics.incomplete_origins {
        let canonical_path = origin
            .canonical_path
            .as_deref()
            .map(|path| path.display().to_string())
            .unwrap_or_else(|| "<none>".to_string());
        println!(
            "    {} ({}, canonical={}, {})",
            origin.configured_path.display(),
            origin.kind,
            canonical_path,
            origin.error.as_deref().unwrap_or("unknown error")
        );
    }
    print_row(
        "Service installed",
        yes_no_unknown(diagnostics.service.installed),
    );
    print_row(
        "Service enabled",
        yes_no_unknown(diagnostics.service.enabled),
    );
    print_row(
        "Service running",
        yes_no_unknown(diagnostics.service.running),
    );
    print_installed_service_roots(diagnostics.service.protected_roots.as_deref());
    print_row(
        "Service environment divergence",
        diagnostics
            .service_environment_divergence
            .map(|diverges| if diverges { "detected" } else { "none" })
            .unwrap_or("<unknown>"),
    );
    if diagnostics.service_environment_divergence == Some(true) {
        println!(
            "    Warning: protected-root inputs differ; run `car-go-clean service install` to recapture the current environment."
        );
    }
    if let Some(warning) = &diagnostics.service.warning {
        print_row("Service warning", warning.kind);
        println!("    {}", warning.detail);
    }
}

fn yes_no_unknown(value: Option<bool>) -> &'static str {
    value.map(yes_no).unwrap_or("<unknown>")
}

fn print_installed_service_roots(roots: Option<&[ProtectedRootDiagnostics]>) {
    let Some(roots) = roots else {
        print_row("Installed service protected roots", "<unknown>");
        return;
    };
    print_row(
        "Installed service protected roots",
        format_count(roots.len()),
    );
    for root in roots {
        println!(
            "    {} ({}, {})",
            root.path.display(),
            root.kind,
            root.provenance
        );
    }
}

fn format_paths(paths: &[PathBuf]) -> String {
    if paths.is_empty() {
        "<none>".to_string()
    } else {
        paths
            .iter()
            .map(|path| path.display().to_string())
            .collect::<Vec<_>>()
            .join(", ")
    }
}

fn discovery_origin_kind_label(kind: DiscoveryOriginKind) -> &'static str {
    match kind {
        DiscoveryOriginKind::ScanRoot => "scan_root",
        DiscoveryOriginKind::ExplicitProject => "explicit_project",
    }
}

fn protected_root_kind_label(kind: &ProtectedRootKind) -> &'static str {
    match kind {
        ProtectedRootKind::Cargo => "cargo",
        ProtectedRootKind::Rustup => "rustup",
        ProtectedRootKind::GoModule => "go_module",
        ProtectedRootKind::Bun => "bun",
        ProtectedRootKind::ManagedCache => "managed_cache",
        ProtectedRootKind::Container => "container",
    }
}

fn root_provenance_label(provenance: &RootProvenance) -> String {
    match provenance {
        RootProvenance::Default => "default".to_string(),
        RootProvenance::Environment(variable) => format!("environment:{variable}"),
        RootProvenance::ServiceDefinition => "service_definition".to_string(),
        RootProvenance::Structural => "structural".to_string(),
    }
}

fn projects(
    config_path: Option<PathBuf>,
    state_dir: Option<PathBuf>,
    risky: bool,
    active: bool,
    json: bool,
    all: bool,
) -> Result<CommandOutcome> {
    let (cfg, config_source) = load_config_with_source(config_path)?;
    let policy = build_policy(&cfg, &config_source)?;
    let store = open_store(state_dir.as_deref())?;
    reconcile_review_state(&store, &cfg, &policy)?;
    let safety = SafetyOptions {
        target_quiet_period: cfg.target_quiet_period,
        include_managed_cache: risky,
        include_active: active,
        force: false,
    };
    let batch = project_reviews(&store, &policy, &safety, cfg.scan_interval, "projects")?;
    let status = review_batch_incomplete_status(&batch);
    let diagnostics = cleanup_authority_diagnostics(&policy, &store, cfg.scan_interval)?;
    let scan_errors = review_batch_scan_errors(&batch, &diagnostics);
    let generation = batch.generation;
    let reviews = batch.reviews;
    #[derive(Serialize)]
    struct ProjectsData<'a> {
        reviews: &'a [ProjectReview],
    }
    let report = CommandReport::new(
        "projects",
        &status,
        Some(policy.hash().to_string()),
        generation,
        None,
        scan_errors,
        ProjectsData { reviews: &reviews },
    );

    if json {
        print_json(&report)?;
        return Ok(status.outcome());
    }

    if all {
        for review in &reviews {
            println!(
                "{}\t{}\t{}\t{}",
                decision_label(&review.decision),
                class_label(review.class),
                review.target_bytes,
                review.path.display()
            );
        }
    } else {
        print_review_summary("Projects", &reviews);
        print_skip_breakdown(&review_summary(&reviews));
        print_cleanable_target_preview(&reviews, DEFAULT_PREVIEW_LIMIT, false);
    }
    print_text_outcome(&report);
    Ok(status.outcome())
}

fn scan(
    config_path: Option<PathBuf>,
    state_dir: Option<PathBuf>,
    json: bool,
) -> Result<CommandOutcome> {
    let path_set = paths_for(state_dir.as_deref());
    let _lock = lockfile::try_acquire(&path_set.lock_path)
        .context("another car-go-clean process is running")?;
    let (cfg, config_source) = load_config_with_source(config_path)?;
    let policy = build_policy(&cfg, &config_source)?;
    let store = open_store_at(&path_set)?;
    let execution = scan_and_report(&store, &cfg, &policy, json, true)?;
    Ok(execution.status.outcome())
}

#[derive(Debug)]
struct ScanExecution {
    status: CommandStatus,
    generation: i64,
    policy_hash: String,
    scan_errors: Vec<ScanErrorReport>,
}

fn scan_and_report(
    store: &Store,
    cfg: &Config,
    policy: &ScopePolicy,
    json: bool,
    terminal: bool,
) -> Result<ScanExecution> {
    let result = daemon_for_scan(store, cfg, policy).scan_cycle()?;
    let projects = result
        .origins
        .iter()
        .flat_map(|origin| origin.projects.iter().map(|project| project.path.clone()))
        .collect::<BTreeSet<_>>();
    let scan_errors = result
        .origins
        .iter()
        .filter(|origin| !origin.completed)
        .map(|origin| ScanErrorReport {
            kind: match origin.kind {
                crate::scanner::DiscoveryOriginKind::ScanRoot => "scan_root",
                crate::scanner::DiscoveryOriginKind::ExplicitProject => "explicit_project",
            }
            .to_string(),
            path: origin
                .canonical_path
                .clone()
                .or_else(|| Some(origin.configured_path.clone())),
            message: origin
                .error
                .clone()
                .unwrap_or_else(|| "origin incomplete".to_string()),
        })
        .collect::<Vec<_>>();
    let incomplete = result.errors > 0 || result.origins.iter().any(|origin| !origin.completed);
    let mut status = if incomplete {
        CommandStatus::incomplete(reason::SCAN_INCOMPLETE)
    } else {
        CommandStatus::complete()
    };
    if result.origins.iter().any(|origin| !origin.completed) {
        status = status.merge_reason(CommandOutcome::Incomplete, reason::ORIGIN_INCOMPLETE);
    }
    let data = serde_json::json!({
        "origins": result.origins.iter().map(|origin| {
            serde_json::json!({
                "kind": origin.kind,
                "path": origin.configured_path.to_string_lossy(),
                "canonical_path": origin.canonical_path.as_ref().map(|path| path.to_string_lossy()),
                "completed": origin.completed,
                "error": origin.error,
            })
        }).collect::<Vec<_>>(),
        "projects": projects.iter().map(|path| path.to_string_lossy()).collect::<Vec<_>>(),
    });
    let report = CommandReport::new(
        "scan",
        &status,
        Some(result.policy_hash.clone()),
        Some(result.generation),
        None,
        scan_errors.clone(),
        data,
    );
    if json {
        if terminal {
            print_json(&report)?;
        } else {
            print_stream_event(
                "scan",
                serde_json::json!({
                    "policy_hash": result.policy_hash.clone(),
                    "generation": result.generation,
                    "scan_errors": report.scan_errors.iter().map(|error| {
                        serde_json::json!({
                            "kind": error.kind,
                            "path": error.path.as_ref().map(|path| path.to_string_lossy()),
                            "message": error.message,
                        })
                    }).collect::<Vec<_>>(),
                    "result": report.data,
                }),
            );
        }
    } else {
        println!("Scan complete: errors={}", result.errors);
        println!(
            "Authority: generation={} policy_hash={}",
            result.generation, result.policy_hash
        );
        for origin in result.origins.iter().filter(|origin| !origin.completed) {
            println!(
                "Incomplete origin: {} ({})",
                origin.configured_path.display(),
                origin.error.as_deref().unwrap_or("unknown error")
            );
        }
        if terminal {
            print_text_outcome(&report);
        }
    }
    Ok(ScanExecution {
        status,
        generation: result.generation,
        policy_hash: result.policy_hash,
        scan_errors,
    })
}

fn run_once(options: RunOptions) -> Result<CommandOutcome> {
    let RunOptions {
        config_path,
        state_dir,
        dry_run,
        no_scan,
        include_managed_cache,
        include_active,
        force,
        all,
        review,
        json,
    } = options;
    let path_set = paths_for(state_dir.as_deref());
    let _lock = lockfile::try_acquire(&path_set.lock_path)
        .context("another car-go-clean process is running")?;
    let (cfg, config_source) = load_config_with_source(config_path)?;
    let policy = build_policy(&cfg, &config_source)?;
    let safety = SafetyOptions {
        target_quiet_period: cfg.target_quiet_period,
        include_managed_cache,
        include_active,
        force,
    };
    let store = open_store_at(&path_set)?;

    if let Some(review_id) = review {
        let current_generation = store.current_generation(policy.hash())?;
        let current_generation_id = current_generation
            .as_ref()
            .map(|generation| generation.id)
            .unwrap_or(-1);
        let plan = store
            .load_review_plan(
                review_id,
                SystemTime::now(),
                policy.hash(),
                current_generation_id,
            )
            .map_err(|error| anyhow!(error))?;
        let cargo = resolve_cargo_bin(&default_cargo_candidates())?;
        let daemon =
            daemon_for_clean(&store, &cfg, cargo, &policy).with_target_reporter(move |review| {
                if json {
                    print_stream_event(
                        "target",
                        serde_json::json!({
                            "project": review.path.to_string_lossy(),
                            "target": review.target_path.to_string_lossy(),
                        }),
                    );
                } else {
                    println!(
                        "Cleaning {} (project {})",
                        review.target_path.display(),
                        review.path.display()
                    );
                }
            });
        // Only rows already persisted as Cleanable reach the execution engine.
        // Preserve that plan-time managed-storage opt-in while current activity,
        // scan diagnostics, quiet-period, policy, and identity checks stay strict.
        let reviewed_safety = SafetyOptions {
            include_managed_cache: true,
            ..safety
        };
        let result = daemon.execute_reviews_with_safety(
            plan.targets
                .into_iter()
                .map(|target| target.review)
                .collect(),
            plan.coverage_incomplete,
            reviewed_safety,
            &crate::activity::SysinfoProcessInspector,
            RunSource::Reviewed,
        )?;
        let diagnostics = cleanup_authority_diagnostics(&policy, &store, cfg.scan_interval)?;
        let scan_errors = diagnostics_scan_errors(&diagnostics);
        let status = run_result_status(&result, &scan_errors);
        let report = run_command_report(
            &status,
            &result,
            policy.hash(),
            Some(current_generation_id),
            Some(plan.id),
            scan_errors,
        );
        print_run_result(&report, json)?;
        return Ok(status.outcome());
    }

    let mut status = CommandStatus::complete();
    let mut scan_errors = Vec::new();
    let mut scan_generation = None;
    if !no_scan {
        let execution = scan_and_report(&store, &cfg, &policy, json, false)?;
        status = status.merge(execution.status);
        scan_generation = Some(execution.generation);
        scan_errors = execution.scan_errors;
        debug_assert_eq!(execution.policy_hash, policy.hash());
    }

    if dry_run {
        reconcile_review_state(&store, &cfg, &policy)?;
        let batch = project_reviews(&store, &policy, &safety, cfg.scan_interval, "dry-run")?;
        status = status.merge(review_batch_incomplete_status(&batch));
        let diagnostics = cleanup_authority_diagnostics(&policy, &store, cfg.scan_interval)?;
        scan_errors.extend(review_batch_scan_errors(&batch, &diagnostics));
        normalize_scan_errors(&mut scan_errors);
        let plan_generation = batch.generation;
        let generation = batch.generation.or(scan_generation);
        let reviews = batch.reviews;
        let summary = review_summary(&reviews);
        let plan = match plan_generation {
            Some(generation_id) => Some(
                store.create_review_plan(
                    SystemTime::now(),
                    policy.hash(),
                    generation_id,
                    batch.coverage_incomplete,
                    i64::try_from(summary.cleanable_bytes)
                        .context("candidate bytes exceed supported range")?,
                    &reviews,
                )?,
            ),
            None => None,
        };
        #[derive(Serialize)]
        struct DryRunData<'a> {
            review: Option<serde_json::Value>,
            reviews: &'a [ProjectReview],
            summary: &'a crate::safety::ReviewSummary,
            coverage_incomplete: bool,
        }
        let report = CommandReport::new(
            "run",
            &status,
            Some(policy.hash().to_string()),
            generation,
            plan.as_ref().map(|plan| plan.id),
            scan_errors,
            DryRunData {
                review: plan.as_ref().map(review_plan_json),
                reviews: &reviews,
                summary: &summary,
                coverage_incomplete: batch.coverage_incomplete,
            },
        );
        if json {
            print_json(&report)?;
        } else {
            print_review_summary("Dry run", report.data.reviews);
            print_skip_breakdown(report.data.summary);
            print_cleanable_target_preview(report.data.reviews, DEFAULT_PREVIEW_LIMIT, all);
            match &plan {
                Some(plan) => print_review_plan(plan),
                None => println!(
                    "No review ID was created because no valid matching discovery generation exists."
                ),
            }
            print_text_outcome(&report);
        }
        return Ok(status.outcome());
    }

    let cargo = resolve_cargo_bin(&default_cargo_candidates())?;
    let daemon =
        daemon_for_clean(&store, &cfg, cargo, &policy).with_target_reporter(move |review| {
            if json {
                print_stream_event(
                    "target",
                    serde_json::json!({
                        "project": review.path.to_string_lossy(),
                        "target": review.target_path.to_string_lossy(),
                    }),
                );
            } else {
                println!(
                    "Cleaning {} (project {})",
                    review.target_path.display(),
                    review.path.display()
                );
            }
        });
    let result = daemon.run_cycle_with_safety(safety, &crate::activity::SysinfoProcessInspector)?;
    let diagnostics = cleanup_authority_diagnostics(&policy, &store, cfg.scan_interval)?;
    scan_errors.extend(diagnostics_scan_errors(&diagnostics));
    normalize_scan_errors(&mut scan_errors);
    status = status.merge(run_result_status(&result, &scan_errors));
    let generation = store
        .current_generation(policy.hash())?
        .map(|generation| generation.id)
        .or(scan_generation);
    let report = run_command_report(
        &status,
        &result,
        policy.hash(),
        generation,
        None,
        scan_errors,
    );
    print_run_result(&report, json)?;
    Ok(status.outcome())
}

#[derive(Debug)]
struct ReviewBatch {
    reviews: Vec<ProjectReview>,
    coverage_incomplete: bool,
    generation: Option<i64>,
    generation_invalid: bool,
    origin_incomplete: bool,
    scan_error_paths: Vec<PathBuf>,
}

fn review_batch_incomplete_status(batch: &ReviewBatch) -> CommandStatus {
    if !batch.coverage_incomplete {
        return CommandStatus::complete();
    }
    let mut status = CommandStatus::incomplete(reason::SCAN_INCOMPLETE);
    if batch.generation.is_none() {
        status = status.merge_reason(
            CommandOutcome::Incomplete,
            if batch.generation_invalid {
                reason::GENERATION_INVALID
            } else {
                reason::GENERATION_MISSING
            },
        );
    }
    if batch.origin_incomplete {
        status = status.merge_reason(CommandOutcome::Incomplete, reason::ORIGIN_INCOMPLETE);
    }
    status
}

fn review_batch_scan_errors(
    batch: &ReviewBatch,
    diagnostics: &CleanupAuthorityDiagnostics,
) -> Vec<ScanErrorReport> {
    let mut reports = diagnostics_scan_errors(diagnostics);
    for path in &batch.scan_error_paths {
        if reports
            .iter()
            .any(|report| report.path.as_deref() == Some(path.as_path()))
        {
            continue;
        }
        reports.push(ScanErrorReport {
            kind: "scan".to_string(),
            path: Some(path.clone()),
            message: "recent scan or worktree discovery error".to_string(),
        });
    }
    reports.sort_by(|left, right| {
        left.path
            .cmp(&right.path)
            .then_with(|| left.kind.cmp(&right.kind))
    });
    reports
}

fn project_reviews(
    store: &Store,
    policy: &ScopePolicy,
    safety: &SafetyOptions,
    scan_interval: Duration,
    source: &str,
) -> Result<ReviewBatch> {
    let now = SystemTime::now();
    let generation = store.current_generation(policy.hash())?;
    let generation_invalid = generation.is_none() && store.project_count()? > 0;
    let origin_incomplete = match generation.as_ref() {
        Some(generation) => store
            .discovery_origins(generation.id)?
            .iter()
            .any(|origin| !origin.completed),
        None => false,
    };
    let observations = match &generation {
        Some(generation) => store.authorized_observations(generation.id)?,
        None => Vec::new(),
    };
    let paths: Vec<PathBuf> = observations
        .iter()
        .map(|observation| observation.project_path.clone())
        .collect();
    let scan_error_since = now
        .checked_sub(scan_interval)
        .unwrap_or(SystemTime::UNIX_EPOCH);
    let scan_errors = store.scan_error_paths_since(scan_error_since)?;
    let scan_coverage_incomplete = store.scan_coverage_incomplete_since(scan_error_since)?;
    let durable_generation_incomplete =
        store.current_generation_coverage_incomplete(policy.hash())?;
    let discovery_blocks = store.blocked_worktree_discovery_paths()?;
    let activity = crate::activity::SysinfoProcessInspector.active_projects(&paths)?;

    let reviews = observations
        .iter()
        .map(|observation| {
            let mut review = review_project_with_identity_provider(
                &observation.project_path,
                &scan_errors,
                &discovery_blocks,
                &activity,
                now,
                safety,
                &SystemIdentityProvider,
            )?;
            if review.decision == CleanDecision::Cleanable {
                if !policy.contains_project(&review.path) {
                    review.decision = CleanDecision::Skipped(SkipReason::OutOfScope);
                } else if policy.is_excluded(&review.path)
                    || policy.is_excluded(&review.target_path)
                {
                    review.decision = CleanDecision::Skipped(SkipReason::Excluded);
                }
            }
            if review.decision == CleanDecision::Cleanable {
                let observed_boot = observation
                    .boot_session_id
                    .as_ref()
                    .map(|boot| BootSessionId(boot.clone()));
                bind_review_to_observation(
                    &mut review,
                    &observation.project_identity,
                    observation.target_identity.as_ref(),
                    observed_boot.as_ref(),
                );
            }
            Ok(review)
        })
        .collect::<Result<Vec<_>>>()?;
    record_review_diagnostics(store, &reviews)?;
    store.record_review_status(now, source, &review_summary(&reviews))?;
    Ok(ReviewBatch {
        reviews,
        coverage_incomplete: generation.is_none()
            || durable_generation_incomplete
            || scan_coverage_incomplete
            || !discovery_blocks.is_empty(),
        generation: generation.map(|generation| generation.id),
        generation_invalid,
        origin_incomplete,
        scan_error_paths: scan_errors,
    })
}

fn review_plan_json(plan: &ReviewPlan) -> serde_json::Value {
    serde_json::json!({
        "id": plan.id,
        "policy_hash": plan.policy_hash,
        "generation": plan.generation_id,
        "created": humantime::format_rfc3339_seconds(plan.created_at).to_string(),
        "expires": humantime::format_rfc3339_seconds(plan.expires_at).to_string(),
        "candidate_bytes": plan.candidate_bytes,
    })
}

fn print_review_plan(plan: &ReviewPlan) {
    println!("Review ID: {}", plan.id);
    println!("Policy hash: {}", plan.policy_hash);
    println!("Discovery generation: {}", plan.generation_id);
    println!(
        "Created: {}",
        humantime::format_rfc3339_seconds(plan.created_at)
    );
    println!(
        "Expires: {}",
        humantime::format_rfc3339_seconds(plan.expires_at)
    );
    println!("Candidate bytes: {}", plan.candidate_bytes);
}

#[derive(Debug, Clone, Serialize)]
struct RunResultData {
    run_id: i64,
    cleaned: i64,
    skipped: i64,
    bytes_recovered: i64,
    errors: i64,
    cargo_failures: i64,
    measurement_failures: i64,
    cleanup_failures: i64,
    coverage_incomplete: bool,
}

fn run_command_report(
    status: &CommandStatus,
    result: &crate::daemon::RunCycleResult,
    policy_hash: &str,
    generation: Option<i64>,
    review_id: Option<i64>,
    scan_errors: Vec<ScanErrorReport>,
) -> CommandReport<RunResultData> {
    CommandReport::new(
        "run",
        status,
        Some(policy_hash.to_string()),
        generation,
        review_id,
        scan_errors,
        RunResultData {
            run_id: result.run_id,
            cleaned: result.cleaned,
            skipped: result.skipped,
            bytes_recovered: result.bytes_recovered,
            errors: result.errors,
            cargo_failures: result.cargo_failures,
            measurement_failures: result.measurement_failures,
            cleanup_failures: result.cleanup_failures,
            coverage_incomplete: result.coverage_incomplete,
        },
    )
}

fn print_run_result(report: &CommandReport<RunResultData>, json: bool) -> Result<()> {
    if json {
        print_json(report)?;
    } else {
        println!(
            "Run complete: cleaned={} skipped={} recovered={} errors={}",
            report.data.cleaned,
            report.data.skipped,
            report.data.bytes_recovered,
            report.data.errors
        );
        print_text_outcome(report);
    }
    Ok(())
}

fn run_result_status(
    result: &crate::daemon::RunCycleResult,
    scan_errors: &[ScanErrorReport],
) -> CommandStatus {
    let mut status = CommandStatus::complete();
    if result.cargo_failures > 0 {
        status = status.merge(CommandStatus::failed(reason::CARGO_FAILED));
    }
    if result.measurement_failures > 0 {
        status = status.merge(CommandStatus::failed(reason::MEASUREMENT_FAILED));
    }
    if result.cleanup_failures > 0 {
        status = status.merge(CommandStatus::failed(reason::CLEANUP_FAILED));
    }
    debug_assert_eq!(
        result.errors,
        result.cargo_failures + result.measurement_failures + result.cleanup_failures
    );
    if result.coverage_incomplete {
        status = status.merge(CommandStatus::incomplete(reason::SCAN_INCOMPLETE));
        if scan_errors
            .iter()
            .any(|error| matches!(error.kind.as_str(), "scan_root" | "explicit_project"))
        {
            status = status.merge_reason(CommandOutcome::Incomplete, reason::ORIGIN_INCOMPLETE);
        }
    }
    status
}

fn normalize_scan_errors(errors: &mut Vec<ScanErrorReport>) {
    errors.sort_by(|left, right| {
        left.path
            .cmp(&right.path)
            .then_with(|| left.kind.cmp(&right.kind))
            .then_with(|| left.message.cmp(&right.message))
    });
    errors.dedup();
}

fn print_text_outcome<T>(report: &CommandReport<T>) {
    println!(
        "Outcome: {} (code={})",
        report.outcome.kind, report.outcome.code
    );
    if !report.outcome.reasons.is_empty() {
        println!("Reasons: {}", report.outcome.reasons.join(", "));
    }
}

fn print_review_summary(label: &str, reviews: &[ProjectReview]) {
    let summary = review_summary(reviews);
    println!("{label}");
    print_summary_counts(&summary);
}

fn print_review_status(status: &crate::store::ReviewStatus) {
    let reviewed_age = SystemTime::now()
        .duration_since(status.reviewed_at)
        .unwrap_or_default();
    print_row(
        "Last review",
        format!("{} ago", format_duration_display(reviewed_age)),
    );
    print_row("Source", review_source_label(&status.source));
    print_summary_counts(&status.summary);
    print_skip_breakdown(&status.summary);
}

fn print_scheduler_status(store: &Store, cfg: &Config) -> Result<()> {
    print_row(
        "Clean interval",
        format_duration_display(cfg.clean_interval),
    );
    print_row("Scan interval", format_duration_display(cfg.scan_interval));
    match store.scheduler_status()? {
        Some(status) => {
            let now = SystemTime::now();
            print_row(
                "Scheduler state",
                format!(
                    "recorded {} ago",
                    format_duration_display(
                        now.duration_since(status.updated_at).unwrap_or_default(),
                    )
                ),
            );
            print_row(
                "Next scheduled clean",
                schedule_time_label(status.next_clean_at, now),
            );
            print_row(
                "Next scheduled scan",
                schedule_time_label(status.next_scan_at, now),
            );
        }
        None => {
            print_row("Scheduler state", "<not recorded>");
            print_row("Next scheduled clean", "<not recorded>");
            print_row("Next scheduled scan", "<not recorded>");
        }
    }
    Ok(())
}

fn schedule_time_label(when: SystemTime, now: SystemTime) -> String {
    match when.duration_since(now) {
        Ok(remaining) => format!("in {}", format_duration_display(remaining)),
        Err(_) => {
            let overdue = now.duration_since(when).unwrap_or_default();
            format!("overdue by {}", format_duration_display(overdue))
        }
    }
}

fn review_source_label(source: &str) -> String {
    if source == "run" {
        "run (pre-clean snapshot)".to_string()
    } else {
        source.to_string()
    }
}

fn print_summary_counts(summary: &crate::safety::ReviewSummary) {
    print_row("Total projects", format_count(summary.total_projects));
    print_row(
        "Cleanable projects",
        format_count(summary.cleanable_projects),
    );
    print_row("Skipped projects", format_count(summary.skipped_projects));
    print_row("Cleanable bytes", format_bytes_u64(summary.cleanable_bytes));
}

fn print_skip_breakdown(summary: &crate::safety::ReviewSummary) {
    if summary.skipped_projects == 0 {
        return;
    }

    let mut parts = Vec::new();
    if summary.no_target > 0 {
        parts.push(format!("no_target={}", summary.no_target));
    }
    if summary.active_recent_write > 0 {
        parts.push(format!("recent_write={}", summary.active_recent_write));
    }
    if summary.active_process > 0 {
        parts.push(format!("active_process={}", summary.active_process));
    }
    if summary.managed_cache > 0 {
        parts.push(format!("managed_cache={}", summary.managed_cache));
    }
    if summary.container_storage > 0 {
        parts.push(format!("container_storage={}", summary.container_storage));
    }
    if summary.scan_error > 0 {
        parts.push(format!("scan_error={}", summary.scan_error));
    }
    if summary.target_read_error > 0 {
        parts.push(format!("target_read_error={}", summary.target_read_error));
    }
    if !parts.is_empty() {
        print_row("Skipped breakdown", parts.join(", "));
    }
}

fn print_cleanable_target_preview(reviews: &[ProjectReview], limit: usize, all: bool) {
    let cleanable: Vec<_> = reviews
        .iter()
        .filter(|review| review.decision == CleanDecision::Cleanable)
        .collect();
    if cleanable.is_empty() {
        return;
    }

    let shown = if all {
        cleanable.len()
    } else {
        cleanable.len().min(limit)
    };
    println!("Cleanable target preview:");
    for review in cleanable.iter().take(shown) {
        println!(
            "  {}\t{}\t{}",
            review.target_bytes,
            review.target_path.display(),
            review.path.display()
        );
    }
    if shown < cleanable.len() {
        println!("Use `projects --all` to show all {} rows.", reviews.len());
        println!("Use `run --dry-run --all` to show all cleanable targets.");
    }
}

fn record_review_diagnostics(store: &Store, reviews: &[ProjectReview]) -> Result<()> {
    let now = SystemTime::now();
    for review in reviews {
        if review.decision == CleanDecision::Skipped(SkipReason::TargetReadError) {
            store.record_error(&ErrorRecord {
                id: 0,
                ts: now,
                category: "review".to_string(),
                path: review.target_path.to_str().map(str::to_owned),
                message: "target read error: unable to read direct target directory".to_string(),
            })?;
        }
    }
    Ok(())
}

fn decision_label(decision: &CleanDecision) -> &'static str {
    match decision {
        CleanDecision::Cleanable => "cleanable",
        CleanDecision::Skipped(reason) => match reason {
            SkipReason::NoTarget => "skipped:no_target",
            SkipReason::ActiveRecentWrite { .. } => "skipped:active_recent_write",
            SkipReason::ActiveProcess => "skipped:active_process",
            SkipReason::ManagedCache => "skipped:managed_cache",
            SkipReason::ContainerStorage => "skipped:container_storage",
            SkipReason::ScanError => "skipped:scan_error",
            SkipReason::TargetReadError => "skipped:target_read_error",
            SkipReason::InvalidManifest => "skipped:invalid_manifest",
            SkipReason::ProjectIdentityUnavailable => "skipped:project_identity_unavailable",
            SkipReason::TargetIdentityUnavailable => "skipped:target_identity_unavailable",
            SkipReason::CrossDeviceTarget => "skipped:cross_device_target",
            SkipReason::ProjectIdentityChanged => "skipped:project_identity_changed",
            SkipReason::TargetIdentityChanged => "skipped:target_identity_changed",
            SkipReason::OutOfScope => "skipped:out_of_scope",
            SkipReason::Excluded => "skipped:excluded",
        },
    }
}

fn class_label(class: ProjectClass) -> &'static str {
    match class {
        ProjectClass::Workspace => "workspace",
        ProjectClass::ManagedCache => "managed_cache",
        ProjectClass::ContainerStorage => "container_storage",
    }
}

fn daemon(config_path: Option<PathBuf>, state_dir: Option<PathBuf>) -> Result<()> {
    let path_set = paths_for(state_dir.as_deref());
    let _lock = lockfile::try_acquire(&path_set.lock_path).context("daemon already running")?;
    let (cfg, config_source) = load_config_with_source(config_path)?;
    let policy = build_policy(&cfg, &config_source)?;
    let logger = Logger::new(&path_set.log_path)?;
    logger.info("daemon starting");
    let cargo = resolve_cargo_bin(&default_cargo_candidates())?;
    let store = open_store_at(&path_set)?;
    let daemon = daemon_for_clean(&store, &cfg, cargo, &policy)
        .with_logger(logger)
        .with_cycle_factory(Arc::new(ConfigCycleFactory { config_source }));
    daemon.run_forever()
}

struct ConfigCycleFactory {
    config_source: PathBuf,
}

impl DaemonCycleFactory for ConfigCycleFactory {
    fn snapshot(&self) -> Result<DaemonCycleSnapshot> {
        let cfg = load(&self.config_source)?;
        cfg.validate()?;
        let policy = build_policy(&cfg, &self.config_source)?;
        Ok(DaemonCycleSnapshot::new(
            scanner_for(&cfg, &policy),
            DaemonOptions {
                clean_interval: cfg.clean_interval,
                scan_interval: cfg.scan_interval,
                target_quiet_period: cfg.target_quiet_period,
            },
        ))
    }
}

fn stats(
    state_dir: Option<PathBuf>,
    since: Option<String>,
    top: usize,
    json: bool,
) -> Result<CommandOutcome> {
    let since_time = match since {
        Some(value) => SystemTime::now() - parse_since(&value)?,
        None => SystemTime::UNIX_EPOCH,
    };
    let store = open_store(state_dir.as_deref())?;
    let total = store.total_bytes_recovered(since_time)?;
    let top_projects = store.top_projects_by_bytes(since_time, top)?;
    let failed_clean_attempts = store.failed_clean_attempts(since_time)?;
    let status = CommandStatus::complete();
    let report = CommandReport::new(
        "stats",
        &status,
        None,
        None,
        None,
        Vec::new(),
        serde_json::json!({
            "total_bytes": total,
            "top_projects": top_projects,
            "failed_clean_attempts": failed_clean_attempts,
        }),
    );
    if json {
        print_json(&report)?;
    } else {
        println!("Bytes recovered: {}", report.data["total_bytes"]);
        println!(
            "Failed clean attempts: {}",
            report.data["failed_clean_attempts"]
        );
        for (idx, project) in report.data["top_projects"]
            .as_array()
            .into_iter()
            .flatten()
            .enumerate()
        {
            println!(
                "  {}. {} - {} bytes",
                idx + 1,
                project["path"].as_str().unwrap_or_default(),
                project["bytes"].as_i64().unwrap_or_default()
            );
        }
        print_text_outcome(&report);
    }
    Ok(status.outcome())
}

fn logs(
    state_dir: Option<PathBuf>,
    errors_only: bool,
    tail: usize,
    json: bool,
) -> Result<CommandOutcome> {
    let path_set = paths_for(state_dir.as_deref());
    let status = CommandStatus::complete();
    if errors_only {
        let store = open_store_at(&path_set)?;
        let since = SystemTime::now() - Duration::from_secs(7 * 24 * 60 * 60);
        let errors = store.errors_since(since)?;
        let data = serde_json::json!({
            "errors": errors.iter().map(|error| {
                serde_json::json!({
                    "category": error.category,
                    "path": error.path,
                    "message": error.message,
                })
            }).collect::<Vec<_>>(),
        });
        let report = CommandReport::new("logs", &status, None, None, None, Vec::new(), data);
        if json {
            print_json(&report)?;
        } else {
            for error in errors {
                println!("[{}] {:?}: {}", error.category, error.path, error.message);
            }
            print_text_outcome(&report);
        }
        return Ok(status.outcome());
    }
    let lines = tail_file_lines(&path_set.log_path, tail)?;
    let report = CommandReport::new(
        "logs",
        &status,
        None,
        None,
        None,
        Vec::new(),
        serde_json::json!({"lines": lines}),
    );
    if json {
        print_json(&report)?;
    } else {
        for line in report.data["lines"].as_array().into_iter().flatten() {
            println!("{}", line.as_str().unwrap_or_default());
        }
        print_text_outcome(&report);
    }
    Ok(status.outcome())
}

fn load_config(config_path: Option<PathBuf>) -> Result<Config> {
    load_config_with_source(config_path).map(|(config, _)| config)
}

fn load_config_with_source(config_path: Option<PathBuf>) -> Result<(Config, PathBuf)> {
    let path = config_path.unwrap_or_else(default_path);
    let cfg = load(&path)?;
    cfg.validate()?;
    if cfg.warnings().contains(&ConfigWarning::LegacyExcludes) {
        eprintln!(
            "warning: `excludes` is deprecated in v0.4; run `car-go-clean config migrate` to rename it to `override_excludes` before v0.5"
        );
    }
    Ok((cfg, path))
}

fn build_policy(cfg: &Config, config_source: &Path) -> Result<ScopePolicy> {
    ScopePolicy::build(cfg, config_source, &ProcessEnvironment)
}

fn open_store(state_dir: Option<&Path>) -> Result<Store> {
    open_store_at(&paths_for(state_dir))
}

fn open_store_at(path_set: &PathSet) -> Result<Store> {
    let store = Store::open(&path_set.db_path)?;
    store.migrate()?;
    Ok(store)
}

fn paths_for(state_dir: Option<&Path>) -> PathSet {
    let mut path_set = paths();
    if let Some(state_dir) = state_dir {
        path_set.state_dir = state_dir.to_path_buf();
        path_set.db_path = state_dir.join("state.db");
        path_set.log_path = state_dir.join("car-go-clean.log");
        path_set.lock_path = state_dir.join("daemon.lock");
    }
    path_set
}

fn daemon_for_scan<'a>(
    store: &'a Store,
    cfg: &Config,
    policy: &ScopePolicy,
) -> Daemon<'a, RealRunner> {
    Daemon::new(
        store,
        Cache::new(store),
        scanner_for(cfg, policy),
        Cleaner::new("cargo", RealRunner, cfg.clean_interval),
        DaemonOptions {
            clean_interval: cfg.clean_interval,
            scan_interval: cfg.scan_interval,
            target_quiet_period: cfg.target_quiet_period,
        },
    )
}

fn reconcile_review_state(store: &Store, cfg: &Config, policy: &ScopePolicy) -> Result<()> {
    daemon_for_scan(store, cfg, policy).reconcile_cached_state()?;
    Ok(())
}

fn daemon_for_clean<'a>(
    store: &'a Store,
    cfg: &Config,
    cargo_bin: PathBuf,
    policy: &ScopePolicy,
) -> Daemon<'a, RealRunner> {
    Daemon::new(
        store,
        Cache::new(store),
        scanner_for(cfg, policy),
        Cleaner::new(cargo_bin, RealRunner, cfg.clean_interval),
        DaemonOptions {
            clean_interval: cfg.clean_interval,
            scan_interval: cfg.scan_interval,
            target_quiet_period: cfg.target_quiet_period,
        },
    )
}

fn scanner_for(cfg: &Config, policy: &ScopePolicy) -> Scanner {
    Scanner::new(ScannerOptions {
        roots: cfg.scan_dirs.clone(),
        project_dirs: cfg.project_dirs.clone(),
        excludes: cfg.effective_excludes(),
    })
    .with_authority(policy.clone(), Arc::new(SystemIdentityProvider))
}

fn parse_since(value: &str) -> Result<Duration> {
    if let Some(days) = value.strip_suffix('d') {
        return Ok(Duration::from_secs(days.parse::<u64>()? * 24 * 60 * 60));
    }
    if let Some(weeks) = value.strip_suffix('w') {
        return Ok(Duration::from_secs(
            weeks.parse::<u64>()? * 7 * 24 * 60 * 60,
        ));
    }
    humantime::parse_duration(value).map_err(Into::into)
}

fn tail_file_lines(path: &Path, n: usize) -> Result<Vec<String>> {
    let file = fs::File::open(path)?;
    let reader = io::BufReader::new(file);
    let mut lines = Vec::new();
    for line in reader.lines() {
        lines.push(line?);
        if lines.len() > n {
            lines.remove(0);
        }
    }
    Ok(lines)
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::os::unix::fs::symlink;

    #[test]
    fn broken_bound_root_maps_to_the_public_incomplete_exit_code() {
        let work = tempfile::tempdir().unwrap();
        let physical_root = work.path().join("physical-root");
        fs::create_dir_all(&physical_root).unwrap();
        let root_alias = work.path().join("root-alias");
        symlink(&physical_root, &root_alias).unwrap();
        let config_path = work.path().join("config.toml");
        fs::write(
            &config_path,
            format!("scan_dirs = [\"{}\"]\n", root_alias.display()),
        )
        .unwrap();
        let (config, config_source) = load_config_with_source(Some(config_path.clone())).unwrap();
        let policy = build_policy(&config, &config_source).unwrap();
        fs::remove_file(&root_alias).unwrap();
        let store = Store::open(work.path().join("state.db")).unwrap();
        store.migrate().unwrap();

        let execution = scan_and_report(&store, &config, &policy, false, true).unwrap();
        let outcome = execution.status.outcome();

        assert_eq!(outcome, CommandOutcome::Incomplete);
        assert_eq!(outcome.code(), 2);
    }
}
