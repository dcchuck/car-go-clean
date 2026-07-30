//! User-service management for the background cleanup daemon.
//!
//! The service definitions are embedded in the binary so an installed
//! `car-go-clean` never needs to find files from a source checkout.

use crate::policy::{Environment, ProcessEnvironment};
use crate::storage::{protected_roots_for, HostPlatform};
use anyhow::{anyhow, bail, Context, Result};
use std::collections::BTreeMap;
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
const SERVICE_ENVIRONMENT_MARKER: &str = "car-go-clean-service-environment-v1";
const SERVICE_ENVIRONMENT_VARIABLES: &[&str] = &[
    "HOME",
    "CARGO_HOME",
    "RUSTUP_HOME",
    "XDG_CACHE_HOME",
    "XDG_DATA_HOME",
    "GOMODCACHE",
    "BUN_INSTALL",
    "BUN_INSTALL_CACHE_DIR",
    "COLIMA_HOME",
    "LIMA_HOME",
];

static TEMP_FILE_COUNTER: AtomicU64 = AtomicU64::new(0);

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ServiceAction {
    Install,
    Status,
    Start,
    Stop,
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
    pub enabled: bool,
    pub active: bool,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct ServiceEnvironment {
    pub values: BTreeMap<String, OsString>,
}

impl ServiceEnvironment {
    pub fn capture(environment: &dyn Environment) -> Self {
        let values = SERVICE_ENVIRONMENT_VARIABLES
            .iter()
            .filter_map(|name| {
                environment
                    .var_os(name)
                    .map(|value| ((*name).to_string(), value))
            })
            .collect();
        Self { values }
    }
}

impl Environment for ServiceEnvironment {
    fn var_os(&self, name: &str) -> Option<OsString> {
        self.values.get(name).cloned()
    }
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
    service_environment: ServiceEnvironment,
}

impl<R: CommandRunner> ServiceManager<R> {
    pub fn new(platform: ServicePlatform, home_dir: PathBuf, binary: PathBuf, runner: R) -> Self {
        Self::new_with_environment(platform, home_dir, binary, runner, &ProcessEnvironment)
    }

    #[doc(hidden)]
    pub fn new_with_environment(
        platform: ServicePlatform,
        home_dir: PathBuf,
        binary: PathBuf,
        runner: R,
        environment: &dyn Environment,
    ) -> Self {
        Self {
            platform,
            home_dir,
            binary,
            runner,
            service_environment: ServiceEnvironment::capture(environment),
        }
    }

    pub fn install(&mut self) -> Result<ServiceStatus> {
        self.require_absolute_binary()?;
        self.validate_definition_rendering()?;
        self.stop_active_service_for_reinstall()?;
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

    pub fn stop(&mut self) -> Result<ServiceStatus> {
        let status = self.status()?;
        if !status.installed || (!status.enabled && !status.active) {
            return Ok(status);
        }
        match self.platform {
            ServicePlatform::MacOs => {
                if status.enabled {
                    self.run_checked(
                        Path::new("launchctl"),
                        &[
                            OsString::from("disable"),
                            OsString::from(self.launchd_service_target()),
                        ],
                    )?;
                }
                if status.active {
                    let args = [
                        OsString::from("bootout"),
                        OsString::from(self.launchd_domain()),
                        self.launchd_plist_path().into_os_string(),
                    ];
                    let output = self.run(Path::new("launchctl"), &args)?;
                    if !output.success && !is_missing_launchd_service(&output) {
                        return Err(command_failed(Path::new("launchctl"), &args, &output));
                    }
                }
            }
            ServicePlatform::Linux => self.run_checked(
                Path::new("systemctl"),
                &[
                    OsString::from("--user"),
                    OsString::from("disable"),
                    OsString::from("--now"),
                    OsString::from(UNIT),
                ],
            )?,
        }
        Ok(ServiceStatus {
            installed: true,
            enabled: false,
            active: false,
        })
    }

    pub fn start(&mut self) -> Result<ServiceStatus> {
        let status = self.status()?;
        if !status.installed {
            bail!("car-go-clean service is not installed");
        }
        if status.enabled && status.active {
            return Ok(status);
        }
        match self.platform {
            ServicePlatform::MacOs => {
                if !status.enabled {
                    self.run_checked(
                        Path::new("launchctl"),
                        &[
                            OsString::from("enable"),
                            OsString::from(self.launchd_service_target()),
                        ],
                    )?;
                }
                if !status.active {
                    self.run_checked(
                        Path::new("launchctl"),
                        &[
                            OsString::from("bootstrap"),
                            OsString::from(self.launchd_domain()),
                            self.launchd_plist_path().into_os_string(),
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
                }
            }
            ServicePlatform::Linux => {
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
            }
        }
        Ok(ServiceStatus {
            installed: true,
            enabled: true,
            active: true,
        })
    }

    pub fn restart(&mut self) -> Result<ServiceStatus> {
        let status = self.status()?;
        if !status.installed {
            bail!("car-go-clean service is not installed");
        }
        if !status.enabled {
            bail!("car-go-clean service is not enabled");
        }
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
            enabled: true,
            active: true,
        })
    }

    pub fn uninstall(&mut self) -> Result<ServiceStatus> {
        let status = self.status()?;
        if !status.installed {
            return Ok(status);
        }
        match self.platform {
            ServicePlatform::MacOs => {
                if status.enabled {
                    self.run_checked(
                        Path::new("launchctl"),
                        &[
                            OsString::from("disable"),
                            OsString::from(self.launchd_service_target()),
                        ],
                    )?;
                }
                if status.active {
                    let args = [
                        OsString::from("bootout"),
                        OsString::from(self.launchd_domain()),
                        self.launchd_plist_path().into_os_string(),
                    ];
                    let output = self.run(Path::new("launchctl"), &args)?;
                    if !output.success && !is_missing_launchd_service(&output) {
                        return Err(command_failed(Path::new("launchctl"), &args, &output));
                    }
                }
                let plist = self.launchd_plist_path();
                fs::remove_file(&plist)
                    .with_context(|| format!("could not remove {}", plist.display()))?;
            }
            ServicePlatform::Linux => {
                let args = [
                    OsString::from("--user"),
                    OsString::from("disable"),
                    OsString::from("--now"),
                    OsString::from(UNIT),
                ];
                let output = self.run(Path::new("systemctl"), &args)?;
                if !output.success && !is_missing_systemd_unit(&output) {
                    return Err(command_failed(Path::new("systemctl"), &args, &output));
                }
                let unit = self.systemd_unit_path();
                fs::remove_file(&unit)
                    .with_context(|| format!("could not remove {}", unit.display()))?;
                self.run_checked(
                    Path::new("systemctl"),
                    &[OsString::from("--user"), OsString::from("daemon-reload")],
                )?;
            }
        }
        Ok(ServiceStatus {
            installed: false,
            enabled: false,
            active: false,
        })
    }

    pub fn into_runner(self) -> R {
        self.runner
    }

    pub fn installed_environment(&self) -> Result<Option<ServiceEnvironment>> {
        let path = match self.platform {
            ServicePlatform::MacOs => self.launchd_plist_path(),
            ServicePlatform::Linux => self.systemd_unit_path(),
        };
        if !path.exists() {
            return Ok(None);
        }
        let definition = fs::read_to_string(&path)
            .with_context(|| format!("could not read service definition {}", path.display()))?;
        match self.platform {
            ServicePlatform::MacOs => parse_launchd_environment(&definition),
            ServicePlatform::Linux => parse_systemd_environment(&definition),
        }
    }

    pub fn environment_divergence(
        &self,
        current_environment: &dyn Environment,
    ) -> Result<Option<bool>> {
        let Some(installed) = self.installed_environment()? else {
            return Ok(None);
        };
        let current = ServiceEnvironment::capture(current_environment);
        let platform = match self.platform {
            ServicePlatform::MacOs => HostPlatform::MacOs,
            ServicePlatform::Linux => HostPlatform::Linux,
        };
        Ok(Some(
            resolved_protected_roots(platform, &installed)
                != resolved_protected_roots(platform, &current),
        ))
    }

    fn stop_active_service_for_reinstall(&mut self) -> Result<()> {
        let definition_exists = match self.platform {
            ServicePlatform::MacOs => self.launchd_plist_path().exists(),
            ServicePlatform::Linux => self.systemd_unit_path().exists(),
        };
        if !definition_exists {
            return Ok(());
        }
        let status = self.status()?;
        if !status.active {
            return Ok(());
        }
        match self.platform {
            ServicePlatform::MacOs => {
                let args = [
                    OsString::from("bootout"),
                    OsString::from(self.launchd_domain()),
                    self.launchd_plist_path().into_os_string(),
                ];
                let output = self.run(Path::new("launchctl"), &args)?;
                if !output.success && !is_missing_launchd_service(&output) {
                    return Err(command_failed(Path::new("launchctl"), &args, &output));
                }
            }
            ServicePlatform::Linux => {
                self.run_checked(
                    Path::new("systemctl"),
                    &[
                        OsString::from("--user"),
                        OsString::from("stop"),
                        OsString::from(UNIT),
                    ],
                )?;
            }
        }
        Ok(())
    }

    fn validate_definition_rendering(&self) -> Result<()> {
        match self.platform {
            ServicePlatform::MacOs => {
                render_launchd_template(
                    &self.binary,
                    &self.home_dir.join("Library/Logs/car-go-clean"),
                    &self.service_environment,
                )?;
            }
            ServicePlatform::Linux => {
                render_systemd_template(&self.binary, &self.service_environment)?;
            }
        }
        Ok(())
    }

    fn install_macos(&mut self) -> Result<ServiceStatus> {
        let plist = self.launchd_plist_path();
        let log_dir = self.home_dir.join("Library/Logs/car-go-clean");
        fs::create_dir_all(&log_dir)
            .with_context(|| format!("could not create {}", log_dir.display()))?;
        let rendered = render_launchd_template(&self.binary, &log_dir, &self.service_environment)?;
        atomic_write(&plist, rendered.as_bytes())?;

        self.run_checked(
            Path::new("launchctl"),
            &[
                OsString::from("enable"),
                OsString::from(self.launchd_service_target()),
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
            enabled: true,
            active: true,
        })
    }

    fn install_linux(&mut self) -> Result<ServiceStatus> {
        self.require_systemd_user()?;
        let unit = self.systemd_unit_path();
        let rendered = render_systemd_template(&self.binary, &self.service_environment)?;
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
            enabled: true,
            active: true,
        })
    }

    fn status_macos(&mut self) -> Result<ServiceStatus> {
        if !self.launchd_plist_path().exists() {
            return Ok(ServiceStatus {
                installed: false,
                enabled: false,
                active: false,
            });
        }
        let disabled_args = [
            OsString::from("print-disabled"),
            OsString::from(self.launchd_domain()),
        ];
        let disabled_output = self.run(Path::new("launchctl"), &disabled_args)?;
        if !disabled_output.success {
            return Err(command_failed(
                Path::new("launchctl"),
                &disabled_args,
                &disabled_output,
            ));
        }
        let enabled = parse_launchd_enabled(&disabled_output.stdout)?;

        let active_args = [
            OsString::from("print"),
            OsString::from(self.launchd_service_target()),
        ];
        let active_output = self.run(Path::new("launchctl"), &active_args)?;
        if !active_output.success && !is_missing_launchd_service(&active_output) {
            return Err(command_failed(
                Path::new("launchctl"),
                &active_args,
                &active_output,
            ));
        }
        Ok(ServiceStatus {
            installed: true,
            enabled,
            active: active_output.success,
        })
    }

    fn status_linux(&mut self) -> Result<ServiceStatus> {
        if !self.systemd_unit_path().exists() {
            return Ok(ServiceStatus {
                installed: false,
                enabled: false,
                active: false,
            });
        }
        self.require_systemd_user()?;
        let enabled_args = [
            OsString::from("--user"),
            OsString::from("is-enabled"),
            OsString::from(UNIT),
        ];
        let enabled_output = self.run(Path::new("systemctl"), &enabled_args)?;
        if !enabled_output.success && enabled_output.stdout.trim().is_empty() {
            return Err(command_failed(
                Path::new("systemctl"),
                &enabled_args,
                &enabled_output,
            ));
        }
        let enabled = parse_systemd_enabled(&enabled_output)?;

        let active_args = [
            OsString::from("--user"),
            OsString::from("is-active"),
            OsString::from(UNIT),
        ];
        let active_output = self.run(Path::new("systemctl"), &active_args)?;
        if !active_output.success && active_output.stdout.trim().is_empty() {
            return Err(command_failed(
                Path::new("systemctl"),
                &active_args,
                &active_output,
            ));
        }
        let active = parse_systemd_active(&active_output)?;
        Ok(ServiceStatus {
            installed: true,
            enabled,
            active,
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

fn render_launchd_template(
    binary: &Path,
    log_dir: &Path,
    environment: &ServiceEnvironment,
) -> Result<String> {
    require_absolute(binary)?;
    require_absolute(log_dir)?;
    let environment = render_launchd_environment(environment)?;
    Ok(LAUNCHD_TEMPLATE
        .replace(
            "__CAR_GO_CLEAN_BIN__",
            &xml_escape(path_as_utf8(binary, "service binary")?),
        )
        .replace(
            "__CAR_GO_CLEAN_LOG_DIR__",
            &xml_escape(path_as_utf8(log_dir, "service log directory")?),
        )
        .replace("__CAR_GO_CLEAN_ENVIRONMENT__", &environment))
}

fn render_systemd_template(binary: &Path, environment: &ServiceEnvironment) -> Result<String> {
    require_absolute(binary)?;
    Ok(SYSTEMD_TEMPLATE
        .replace(
            "__CAR_GO_CLEAN_BIN__",
            &systemd_quote(path_as_utf8(binary, "service binary")?),
        )
        .replace(
            "__CAR_GO_CLEAN_ENVIRONMENT__",
            &render_systemd_environment(environment)?,
        ))
}

fn path_as_utf8<'a>(path: &'a Path, description: &str) -> Result<&'a str> {
    path.to_str()
        .ok_or_else(|| anyhow!("{description} is not valid UTF-8: {path:?}"))
}

fn render_launchd_environment(environment: &ServiceEnvironment) -> Result<String> {
    let mut rendered = format!("  <!-- {SERVICE_ENVIRONMENT_MARKER} -->\n");
    rendered.push_str("  <key>EnvironmentVariables</key>\n  <dict>\n");
    for (name, value) in &environment.values {
        let value = value
            .to_str()
            .ok_or_else(|| anyhow!("service environment {name} is not valid UTF-8"))?;
        require_renderable_environment_value(name, value)?;
        rendered.push_str(&format!(
            "    <key>{}</key>\n    <string>{}</string>\n",
            xml_escape(name),
            xml_escape(value)
        ));
    }
    rendered.push_str("  </dict>");
    Ok(rendered)
}

fn render_systemd_environment(environment: &ServiceEnvironment) -> Result<String> {
    let mut rendered = format!("# {SERVICE_ENVIRONMENT_MARKER}");
    for (name, value) in &environment.values {
        let value = value
            .to_str()
            .ok_or_else(|| anyhow!("service environment {name} is not valid UTF-8"))?;
        require_renderable_environment_value(name, value)?;
        rendered.push_str("\nEnvironment=");
        rendered.push_str(&systemd_quote(&format!("{name}={value}")));
    }
    Ok(rendered)
}

fn require_renderable_environment_value(name: &str, value: &str) -> Result<()> {
    if value.chars().any(char::is_control) {
        bail!("service environment {name} cannot be rendered safely");
    }
    Ok(())
}

fn parse_launchd_environment(definition: &str) -> Result<Option<ServiceEnvironment>> {
    let Some(marker_offset) = definition.find(SERVICE_ENVIRONMENT_MARKER) else {
        return Ok(None);
    };
    let captured = &definition[marker_offset + SERVICE_ENVIRONMENT_MARKER.len()..];
    let dict_start = captured
        .find("<dict>")
        .ok_or_else(|| anyhow!("malformed captured launchd environment: missing <dict>"))?;
    let captured = &captured[dict_start + "<dict>".len()..];
    let dict_end = captured
        .find("</dict>")
        .ok_or_else(|| anyhow!("malformed captured launchd environment: missing </dict>"))?;
    let mut lines = captured[..dict_end]
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty());
    let mut values = BTreeMap::new();
    while let Some(key_line) = lines.next() {
        let name = xml_element(key_line, "key")
            .ok_or_else(|| anyhow!("malformed captured launchd environment key"))?;
        let value_line = lines
            .next()
            .ok_or_else(|| anyhow!("malformed captured launchd environment value"))?;
        let value = xml_element(value_line, "string")
            .ok_or_else(|| anyhow!("malformed captured launchd environment value"))?;
        let name = xml_unescape(name)?;
        if !SERVICE_ENVIRONMENT_VARIABLES.contains(&name.as_str()) {
            bail!("unsupported captured launchd environment variable {name}");
        }
        if values
            .insert(name.clone(), OsString::from(xml_unescape(value)?))
            .is_some()
        {
            bail!("duplicate captured launchd environment variable {name}");
        }
    }
    Ok(Some(ServiceEnvironment { values }))
}

fn parse_systemd_environment(definition: &str) -> Result<Option<ServiceEnvironment>> {
    let Some(marker_line) = definition
        .lines()
        .position(|line| line.trim() == format!("# {SERVICE_ENVIRONMENT_MARKER}"))
    else {
        return Ok(None);
    };
    let mut values = BTreeMap::new();
    for line in definition.lines().skip(marker_line + 1) {
        let line = line.trim();
        let Some(quoted) = line.strip_prefix("Environment=") else {
            break;
        };
        let assignment = parse_systemd_quoted(quoted)?;
        let (name, value) = assignment
            .split_once('=')
            .ok_or_else(|| anyhow!("malformed captured systemd environment assignment"))?;
        if !SERVICE_ENVIRONMENT_VARIABLES.contains(&name) {
            bail!("unsupported captured systemd environment variable {name}");
        }
        if values
            .insert(name.to_string(), OsString::from(value))
            .is_some()
        {
            bail!("duplicate captured systemd environment variable {name}");
        }
    }
    Ok(Some(ServiceEnvironment { values }))
}

fn xml_element<'a>(line: &'a str, name: &str) -> Option<&'a str> {
    line.strip_prefix(&format!("<{name}>"))?
        .strip_suffix(&format!("</{name}>"))
}

fn xml_unescape(value: &str) -> Result<String> {
    let mut unescaped = String::with_capacity(value.len());
    let mut remaining = value;
    while let Some(offset) = remaining.find('&') {
        unescaped.push_str(&remaining[..offset]);
        remaining = &remaining[offset..];
        let (replacement, consumed) = if remaining.starts_with("&amp;") {
            ('&', 5)
        } else if remaining.starts_with("&lt;") {
            ('<', 4)
        } else if remaining.starts_with("&gt;") {
            ('>', 4)
        } else if remaining.starts_with("&quot;") {
            ('"', 6)
        } else if remaining.starts_with("&apos;") {
            ('\'', 6)
        } else {
            bail!("malformed XML escape in captured launchd environment");
        };
        unescaped.push(replacement);
        remaining = &remaining[consumed..];
    }
    unescaped.push_str(remaining);
    Ok(unescaped)
}

fn parse_systemd_quoted(value: &str) -> Result<String> {
    let inner = value
        .strip_prefix('"')
        .and_then(|value| value.strip_suffix('"'))
        .ok_or_else(|| anyhow!("malformed captured systemd environment quoting"))?;
    let mut parsed = String::with_capacity(inner.len());
    let mut characters = inner.chars().peekable();
    while let Some(character) = characters.next() {
        match character {
            '\\' => parsed.push(
                characters
                    .next()
                    .ok_or_else(|| anyhow!("malformed captured systemd environment escape"))?,
            ),
            '%' if characters.peek() == Some(&'%') => {
                characters.next();
                parsed.push('%');
            }
            '%' => bail!("malformed captured systemd environment specifier"),
            _ => parsed.push(character),
        }
    }
    Ok(parsed)
}

fn resolved_protected_roots(
    platform: HostPlatform,
    environment: &ServiceEnvironment,
) -> Vec<crate::policy::ProtectedRoot> {
    let home = environment
        .var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_default();
    protected_roots_for(platform, &home, environment)
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

fn command_failed(program: &Path, args: &[OsString], output: &CommandOutput) -> anyhow::Error {
    anyhow!(
        "{} failed{}",
        command_description(program, args),
        format_command_error(output)
    )
}

fn parse_launchd_enabled(output: &str) -> Result<bool> {
    let trimmed = output.trim();
    if !trimmed.contains('{') || !trimmed.ends_with('}') {
        bail!("malformed launchctl print-disabled output");
    }
    let matching = trimmed
        .lines()
        .filter(|line| line.contains(LABEL))
        .collect::<Vec<_>>();
    if matching.is_empty() {
        return Ok(true);
    }
    if matching.len() != 1 {
        bail!("malformed launchctl print-disabled output");
    }
    let value = matching[0]
        .split_once("=>")
        .map(|(_, value)| value.trim().trim_end_matches(';'))
        .ok_or_else(|| anyhow!("malformed launchctl print-disabled output"))?;
    match value {
        "true" => Ok(false),
        "false" => Ok(true),
        _ => bail!("malformed launchctl print-disabled output"),
    }
}

fn parse_systemd_enabled(output: &CommandOutput) -> Result<bool> {
    let value = output.stdout.trim();
    let enabled = match value {
        "enabled" => true,
        "disabled" | "static" | "indirect" | "masked" | "generated" | "transient"
        | "enabled-runtime" | "linked" | "linked-runtime" | "alias" | "not-found" => false,
        _ => bail!("malformed systemctl is-enabled output: {value:?}"),
    };
    if output.success != (value == "enabled") {
        bail!("malformed systemctl is-enabled result for {value:?}");
    }
    Ok(enabled)
}

fn parse_systemd_active(output: &CommandOutput) -> Result<bool> {
    let value = output.stdout.trim();
    let active = match value {
        "active" => true,
        "inactive" | "failed" | "activating" | "deactivating" | "reloading" | "unknown" => false,
        _ => bail!("malformed systemctl is-active output: {value:?}"),
    };
    if output.success != active {
        bail!("malformed systemctl is-active result for {value:?}");
    }
    Ok(active)
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
