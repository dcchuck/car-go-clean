//! User-service management for the background cleanup daemon.
//!
//! The service definitions are embedded in the binary so an installed
//! `car-go-clean` never needs to find files from a source checkout.

use anyhow::{anyhow, bail, Context, Result};
use std::env;
use std::ffi::{OsStr, OsString};
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};

const LABEL: &str = "com.dcchuck.car-go-clean";
const UNIT: &str = "car-go-clean.service";
const LAUNCHD_TEMPLATE: &str = include_str!("../packaging/launchd/com.dcchuck.car-go-clean.plist");
const SYSTEMD_TEMPLATE: &str = include_str!("../packaging/systemd/car-go-clean.service");

static TEMP_FILE_COUNTER: AtomicU64 = AtomicU64::new(0);

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ServiceAction {
    Install,
    Status,
    Restart,
    Uninstall,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ServicePlatform {
    MacOs,
    Linux,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ServiceStatus {
    pub installed: bool,
    pub active: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CommandOutput {
    pub success: bool,
    pub stdout: String,
    pub stderr: String,
}

impl CommandOutput {
    pub fn new(success: bool, stdout: String, stderr: String) -> Self {
        Self {
            success,
            stdout,
            stderr,
        }
    }
}

pub trait CommandRunner {
    fn run(&mut self, program: &Path, args: &[OsString]) -> Result<CommandOutput>;
}

#[derive(Default)]
pub struct SystemCommandRunner;

impl CommandRunner for SystemCommandRunner {
    fn run(&mut self, program: &Path, args: &[OsString]) -> Result<CommandOutput> {
        let output = Command::new(program)
            .args(args)
            .output()
            .with_context(|| format!("could not run {}", program.display()))?;
        Ok(CommandOutput::new(
            output.status.success(),
            String::from_utf8_lossy(&output.stdout).into_owned(),
            String::from_utf8_lossy(&output.stderr).into_owned(),
        ))
    }
}

pub struct ServiceManager<R: CommandRunner> {
    platform: ServicePlatform,
    home_dir: PathBuf,
    binary: PathBuf,
    runner: R,
}

impl<R: CommandRunner> ServiceManager<R> {
    pub fn new(platform: ServicePlatform, home_dir: PathBuf, binary: PathBuf, runner: R) -> Self {
        Self {
            platform,
            home_dir,
            binary,
            runner,
        }
    }

    pub fn install(&mut self) -> Result<ServiceStatus> {
        self.require_absolute_binary()?;
        match self.platform {
            ServicePlatform::MacOs => self.install_macos(),
            ServicePlatform::Linux => self.install_linux(),
        }
    }

    pub fn status(&mut self) -> Result<ServiceStatus> {
        match self.platform {
            ServicePlatform::MacOs => self.status_macos(),
            ServicePlatform::Linux => self.status_linux(),
        }
    }

    pub fn restart(&mut self) -> Result<ServiceStatus> {
        match self.platform {
            ServicePlatform::MacOs => {
                self.run_checked(
                    Path::new("launchctl"),
                    &[
                        OsString::from("kickstart"),
                        OsString::from("-k"),
                        OsString::from(self.launchd_service_target()),
                    ],
                )?;
            }
            ServicePlatform::Linux => {
                self.require_systemd_user()?;
                self.run_checked(
                    Path::new("systemctl"),
                    &[
                        OsString::from("--user"),
                        OsString::from("restart"),
                        OsString::from(UNIT),
                    ],
                )?;
            }
        }
        Ok(ServiceStatus {
            installed: true,
            active: true,
        })
    }

    pub fn uninstall(&mut self) -> Result<ServiceStatus> {
        match self.platform {
            ServicePlatform::MacOs => {
                let plist = self.launchd_plist_path();
                let bootout_args = [
                    OsString::from("bootout"),
                    OsString::from(self.launchd_domain()),
                    plist.as_os_str().to_os_string(),
                ];
                let bootout_output = self.run(Path::new("launchctl"), &bootout_args)?;
                if !bootout_output.success && !is_missing_launchd_service(&bootout_output) {
                    return Err(anyhow!(
                        "{} failed{}",
                        command_description(Path::new("launchctl"), &bootout_args),
                        format_command_error(&bootout_output)
                    ));
                }
                if plist.exists() {
                    fs::remove_file(&plist)
                        .with_context(|| format!("could not remove {}", plist.display()))?;
                }
            }
            ServicePlatform::Linux => {
                self.require_systemd_user()?;
                let disable_output = self.run(
                    Path::new("systemctl"),
                    &[
                        OsString::from("--user"),
                        OsString::from("disable"),
                        OsString::from("--now"),
                        OsString::from(UNIT),
                    ],
                )?;
                if !disable_output.success && !is_missing_systemd_unit(&disable_output) {
                    return Err(anyhow!(
                        "{} failed{}",
                        command_description(
                            Path::new("systemctl"),
                            &[
                                OsString::from("--user"),
                                OsString::from("disable"),
                                OsString::from("--now"),
                                OsString::from(UNIT),
                            ]
                        ),
                        format_command_error(&disable_output)
                    ));
                }
                let unit = self.systemd_unit_path();
                if unit.exists() {
                    fs::remove_file(&unit)
                        .with_context(|| format!("could not remove {}", unit.display()))?;
                }
                self.run_checked(
                    Path::new("systemctl"),
                    &[OsString::from("--user"), OsString::from("daemon-reload")],
                )?;
            }
        }
        Ok(ServiceStatus {
            installed: false,
            active: false,
        })
    }

    pub fn into_runner(self) -> R {
        self.runner
    }

    fn install_macos(&mut self) -> Result<ServiceStatus> {
        let plist = self.launchd_plist_path();
        let log_dir = self.home_dir.join("Library/Logs/car-go-clean");
        fs::create_dir_all(&log_dir)
            .with_context(|| format!("could not create {}", log_dir.display()))?;
        let rendered = render_launchd_template(&self.binary, &log_dir)?;
        atomic_write(&plist, rendered.as_bytes())?;

        self.run_allow_failure(
            Path::new("launchctl"),
            &[
                OsString::from("bootout"),
                OsString::from(self.launchd_domain()),
                plist.as_os_str().to_os_string(),
            ],
        )?;
        self.run_checked(
            Path::new("launchctl"),
            &[
                OsString::from("bootstrap"),
                OsString::from(self.launchd_domain()),
                plist.as_os_str().to_os_string(),
            ],
        )?;
        self.run_checked(
            Path::new("launchctl"),
            &[
                OsString::from("kickstart"),
                OsString::from("-k"),
                OsString::from(self.launchd_service_target()),
            ],
        )?;
        Ok(ServiceStatus {
            installed: true,
            active: true,
        })
    }

    fn install_linux(&mut self) -> Result<ServiceStatus> {
        self.require_systemd_user()?;
        let unit = self.systemd_unit_path();
        let rendered = render_systemd_template(&self.binary)?;
        atomic_write(&unit, rendered.as_bytes())?;
        self.run_checked(
            Path::new("systemctl"),
            &[OsString::from("--user"), OsString::from("daemon-reload")],
        )?;
        self.run_checked(
            Path::new("systemctl"),
            &[
                OsString::from("--user"),
                OsString::from("enable"),
                OsString::from("--now"),
                OsString::from(UNIT),
            ],
        )?;
        Ok(ServiceStatus {
            installed: true,
            active: true,
        })
    }

    fn status_macos(&mut self) -> Result<ServiceStatus> {
        if !self.launchd_plist_path().exists() {
            return Ok(ServiceStatus {
                installed: false,
                active: false,
            });
        }
        let output = self.run(
            Path::new("launchctl"),
            &[
                OsString::from("print"),
                OsString::from(self.launchd_service_target()),
            ],
        )?;
        Ok(ServiceStatus {
            installed: true,
            active: output.success,
        })
    }

    fn status_linux(&mut self) -> Result<ServiceStatus> {
        self.require_systemd_user()?;
        if !self.systemd_unit_path().exists() {
            return Ok(ServiceStatus {
                installed: false,
                active: false,
            });
        }
        let output = self.run(
            Path::new("systemctl"),
            &[
                OsString::from("--user"),
                OsString::from("status"),
                OsString::from("--no-pager"),
                OsString::from(UNIT),
            ],
        )?;
        Ok(ServiceStatus {
            installed: true,
            active: output.success,
        })
    }

    fn require_absolute_binary(&self) -> Result<()> {
        if self.binary.is_absolute() {
            Ok(())
        } else {
            bail!(
                "service binary must be an absolute path: {}",
                self.binary.display()
            )
        }
    }

    fn require_systemd_user(&mut self) -> Result<()> {
        let output = self.run(
            Path::new("systemctl"),
            &[OsString::from("--user"), OsString::from("show-environment")],
        )?;
        if output.success {
            Ok(())
        } else {
            bail!(
                "systemd --user is unavailable{}",
                format_command_error(&output)
            )
        }
    }

    fn launchd_domain(&self) -> String {
        format!("gui/{}", unsafe { libc::geteuid() })
    }

    fn launchd_service_target(&self) -> String {
        format!("{}/{}", self.launchd_domain(), LABEL)
    }

    fn launchd_plist_path(&self) -> PathBuf {
        self.home_dir
            .join("Library/LaunchAgents")
            .join(format!("{LABEL}.plist"))
    }

    fn systemd_unit_path(&self) -> PathBuf {
        self.home_dir.join(".config/systemd/user").join(UNIT)
    }

    fn run(&mut self, program: &Path, args: &[OsString]) -> Result<CommandOutput> {
        self.runner.run(program, args)
    }

    fn run_checked(&mut self, program: &Path, args: &[OsString]) -> Result<()> {
        let output = self.run(program, args)?;
        if output.success {
            Ok(())
        } else {
            Err(anyhow!(
                "{} failed{}",
                command_description(program, args),
                format_command_error(&output)
            ))
        }
    }

    fn run_allow_failure(&mut self, program: &Path, args: &[OsString]) -> Result<()> {
        self.run(program, args).map(|_| ())
    }
}

pub fn resolve_service_binary(
    argv0: &OsStr,
    path: Option<&OsStr>,
    current_exe: PathBuf,
) -> Result<PathBuf> {
    let argv0_path = Path::new(argv0);
    if argv0_path.is_absolute() {
        if is_executable_file(argv0_path) {
            return Ok(argv0_path.to_path_buf());
        }
    } else if argv0_path.components().count() > 1 {
        let direct_candidate = env::current_dir()
            .context("could not determine the current directory for service binary resolution")?
            .join(argv0_path);
        if is_executable_file(&direct_candidate) {
            return Ok(direct_candidate);
        }
    }

    if !argv0_path.is_absolute() && argv0_path.components().count() == 1 {
        if let Some(path) = path {
            for directory in env::split_paths(path) {
                let directory = if directory.is_absolute() {
                    directory
                } else {
                    env::current_dir()
                        .context("could not determine the current directory for PATH resolution")?
                        .join(directory)
                };
                let candidate = directory.join(argv0_path);
                if is_executable_file(&candidate) {
                    return Ok(candidate);
                }
            }
        }
    }

    if current_exe.is_absolute() && is_executable_file(&current_exe) {
        return Ok(current_exe);
    }
    Err(anyhow!(
        "could not resolve an executable absolute service binary from {:?} or current executable {}",
        argv0,
        current_exe.display()
    ))
}

fn render_launchd_template(binary: &Path, log_dir: &Path) -> Result<String> {
    require_absolute(binary)?;
    require_absolute(log_dir)?;
    Ok(LAUNCHD_TEMPLATE
        .replace(
            "__CAR_GO_CLEAN_BIN__",
            &xml_escape(&binary.display().to_string()),
        )
        .replace(
            "__CAR_GO_CLEAN_LOG_DIR__",
            &xml_escape(&log_dir.display().to_string()),
        ))
}

fn render_systemd_template(binary: &Path) -> Result<String> {
    require_absolute(binary)?;
    Ok(SYSTEMD_TEMPLATE.replace(
        "__CAR_GO_CLEAN_BIN__",
        &systemd_quote(&binary.display().to_string()),
    ))
}

fn require_absolute(path: &Path) -> Result<()> {
    if path.is_absolute() {
        Ok(())
    } else {
        bail!("path must be absolute: {}", path.display())
    }
}

fn xml_escape(value: &str) -> String {
    let mut escaped = String::with_capacity(value.len());
    for character in value.chars() {
        match character {
            '&' => escaped.push_str("&amp;"),
            '<' => escaped.push_str("&lt;"),
            '>' => escaped.push_str("&gt;"),
            '"' => escaped.push_str("&quot;"),
            '\'' => escaped.push_str("&apos;"),
            _ => escaped.push(character),
        }
    }
    escaped
}

fn systemd_quote(value: &str) -> String {
    format!(
        "\"{}\"",
        value
            .replace('\\', "\\\\")
            .replace('"', "\\\"")
            .replace('%', "%%")
    )
}

fn atomic_write(path: &Path, contents: &[u8]) -> Result<()> {
    let parent = path
        .parent()
        .ok_or_else(|| anyhow!("{} has no parent directory", path.display()))?;
    fs::create_dir_all(parent).with_context(|| format!("could not create {}", parent.display()))?;

    let file_name = path
        .file_name()
        .ok_or_else(|| anyhow!("{} has no file name", path.display()))?;
    let temporary = parent.join(format!(
        ".{}.{}.{}",
        file_name.to_string_lossy(),
        std::process::id(),
        TEMP_FILE_COUNTER.fetch_add(1, Ordering::Relaxed)
    ));
    let result = (|| -> Result<()> {
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temporary)
            .with_context(|| format!("could not create {}", temporary.display()))?;
        file.write_all(contents)
            .with_context(|| format!("could not write {}", temporary.display()))?;
        file.sync_all()
            .with_context(|| format!("could not sync {}", temporary.display()))?;
        fs::rename(&temporary, path).with_context(|| {
            format!(
                "could not atomically replace {} with {}",
                path.display(),
                temporary.display()
            )
        })?;
        Ok(())
    })();
    if result.is_err() && temporary.exists() {
        let _ = fs::remove_file(&temporary);
    }
    result
}

fn is_executable_file(path: &Path) -> bool {
    path.is_file() && is_executable(path)
}

#[cfg(unix)]
fn is_executable(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;

    fs::metadata(path)
        .map(|metadata| metadata.permissions().mode() & 0o111 != 0)
        .unwrap_or(false)
}

#[cfg(not(unix))]
fn is_executable(_path: &Path) -> bool {
    true
}

fn command_description(program: &Path, args: &[OsString]) -> String {
    std::iter::once(program.as_os_str())
        .chain(args.iter().map(OsString::as_os_str))
        .map(|value| value.to_string_lossy())
        .collect::<Vec<_>>()
        .join(" ")
}

fn format_command_error(output: &CommandOutput) -> String {
    if output.stderr.trim().is_empty() {
        String::new()
    } else {
        format!(": {}", output.stderr.trim())
    }
}

fn is_missing_systemd_unit(output: &CommandOutput) -> bool {
    let message = format!("{}\n{}", output.stdout, output.stderr).to_ascii_lowercase();
    message.contains(&UNIT.to_ascii_lowercase())
        && ["not found", "not loaded", "does not exist"]
            .iter()
            .any(|marker| message.contains(marker))
}

fn is_missing_launchd_service(output: &CommandOutput) -> bool {
    let message = format!("{}\n{}", output.stdout, output.stderr).to_ascii_lowercase();
    [
        "no such process",
        "could not find specified service",
        "service not found",
        "not loaded",
    ]
    .iter()
    .any(|marker| message.contains(marker))
}
