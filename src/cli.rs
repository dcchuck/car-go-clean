use crate::activity::ProcessInspector;
use crate::cache::Cache;
use crate::cleaner::{default_cargo_candidates, resolve_cargo_bin, Cleaner, RealRunner};
use crate::config::{default_path, load, paths, Config, ConfigWarning, PathSet};
use crate::daemon::{Daemon, DaemonOptions};
use crate::lockfile;
use crate::logging::Logger;
use crate::safety::{
    review_project_with_discovery_blocks, review_summary, CleanDecision, ProjectClass,
    ProjectReview, SafetyOptions, SkipReason,
};
use crate::scanner::{Scanner, ScannerOptions};
use crate::service::{
    resolve_service_binary, ServiceAction, ServiceManager, ServicePlatform, SystemCommandRunner,
};
use crate::store::{ErrorRecord, Store};
use anyhow::{anyhow, bail, Context, Result};
use clap::{Parser, Subcommand};
use std::fs;
use std::io::{self, BufRead, IsTerminal};
use std::path::{Path, PathBuf};
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
    },
    Config {
        #[arg(long)]
        config: Option<PathBuf>,
    },
    Status {
        #[arg(long)]
        config: Option<PathBuf>,
        #[arg(long)]
        state_dir: Option<PathBuf>,
        #[arg(long)]
        refresh: bool,
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
        #[arg(long)]
        all: bool,
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

pub fn run() -> Result<()> {
    execute(Cli::parse())
}

fn execute(cli: Cli) -> Result<()> {
    match cli.command {
        Commands::Version => {
            println!("{}", env!("CARGO_PKG_VERSION"));
            Ok(())
        }
        Commands::Service { command } => service(command),
        Commands::Health {
            config,
            state_dir,
            skip_cargo,
        } => health(config, state_dir, skip_cargo),
        Commands::Config { config } => {
            let cfg = load_config(config)?;
            print!("{}", cfg.to_toml()?);
            Ok(())
        }
        Commands::Status {
            config,
            state_dir,
            refresh,
        } => status(config, state_dir, refresh),
        Commands::Projects {
            config,
            state_dir,
            risky,
            active,
            json,
            all,
        } => projects(config, state_dir, risky, active, json, all),
        Commands::Scan { config, state_dir } => scan(config, state_dir),
        Commands::Run {
            config,
            state_dir,
            dry_run,
            no_scan,
            include_managed_cache,
            include_active,
            force,
            all,
        } => run_once(RunOptions {
            config_path: config,
            state_dir,
            dry_run,
            no_scan,
            include_managed_cache,
            include_active,
            force,
            all,
        }),
        Commands::Daemon { config, state_dir } => daemon(config, state_dir),
        Commands::Stats {
            since,
            top,
            json,
            state_dir,
        } => stats(state_dir, since, top, json),
        Commands::Logs {
            errors_only,
            tail,
            state_dir,
        } => logs(state_dir, errors_only, tail),
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
    let state = if !status.installed {
        "not installed"
    } else if status.active {
        "running"
    } else {
        "stopped"
    };

    println!("Service");
    print_row("Platform", platform_label);
    print_row("Binary", binary.display().to_string());
    print_row("Definition", definition.display().to_string());
    print_row("State", state);
    Ok(())
}

fn health(
    config_path: Option<PathBuf>,
    state_dir: Option<PathBuf>,
    skip_cargo: bool,
) -> Result<()> {
    let cfg = load_config(config_path)?;
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
    println!("OK");
    if cfg.warnings().contains(&ConfigWarning::LegacyExcludes) {
        println!("WARN: legacy `excludes` is deprecated; run `car-go-clean config migrate`");
    }
    if !errors.is_empty() {
        println!("WARN: {} errors in last 24h", errors.len());
    }
    Ok(())
}

fn status(config_path: Option<PathBuf>, state_dir: Option<PathBuf>, refresh: bool) -> Result<()> {
    let cfg = load_config(config_path)?;
    let store = open_store(state_dir.as_deref())?;
    if refresh {
        reconcile_review_state(&store, &cfg)?;
        let safety = SafetyOptions {
            target_quiet_period: cfg.target_quiet_period,
            include_managed_cache: false,
            include_active: false,
            force: false,
        };
        project_reviews(&store, &safety, cfg.scan_interval, "status --refresh")?;
    }

    let cached_projects = store.project_count()?;
    let total = store.total_bytes_recovered(SystemTime::UNIX_EPOCH)?;

    print_heading("Status");
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
    Ok(())
}

fn projects(
    config_path: Option<PathBuf>,
    state_dir: Option<PathBuf>,
    risky: bool,
    active: bool,
    json: bool,
    all: bool,
) -> Result<()> {
    let cfg = load_config(config_path)?;
    let store = open_store(state_dir.as_deref())?;
    reconcile_review_state(&store, &cfg)?;
    let safety = SafetyOptions {
        target_quiet_period: cfg.target_quiet_period,
        include_managed_cache: risky,
        include_active: active,
        force: false,
    };
    let reviews = project_reviews(&store, &safety, cfg.scan_interval, "projects")?;

    if json {
        println!("{}", serde_json::to_string_pretty(&reviews)?);
        return Ok(());
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
    Ok(())
}

fn scan(config_path: Option<PathBuf>, state_dir: Option<PathBuf>) -> Result<()> {
    let path_set = paths_for(state_dir.as_deref());
    let _lock = lockfile::try_acquire(&path_set.lock_path)
        .context("another car-go-clean process is running")?;
    let cfg = load_config(config_path)?;
    let store = open_store_at(&path_set)?;
    scan_and_report(&store, &cfg)
}

fn scan_and_report(store: &Store, cfg: &Config) -> Result<()> {
    daemon_for_scan(store, cfg).scan_cycle()?;
    println!("Scan complete");
    Ok(())
}

fn run_once(options: RunOptions) -> Result<()> {
    let RunOptions {
        config_path,
        state_dir,
        dry_run,
        no_scan,
        include_managed_cache,
        include_active,
        force,
        all,
    } = options;
    let path_set = paths_for(state_dir.as_deref());
    let _lock = lockfile::try_acquire(&path_set.lock_path)
        .context("another car-go-clean process is running")?;
    let cfg = load_config(config_path)?;
    let safety = SafetyOptions {
        target_quiet_period: cfg.target_quiet_period,
        include_managed_cache,
        include_active,
        force,
    };
    let store = open_store_at(&path_set)?;

    if !no_scan {
        scan_and_report(&store, &cfg)?;
    }

    if dry_run {
        reconcile_review_state(&store, &cfg)?;
        let reviews = project_reviews(&store, &safety, cfg.scan_interval, "dry-run")?;
        print_review_summary("Dry run", &reviews);
        print_skip_breakdown(&review_summary(&reviews));
        print_cleanable_target_preview(&reviews, DEFAULT_PREVIEW_LIMIT, all);
        return Ok(());
    }

    let cargo = resolve_cargo_bin(&default_cargo_candidates())?;
    let daemon = daemon_for_clean(&store, &cfg, cargo);
    let result = daemon.run_cycle_with_safety(safety, &crate::activity::SysinfoProcessInspector)?;
    println!(
        "Run complete: cleaned={} skipped={} recovered={} errors={}",
        result.cleaned, result.skipped, result.bytes_recovered, result.errors
    );
    Ok(())
}

fn project_reviews(
    store: &Store,
    safety: &SafetyOptions,
    scan_interval: Duration,
    source: &str,
) -> Result<Vec<ProjectReview>> {
    let now = SystemTime::now();
    let projects = store.all_projects()?;
    let paths: Vec<PathBuf> = projects
        .iter()
        .map(|project| PathBuf::from(&project.path))
        .collect();
    let scan_error_since = now
        .checked_sub(scan_interval)
        .unwrap_or(SystemTime::UNIX_EPOCH);
    let scan_errors = store.scan_error_paths_since(scan_error_since)?;
    let discovery_blocks = store.blocked_worktree_discovery_paths()?;
    let activity = crate::activity::SysinfoProcessInspector.active_projects(&paths)?;

    let reviews = projects
        .iter()
        .map(|project| {
            review_project_with_discovery_blocks(
                Path::new(&project.path),
                &scan_errors,
                &discovery_blocks,
                &activity,
                now,
                safety,
            )
        })
        .collect::<Result<Vec<_>>>()?;
    record_review_diagnostics(store, &reviews)?;
    store.record_review_status(now, source, &review_summary(&reviews))?;
    Ok(reviews)
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
    let cfg = load_config(config_path)?;
    let logger = Logger::new(&path_set.log_path)?;
    logger.info("daemon starting");
    let cargo = resolve_cargo_bin(&default_cargo_candidates())?;
    let store = open_store_at(&path_set)?;
    let daemon = daemon_for_clean(&store, &cfg, cargo).with_logger(logger);
    daemon.run_forever()
}

fn stats(state_dir: Option<PathBuf>, since: Option<String>, top: usize, json: bool) -> Result<()> {
    let since_time = match since {
        Some(value) => SystemTime::now() - parse_since(&value)?,
        None => SystemTime::UNIX_EPOCH,
    };
    let store = open_store(state_dir.as_deref())?;
    let total = store.total_bytes_recovered(since_time)?;
    let top_projects = store.top_projects_by_bytes(since_time, top)?;
    if json {
        println!(
            "{}",
            serde_json::json!({
                "total_bytes": total,
                "top_projects": top_projects,
            })
        );
    } else {
        println!("Bytes recovered: {total}");
        for (idx, project) in top_projects.iter().enumerate() {
            println!("  {}. {} - {} bytes", idx + 1, project.path, project.bytes);
        }
    }
    Ok(())
}

fn logs(state_dir: Option<PathBuf>, errors_only: bool, tail: usize) -> Result<()> {
    let path_set = paths_for(state_dir.as_deref());
    if errors_only {
        let store = open_store_at(&path_set)?;
        let since = SystemTime::now() - Duration::from_secs(7 * 24 * 60 * 60);
        for error in store.errors_since(since)? {
            println!("[{}] {:?}: {}", error.category, error.path, error.message);
        }
        return Ok(());
    }
    tail_file(&path_set.log_path, tail)
}

fn load_config(config_path: Option<PathBuf>) -> Result<Config> {
    let path = config_path.unwrap_or_else(default_path);
    let cfg = load(path)?;
    cfg.validate()?;
    if cfg.warnings().contains(&ConfigWarning::LegacyExcludes) {
        eprintln!(
            "warning: `excludes` is deprecated in v0.4; run `car-go-clean config migrate` to rename it to `override_excludes` before v0.5"
        );
    }
    Ok(cfg)
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

fn daemon_for_scan<'a>(store: &'a Store, cfg: &Config) -> Daemon<'a, RealRunner> {
    Daemon::new(
        store,
        Cache::new(store),
        scanner_for(cfg),
        Cleaner::new("cargo", RealRunner, cfg.clean_interval),
        DaemonOptions {
            clean_interval: cfg.clean_interval,
            scan_interval: cfg.scan_interval,
            target_quiet_period: cfg.target_quiet_period,
        },
    )
}

fn reconcile_review_state(store: &Store, cfg: &Config) -> Result<()> {
    daemon_for_scan(store, cfg).reconcile_cached_state()?;
    Ok(())
}

fn daemon_for_clean<'a>(
    store: &'a Store,
    cfg: &Config,
    cargo_bin: PathBuf,
) -> Daemon<'a, RealRunner> {
    Daemon::new(
        store,
        Cache::new(store),
        scanner_for(cfg),
        Cleaner::new(cargo_bin, RealRunner, cfg.clean_interval),
        DaemonOptions {
            clean_interval: cfg.clean_interval,
            scan_interval: cfg.scan_interval,
            target_quiet_period: cfg.target_quiet_period,
        },
    )
}

fn scanner_for(cfg: &Config) -> Scanner {
    Scanner::new(ScannerOptions {
        roots: cfg.scan_dirs.clone(),
        project_dirs: cfg.project_dirs.clone(),
        excludes: cfg.effective_excludes(),
    })
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

fn tail_file(path: &Path, n: usize) -> Result<()> {
    let file = fs::File::open(path)?;
    let reader = io::BufReader::new(file);
    let mut lines = Vec::new();
    for line in reader.lines() {
        lines.push(line?);
        if lines.len() > n {
            lines.remove(0);
        }
    }
    for line in lines {
        println!("{line}");
    }
    Ok(())
}
