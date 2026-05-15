use crate::activity::ProcessInspector;
use crate::cache::Cache;
use crate::cleaner::{default_cargo_candidates, resolve_cargo_bin, Cleaner, RealRunner};
use crate::config::{default_path, load, paths, Config, PathSet};
use crate::daemon::{Daemon, DaemonOptions};
use crate::lockfile;
use crate::logging::Logger;
use crate::safety::{
    review_project, review_summary, CleanDecision, ProjectClass, ProjectReview, SafetyOptions,
    SkipReason,
};
use crate::scanner::{Scanner, ScannerOptions};
use crate::store::{ErrorRecord, Store};
use anyhow::{anyhow, Context, Result};
use clap::{Parser, Subcommand};
use std::fs;
use std::io::{self, BufRead};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

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
    },
    Scan {
        #[arg(long)]
        config: Option<PathBuf>,
        #[arg(long)]
        state_dir: Option<PathBuf>,
    },
    Run {
        #[arg(long)]
        config: Option<PathBuf>,
        #[arg(long)]
        state_dir: Option<PathBuf>,
        #[arg(long)]
        dry_run: bool,
        #[arg(long)]
        include_managed_cache: bool,
        #[arg(long)]
        include_active: bool,
        #[arg(long)]
        force: bool,
    },
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

pub fn run() -> Result<()> {
    execute(Cli::parse())
}

fn execute(cli: Cli) -> Result<()> {
    match cli.command {
        Commands::Version => {
            println!("{}", env!("CARGO_PKG_VERSION"));
            Ok(())
        }
        Commands::Health {
            config,
            state_dir,
            skip_cargo,
        } => health(config, state_dir, skip_cargo),
        Commands::Config { config } => {
            let cfg = load_config(config)?;
            print!("{}", toml::to_string_pretty(&cfg)?);
            Ok(())
        }
        Commands::Status { config, state_dir } => status(config, state_dir),
        Commands::Projects {
            config,
            state_dir,
            risky,
            active,
            json,
        } => projects(config, state_dir, risky, active, json),
        Commands::Scan { config, state_dir } => scan(config, state_dir),
        Commands::Run {
            config,
            state_dir,
            dry_run,
            include_managed_cache,
            include_active,
            force,
        } => run_once(
            config,
            state_dir,
            dry_run,
            include_managed_cache,
            include_active,
            force,
        ),
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
    if !errors.is_empty() {
        println!("WARN: {} errors in last 24h", errors.len());
    }
    Ok(())
}

fn status(config_path: Option<PathBuf>, state_dir: Option<PathBuf>) -> Result<()> {
    let cfg = load_config(config_path)?;
    let store = open_store(state_dir.as_deref())?;
    Cache::new(&store).sync_on_disk()?;
    let safety = SafetyOptions {
        target_quiet_period: cfg.target_quiet_period,
        include_managed_cache: false,
        include_active: false,
        force: false,
    };
    let reviews = project_reviews(&store, &safety, cfg.scan_interval)?;
    let projects = store.all_projects()?;
    let total = store.total_bytes_recovered(SystemTime::UNIX_EPOCH)?;
    print_review_summary("Status", &reviews);
    println!("Cached projects: {}", projects.len());
    println!("Total bytes recovered (all time): {total}");
    match store.last_run() {
        Ok(run) => println!(
            "Last run: id={} cleaned={} recovered={} errors={}",
            run.id, run.projects_cleaned, run.bytes_recovered, run.errors_count
        ),
        Err(_) => println!("Last run: <none>"),
    }
    Ok(())
}

fn projects(
    config_path: Option<PathBuf>,
    state_dir: Option<PathBuf>,
    risky: bool,
    active: bool,
    json: bool,
) -> Result<()> {
    let cfg = load_config(config_path)?;
    let store = open_store(state_dir.as_deref())?;
    Cache::new(&store).sync_on_disk()?;
    let safety = SafetyOptions {
        target_quiet_period: cfg.target_quiet_period,
        include_managed_cache: risky,
        include_active: active,
        force: false,
    };
    let reviews = project_reviews(&store, &safety, cfg.scan_interval)?;

    if json {
        println!("{}", serde_json::to_string_pretty(&reviews)?);
        return Ok(());
    }

    for review in &reviews {
        println!(
            "{}\t{}\t{}\t{}",
            decision_label(&review.decision),
            class_label(review.class),
            review.target_bytes,
            review.path.display()
        );
    }
    Ok(())
}

fn scan(config_path: Option<PathBuf>, state_dir: Option<PathBuf>) -> Result<()> {
    let path_set = paths_for(state_dir.as_deref());
    let _lock = lockfile::try_acquire(&path_set.lock_path)
        .context("another car-go-clean process is running")?;
    let cfg = load_config(config_path)?;
    let store = open_store_at(&path_set)?;
    let daemon = daemon_for_scan(&store, &cfg);
    daemon.scan_cycle()?;
    println!("Scan complete");
    Ok(())
}

fn run_once(
    config_path: Option<PathBuf>,
    state_dir: Option<PathBuf>,
    dry_run: bool,
    include_managed_cache: bool,
    include_active: bool,
    force: bool,
) -> Result<()> {
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

    if dry_run {
        Cache::new(&store).sync_on_disk()?;
        let reviews = project_reviews(&store, &safety, cfg.scan_interval)?;
        print_review_summary("Dry run", &reviews);
        print_cleanable_targets(&reviews);
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
    let activity = crate::activity::SysinfoProcessInspector.active_projects(&paths)?;

    let reviews = projects
        .iter()
        .map(|project| {
            review_project(
                Path::new(&project.path),
                &scan_errors,
                &activity,
                now,
                safety,
            )
        })
        .collect::<Result<Vec<_>>>()?;
    record_review_diagnostics(store, &reviews)?;
    Ok(reviews)
}

fn print_review_summary(label: &str, reviews: &[ProjectReview]) {
    let summary = review_summary(reviews);
    println!("{label}");
    println!("Total projects: {}", summary.total_projects);
    println!("Cleanable projects: {}", summary.cleanable_projects);
    println!("Skipped projects: {}", summary.skipped_projects);
    println!("Cleanable bytes: {}", summary.cleanable_bytes);
}

fn print_cleanable_targets(reviews: &[ProjectReview]) {
    if !reviews
        .iter()
        .any(|review| review.decision == CleanDecision::Cleanable)
    {
        return;
    }

    println!("Cleanable targets:");
    for review in reviews
        .iter()
        .filter(|review| review.decision == CleanDecision::Cleanable)
    {
        println!(
            "  {}\t{}\t{}",
            review.target_bytes,
            review.target_path.display(),
            review.path.display()
        );
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
                path: Some(review.target_path.to_string_lossy().into_owned()),
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
    let daemon = daemon_for_clean(&store, &cfg, cargo);
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
        excludes: cfg.excludes.clone(),
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
