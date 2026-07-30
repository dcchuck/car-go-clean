use anyhow::Result;
use car_go_clean::policy::{Environment, ProtectedRootKind, RootProvenance};
use car_go_clean::service::{
    resolve_service_binary, CommandOutput, CommandRunner, ServiceEnvironment, ServiceManager,
    ServicePlatform, ServiceStatus,
};
use std::collections::{BTreeMap, VecDeque};
use std::ffi::OsString;
use std::fs;
use std::path::{Path, PathBuf};

#[derive(Default)]
struct FakeRunner {
    calls: Vec<(PathBuf, Vec<OsString>)>,
    outputs: VecDeque<CommandOutput>,
    fail_systemd_environment: bool,
    disable_output: Option<CommandOutput>,
    bootout_output: Option<CommandOutput>,
}

impl FakeRunner {
    fn with_outputs(outputs: impl IntoIterator<Item = CommandOutput>) -> Self {
        Self {
            outputs: outputs.into_iter().collect(),
            ..Self::default()
        }
    }
}

impl CommandRunner for FakeRunner {
    fn run(&mut self, program: &Path, args: &[OsString]) -> Result<CommandOutput> {
        self.calls.push((program.to_path_buf(), args.to_vec()));
        if let Some(output) = self.outputs.pop_front() {
            return Ok(output);
        }
        if program == Path::new("systemctl")
            && strings(args) == ["--user", "disable", "--now", "car-go-clean.service"]
        {
            if let Some(output) = &self.disable_output {
                return Ok(output.clone());
            }
        }
        if program == Path::new("launchctl")
            && strings(args).first().map(String::as_str) == Some("bootout")
        {
            if let Some(output) = &self.bootout_output {
                return Ok(output.clone());
            }
        }
        let success = !(self.fail_systemd_environment
            && program == Path::new("systemctl")
            && args
                .iter()
                .map(|arg| arg.to_string_lossy())
                .collect::<Vec<_>>()
                == ["--user", "show-environment"]);
        Ok(CommandOutput::new(success, String::new(), String::new()))
    }
}

fn test_manager(
    platform: ServicePlatform,
    home: &Path,
    binary: PathBuf,
) -> ServiceManager<FakeRunner> {
    ServiceManager::new(platform, home.to_path_buf(), binary, FakeRunner::default())
}

fn strings(args: &[OsString]) -> Vec<String> {
    args.iter()
        .map(|arg| arg.to_string_lossy().into_owned())
        .collect()
}

struct TestEnvironment(BTreeMap<String, OsString>);

impl Environment for TestEnvironment {
    fn var_os(&self, name: &str) -> Option<OsString> {
        self.0.get(name).cloned()
    }
}

fn environment(values: &[(&str, &str)]) -> TestEnvironment {
    TestEnvironment(
        values
            .iter()
            .map(|(name, value)| ((*name).to_string(), OsString::from(value)))
            .collect(),
    )
}

fn call_arguments(runner: &FakeRunner) -> Vec<Vec<String>> {
    runner.calls.iter().map(|(_, args)| strings(args)).collect()
}

#[test]
fn mac_install_clears_persistent_disable_before_bootstrap() {
    let work = tempfile::tempdir().unwrap();
    let mut manager = ServiceManager::new_with_environment(
        ServicePlatform::MacOs,
        work.path().to_path_buf(),
        work.path().join("bin/car-go-clean"),
        FakeRunner::default(),
        &environment(&[("HOME", "/Users/tester")]),
    );

    assert_eq!(
        manager.install().unwrap(),
        ServiceStatus {
            installed: true,
            enabled: true,
            active: true,
        }
    );
    assert_eq!(
        call_arguments(&manager.into_runner()),
        [
            vec![
                "enable".to_string(),
                format!("gui/{}/com.dcchuck.car-go-clean", unsafe {
                    libc::geteuid()
                }),
            ],
            vec![
                "bootstrap".to_string(),
                format!("gui/{}", unsafe { libc::geteuid() }),
                work.path()
                    .join("Library/LaunchAgents/com.dcchuck.car-go-clean.plist")
                    .display()
                    .to_string(),
            ],
            vec![
                "kickstart".to_string(),
                "-k".to_string(),
                format!("gui/{}/com.dcchuck.car-go-clean", unsafe {
                    libc::geteuid()
                }),
            ],
        ]
    );
}

#[test]
fn reinstall_transiently_stops_an_active_service_before_loading_new_environment() {
    let work = tempfile::tempdir().unwrap();
    let plist = work
        .path()
        .join("Library/LaunchAgents/com.dcchuck.car-go-clean.plist");
    fs::create_dir_all(plist.parent().unwrap()).unwrap();
    fs::write(&plist, "legacy definition").unwrap();
    let mut mac = ServiceManager::new_with_environment(
        ServicePlatform::MacOs,
        work.path().to_path_buf(),
        work.path().join("bin/car-go-clean"),
        FakeRunner::with_outputs([
            CommandOutput::new(
                true,
                "disabled services = {\n}\n".to_string(),
                String::new(),
            ),
            CommandOutput::new(true, String::new(), String::new()),
            CommandOutput::new(true, String::new(), String::new()),
            CommandOutput::new(true, String::new(), String::new()),
            CommandOutput::new(true, String::new(), String::new()),
            CommandOutput::new(true, String::new(), String::new()),
        ]),
        &environment(&[("HOME", "/new/home")]),
    );
    mac.install().unwrap();
    let mac_calls = call_arguments(&mac.into_runner());
    assert_eq!(mac_calls[2][0], "bootout");
    assert_eq!(mac_calls[3][0], "enable");
    assert_eq!(mac_calls[4][0], "bootstrap");
    assert_eq!(mac_calls[5][0], "kickstart");

    let unit = work
        .path()
        .join(".config/systemd/user/car-go-clean.service");
    fs::create_dir_all(unit.parent().unwrap()).unwrap();
    fs::write(&unit, "legacy definition").unwrap();
    let mut linux = ServiceManager::new_with_environment(
        ServicePlatform::Linux,
        work.path().to_path_buf(),
        work.path().join("bin/car-go-clean"),
        FakeRunner::with_outputs([
            CommandOutput::new(true, String::new(), String::new()),
            CommandOutput::new(true, "enabled\n".to_string(), String::new()),
            CommandOutput::new(true, "active\n".to_string(), String::new()),
            CommandOutput::new(true, String::new(), String::new()),
            CommandOutput::new(true, String::new(), String::new()),
            CommandOutput::new(true, "enabled\n".to_string(), String::new()),
            CommandOutput::new(false, "inactive\n".to_string(), String::new()),
            CommandOutput::new(true, String::new(), String::new()),
            CommandOutput::new(true, String::new(), String::new()),
            CommandOutput::new(true, String::new(), String::new()),
        ]),
        &environment(&[("HOME", "/new/home")]),
    );
    linux.install().unwrap();
    let linux_calls = call_arguments(&linux.into_runner());
    assert_eq!(linux_calls[3], ["--user", "stop", "car-go-clean.service"]);
    assert_eq!(linux_calls[8], ["--user", "daemon-reload"]);
    assert_eq!(
        linux_calls[9],
        ["--user", "enable", "--now", "car-go-clean.service"]
    );
}

#[test]
fn linux_reinstall_stops_and_verifies_activating_service_before_rewrite() {
    let work = tempfile::tempdir().unwrap();
    let unit = work
        .path()
        .join(".config/systemd/user/car-go-clean.service");
    fs::create_dir_all(unit.parent().unwrap()).unwrap();
    fs::write(&unit, "legacy definition").unwrap();
    let mut manager = ServiceManager::new(
        ServicePlatform::Linux,
        work.path().to_path_buf(),
        work.path().join("bin/car-go-clean"),
        FakeRunner::with_outputs([
            CommandOutput::new(true, String::new(), String::new()),
            CommandOutput::new(false, "disabled\n".to_string(), String::new()),
            CommandOutput::new(false, "activating\n".to_string(), String::new()),
            CommandOutput::new(true, String::new(), String::new()),
            CommandOutput::new(true, String::new(), String::new()),
            CommandOutput::new(false, "disabled\n".to_string(), String::new()),
            CommandOutput::new(false, "inactive\n".to_string(), String::new()),
            CommandOutput::new(true, String::new(), String::new()),
            CommandOutput::new(true, String::new(), String::new()),
            CommandOutput::new(true, String::new(), String::new()),
        ]),
    );

    manager.install().unwrap();

    let calls = call_arguments(&manager.into_runner());
    assert_eq!(calls[3], ["--user", "stop", "car-go-clean.service"]);
    assert_eq!(calls[7], ["--user", "show-environment"]);
    assert_eq!(calls[8], ["--user", "daemon-reload"]);
    assert_eq!(
        calls[9],
        ["--user", "enable", "--now", "car-go-clean.service"]
    );
}

#[test]
fn mac_status_models_persistent_enablement_and_process_state_separately() {
    let work = tempfile::tempdir().unwrap();
    let plist = work
        .path()
        .join("Library/LaunchAgents/com.dcchuck.car-go-clean.plist");
    fs::create_dir_all(plist.parent().unwrap()).unwrap();
    fs::write(&plist, "legacy definition").unwrap();
    let mut manager = ServiceManager::new(
        ServicePlatform::MacOs,
        work.path().to_path_buf(),
        work.path().join("bin/car-go-clean"),
        FakeRunner::with_outputs([
            CommandOutput::new(
                true,
                "disabled services = {\n  \"com.dcchuck.car-go-clean\" => true\n}\n".to_string(),
                String::new(),
            ),
            CommandOutput::new(
                false,
                String::new(),
                "Could not find specified service".to_string(),
            ),
        ]),
    );

    assert_eq!(
        manager.status().unwrap(),
        ServiceStatus {
            installed: true,
            enabled: false,
            active: false,
        }
    );
    assert_eq!(
        call_arguments(&manager.into_runner()),
        [
            vec![
                "print-disabled".to_string(),
                format!("gui/{}", unsafe { libc::geteuid() }),
            ],
            vec![
                "print".to_string(),
                format!("gui/{}/com.dcchuck.car-go-clean", unsafe {
                    libc::geteuid()
                }),
            ],
        ]
    );
}

#[test]
fn mac_status_ignores_neighboring_disabled_labels() {
    let work = tempfile::tempdir().unwrap();
    let plist = work
        .path()
        .join("Library/LaunchAgents/com.dcchuck.car-go-clean.plist");
    fs::create_dir_all(plist.parent().unwrap()).unwrap();
    fs::write(&plist, "legacy definition").unwrap();
    let mut manager = ServiceManager::new(
        ServicePlatform::MacOs,
        work.path().to_path_buf(),
        work.path().join("bin/car-go-clean"),
        FakeRunner::with_outputs([
            CommandOutput::new(
                true,
                "disabled services = {\n  \"com.dcchuck.car-go-clean.helper\" => true\n}\n"
                    .to_string(),
                String::new(),
            ),
            CommandOutput::new(
                false,
                String::new(),
                "Could not find specified service".to_string(),
            ),
        ]),
    );

    assert_eq!(
        manager.status().unwrap(),
        ServiceStatus {
            installed: true,
            enabled: true,
            active: false,
        }
    );
}

#[test]
fn mac_stop_disables_before_bootout_and_start_enables_before_bootstrap() {
    let work = tempfile::tempdir().unwrap();
    let plist = work
        .path()
        .join("Library/LaunchAgents/com.dcchuck.car-go-clean.plist");
    fs::create_dir_all(plist.parent().unwrap()).unwrap();
    fs::write(&plist, "legacy definition").unwrap();

    let enabled = "disabled services = {\n}\n";
    let mut stop = ServiceManager::new(
        ServicePlatform::MacOs,
        work.path().to_path_buf(),
        work.path().join("bin/car-go-clean"),
        FakeRunner::with_outputs([
            CommandOutput::new(true, enabled.to_string(), String::new()),
            CommandOutput::new(true, String::new(), String::new()),
            CommandOutput::new(true, String::new(), String::new()),
            CommandOutput::new(true, String::new(), String::new()),
        ]),
    );
    stop.stop().unwrap();
    let stop_calls = call_arguments(&stop.into_runner());
    assert_eq!(stop_calls[2][0], "disable");
    assert_eq!(stop_calls[3][0], "bootout");

    let mut start = ServiceManager::new(
        ServicePlatform::MacOs,
        work.path().to_path_buf(),
        work.path().join("bin/car-go-clean"),
        FakeRunner::with_outputs([
            CommandOutput::new(
                true,
                "disabled services = {\n  \"com.dcchuck.car-go-clean\" => true\n}\n".to_string(),
                String::new(),
            ),
            CommandOutput::new(
                false,
                String::new(),
                "Could not find specified service".to_string(),
            ),
            CommandOutput::new(true, String::new(), String::new()),
            CommandOutput::new(true, String::new(), String::new()),
            CommandOutput::new(true, String::new(), String::new()),
        ]),
    );
    start.start().unwrap();
    let start_calls = call_arguments(&start.into_runner());
    assert_eq!(start_calls[2][0], "enable");
    assert_eq!(start_calls[3][0], "bootstrap");
    assert_eq!(start_calls[4][0], "kickstart");
}

#[test]
fn linux_lifecycle_uses_persistent_enablement_commands_in_order() {
    let work = tempfile::tempdir().unwrap();
    let binary = work.path().join("bin/car-go-clean");
    let mut install = ServiceManager::new(
        ServicePlatform::Linux,
        work.path().to_path_buf(),
        binary.clone(),
        FakeRunner::default(),
    );
    install.install().unwrap();
    assert_eq!(
        call_arguments(&install.into_runner()),
        [
            vec!["--user".to_string(), "show-environment".to_string()],
            vec!["--user".to_string(), "daemon-reload".to_string()],
            vec![
                "--user".to_string(),
                "enable".to_string(),
                "--now".to_string(),
                "car-go-clean.service".to_string(),
            ],
        ]
    );

    let unit = work
        .path()
        .join(".config/systemd/user/car-go-clean.service");
    let status_outputs = || {
        [
            CommandOutput::new(true, String::new(), String::new()),
            CommandOutput::new(false, "disabled\n".to_string(), String::new()),
            CommandOutput::new(false, "inactive\n".to_string(), String::new()),
        ]
    };
    let mut start = ServiceManager::new(
        ServicePlatform::Linux,
        work.path().to_path_buf(),
        binary.clone(),
        FakeRunner::with_outputs(status_outputs()),
    );
    start.start().unwrap();
    assert!(unit.exists());
    let start_calls = call_arguments(&start.into_runner());
    assert_eq!(
        &start_calls[3..],
        [
            vec!["--user".to_string(), "daemon-reload".to_string()],
            vec![
                "--user".to_string(),
                "enable".to_string(),
                "--now".to_string(),
                "car-go-clean.service".to_string(),
            ],
        ]
    );

    let mut stop = ServiceManager::new(
        ServicePlatform::Linux,
        work.path().to_path_buf(),
        binary,
        FakeRunner::with_outputs([
            CommandOutput::new(true, String::new(), String::new()),
            CommandOutput::new(true, "enabled\n".to_string(), String::new()),
            CommandOutput::new(true, "active\n".to_string(), String::new()),
        ]),
    );
    stop.stop().unwrap();
    assert_eq!(
        call_arguments(&stop.into_runner()).last().unwrap(),
        &[
            "--user".to_string(),
            "disable".to_string(),
            "--now".to_string(),
            "car-go-clean.service".to_string(),
        ]
    );
}

#[test]
fn refresh_rewrites_an_installed_definition_without_enabling_or_starting_it() {
    let work = tempfile::tempdir().unwrap();
    let binary = work.path().join("bin/car-go-clean-v040");
    let plist = work
        .path()
        .join("Library/LaunchAgents/com.dcchuck.car-go-clean.plist");
    fs::create_dir_all(plist.parent().unwrap()).unwrap();
    fs::write(&plist, "legacy definition").unwrap();

    let mut manager = ServiceManager::new_with_environment(
        ServicePlatform::MacOs,
        work.path().to_path_buf(),
        binary.clone(),
        FakeRunner::with_outputs([
            CommandOutput::new(
                true,
                "disabled services = {\n  \"com.dcchuck.car-go-clean\" => true\n}\n".to_string(),
                String::new(),
            ),
            CommandOutput::new(
                false,
                String::new(),
                "Could not find specified service".to_string(),
            ),
        ]),
        &environment(&[
            ("HOME", work.path().to_str().unwrap()),
            ("CARGO_HOME", work.path().join("cargo").to_str().unwrap()),
        ]),
    );

    assert_eq!(
        manager.refresh().unwrap(),
        ServiceStatus {
            installed: true,
            enabled: false,
            active: false,
        }
    );
    let definition = fs::read_to_string(&plist).unwrap();
    assert!(definition.contains(binary.to_str().unwrap()));
    assert!(definition.contains("car-go-clean-service-environment-v1"));
    assert!(definition.contains(work.path().join("cargo").to_str().unwrap()));
    assert_eq!(
        call_arguments(&manager.into_runner()),
        [
            vec![
                "print-disabled".to_string(),
                format!("gui/{}", unsafe { libc::geteuid() }),
            ],
            vec![
                "print".to_string(),
                format!("gui/{}/com.dcchuck.car-go-clean", unsafe {
                    libc::geteuid()
                }),
            ],
        ]
    );
}

#[test]
fn linux_refresh_stops_and_verifies_every_noninactive_activity_state_before_rewrite() {
    for (state, success) in [
        ("active", true),
        ("reloading", true),
        ("refreshing", true),
        ("activating", false),
        ("deactivating", false),
        ("failed", false),
        ("maintenance", false),
        ("unknown", false),
    ] {
        let work = tempfile::tempdir().unwrap();
        let unit = work
            .path()
            .join(".config/systemd/user/car-go-clean.service");
        fs::create_dir_all(unit.parent().unwrap()).unwrap();
        fs::write(&unit, format!("legacy definition for {state}")).unwrap();
        let binary = work.path().join("bin/car-go-clean-v040");
        let mut manager = ServiceManager::new(
            ServicePlatform::Linux,
            work.path().to_path_buf(),
            binary.clone(),
            FakeRunner::with_outputs([
                CommandOutput::new(true, String::new(), String::new()),
                CommandOutput::new(false, "disabled\n".to_string(), String::new()),
                CommandOutput::new(success, format!("{state}\n"), String::new()),
                CommandOutput::new(true, String::new(), String::new()),
                CommandOutput::new(true, String::new(), String::new()),
                CommandOutput::new(false, "disabled\n".to_string(), String::new()),
                CommandOutput::new(false, "inactive\n".to_string(), String::new()),
                CommandOutput::new(true, String::new(), String::new()),
            ]),
        );

        manager
            .refresh()
            .unwrap_or_else(|error| panic!("{state}: {error:#}"));

        let calls = call_arguments(&manager.into_runner());
        assert_eq!(
            calls[3],
            [
                "--user".to_string(),
                "disable".to_string(),
                "--now".to_string(),
                "car-go-clean.service".to_string(),
            ],
            "{state}"
        );
        assert_eq!(
            calls.last().unwrap(),
            &["--user".to_string(), "daemon-reload".to_string(),],
            "{state}"
        );
        let definition = fs::read_to_string(&unit).unwrap();
        assert!(definition.contains(binary.to_str().unwrap()), "{state}");
        assert!(!definition.contains("legacy definition"), "{state}");
    }
}

#[test]
fn linux_refresh_does_not_rewrite_when_activity_cannot_be_verified_inactive() {
    let work = tempfile::tempdir().unwrap();
    let unit = work
        .path()
        .join(".config/systemd/user/car-go-clean.service");
    fs::create_dir_all(unit.parent().unwrap()).unwrap();
    let original = "legacy definition";
    fs::write(&unit, original).unwrap();
    let mut manager = ServiceManager::new(
        ServicePlatform::Linux,
        work.path().to_path_buf(),
        work.path().join("bin/car-go-clean-v040"),
        FakeRunner::with_outputs([
            CommandOutput::new(true, String::new(), String::new()),
            CommandOutput::new(false, "disabled\n".to_string(), String::new()),
            CommandOutput::new(false, "activating\n".to_string(), String::new()),
            CommandOutput::new(true, String::new(), String::new()),
            CommandOutput::new(true, String::new(), String::new()),
            CommandOutput::new(false, "disabled\n".to_string(), String::new()),
            CommandOutput::new(false, String::new(), "query failed".to_string()),
        ]),
    );

    let error = format!("{:#}", manager.refresh().unwrap_err());

    assert!(error.contains("query failed"), "{error}");
    assert_eq!(fs::read_to_string(&unit).unwrap(), original);
}

#[test]
fn restart_requires_an_installed_and_enabled_definition() {
    let work = tempfile::tempdir().unwrap();
    let mut missing = test_manager(
        ServicePlatform::MacOs,
        work.path(),
        work.path().join("bin/car-go-clean"),
    );
    assert!(missing
        .restart()
        .unwrap_err()
        .to_string()
        .contains("not installed"));

    let unit = work
        .path()
        .join(".config/systemd/user/car-go-clean.service");
    fs::create_dir_all(unit.parent().unwrap()).unwrap();
    fs::write(&unit, "unit").unwrap();
    let mut disabled = ServiceManager::new(
        ServicePlatform::Linux,
        work.path().to_path_buf(),
        work.path().join("bin/car-go-clean"),
        FakeRunner::with_outputs([
            CommandOutput::new(true, String::new(), String::new()),
            CommandOutput::new(false, "disabled\n".to_string(), String::new()),
            CommandOutput::new(false, "inactive\n".to_string(), String::new()),
        ]),
    );
    assert!(disabled
        .restart()
        .unwrap_err()
        .to_string()
        .contains("not enabled"));
}

#[test]
fn restart_uses_only_the_platform_restart_command_after_state_validation() {
    let work = tempfile::tempdir().unwrap();
    let plist = work
        .path()
        .join("Library/LaunchAgents/com.dcchuck.car-go-clean.plist");
    fs::create_dir_all(plist.parent().unwrap()).unwrap();
    fs::write(&plist, "definition").unwrap();
    let mut mac = ServiceManager::new(
        ServicePlatform::MacOs,
        work.path().to_path_buf(),
        work.path().join("bin/car-go-clean"),
        FakeRunner::with_outputs([
            CommandOutput::new(
                true,
                "disabled services = {\n}\n".to_string(),
                String::new(),
            ),
            CommandOutput::new(true, String::new(), String::new()),
            CommandOutput::new(true, String::new(), String::new()),
        ]),
    );
    mac.restart().unwrap();
    assert_eq!(
        call_arguments(&mac.into_runner()).last().unwrap(),
        &[
            "kickstart".to_string(),
            "-k".to_string(),
            format!("gui/{}/com.dcchuck.car-go-clean", unsafe {
                libc::geteuid()
            }),
        ]
    );

    let unit = work
        .path()
        .join(".config/systemd/user/car-go-clean.service");
    fs::create_dir_all(unit.parent().unwrap()).unwrap();
    fs::write(&unit, "definition").unwrap();
    let mut linux = ServiceManager::new(
        ServicePlatform::Linux,
        work.path().to_path_buf(),
        work.path().join("bin/car-go-clean"),
        FakeRunner::with_outputs([
            CommandOutput::new(true, String::new(), String::new()),
            CommandOutput::new(true, "enabled\n".to_string(), String::new()),
            CommandOutput::new(true, "active\n".to_string(), String::new()),
            CommandOutput::new(true, String::new(), String::new()),
        ]),
    );
    linux.restart().unwrap();
    assert_eq!(
        call_arguments(&linux.into_runner()).last().unwrap(),
        &[
            "--user".to_string(),
            "restart".to_string(),
            "car-go-clean.service".to_string(),
        ]
    );
}

#[test]
fn mac_restart_bootstraps_an_enabled_but_unloaded_definition_before_kickstart() {
    let work = tempfile::tempdir().unwrap();
    let plist = work
        .path()
        .join("Library/LaunchAgents/com.dcchuck.car-go-clean.plist");
    fs::create_dir_all(plist.parent().unwrap()).unwrap();
    fs::write(&plist, "definition").unwrap();
    let mut manager = ServiceManager::new(
        ServicePlatform::MacOs,
        work.path().to_path_buf(),
        work.path().join("bin/car-go-clean"),
        FakeRunner::with_outputs([
            CommandOutput::new(
                true,
                "disabled services = {\n}\n".to_string(),
                String::new(),
            ),
            CommandOutput::new(
                false,
                String::new(),
                "Could not find specified service".to_string(),
            ),
            CommandOutput::new(true, String::new(), String::new()),
            CommandOutput::new(true, String::new(), String::new()),
        ]),
    );

    manager.restart().unwrap();
    let calls = call_arguments(&manager.into_runner());
    assert_eq!(calls[2][0], "bootstrap");
    assert_eq!(calls[3][0], "kickstart");
}

#[test]
fn manager_output_must_match_an_enumerated_state() {
    let work = tempfile::tempdir().unwrap();
    let unit = work
        .path()
        .join(".config/systemd/user/car-go-clean.service");
    fs::create_dir_all(unit.parent().unwrap()).unwrap();
    fs::write(&unit, "unit").unwrap();
    let mut manager = ServiceManager::new(
        ServicePlatform::Linux,
        work.path().to_path_buf(),
        work.path().join("bin/car-go-clean"),
        FakeRunner::with_outputs([
            CommandOutput::new(true, String::new(), String::new()),
            CommandOutput::new(true, "surprise\n".to_string(), String::new()),
        ]),
    );

    assert!(manager
        .status()
        .unwrap_err()
        .to_string()
        .contains("malformed systemctl is-enabled output"));
}

#[test]
fn manager_status_errors_preserve_the_underlying_failure() {
    let work = tempfile::tempdir().unwrap();
    let unit = work
        .path()
        .join(".config/systemd/user/car-go-clean.service");
    fs::create_dir_all(unit.parent().unwrap()).unwrap();
    fs::write(&unit, "unit").unwrap();
    let mut manager = ServiceManager::new(
        ServicePlatform::Linux,
        work.path().to_path_buf(),
        work.path().join("bin/car-go-clean"),
        FakeRunner::with_outputs([
            CommandOutput::new(true, String::new(), String::new()),
            CommandOutput::new(
                false,
                String::new(),
                "Failed to query unit: Permission denied".to_string(),
            ),
        ]),
    );

    assert!(manager
        .status()
        .unwrap_err()
        .to_string()
        .contains("Permission denied"));
}

#[test]
fn systemd_status_recognizes_native_enablement_states_independent_of_exit_success() {
    let work = tempfile::tempdir().unwrap();
    let unit = work
        .path()
        .join(".config/systemd/user/car-go-clean.service");
    fs::create_dir_all(unit.parent().unwrap()).unwrap();
    fs::write(&unit, "unit").unwrap();

    for (state, success, persistent) in [
        ("enabled", true, true),
        ("enabled-runtime", true, false),
        ("static", true, false),
        ("alias", true, false),
        ("indirect", true, false),
        ("generated", true, false),
        ("disabled", false, false),
        ("masked", false, false),
        ("masked-runtime", false, false),
        ("linked", true, false),
        ("linked-runtime", true, false),
        ("transient", true, false),
        ("bad", false, false),
        ("not-found", false, false),
    ] {
        let mut manager = ServiceManager::new(
            ServicePlatform::Linux,
            work.path().to_path_buf(),
            work.path().join("bin/car-go-clean"),
            FakeRunner::with_outputs([
                CommandOutput::new(true, String::new(), String::new()),
                CommandOutput::new(success, format!("{state}\n"), String::new()),
                CommandOutput::new(false, "inactive\n".to_string(), String::new()),
            ]),
        );

        let status = manager
            .status()
            .unwrap_or_else(|error| panic!("{state}: {error:#}"));
        assert_eq!(status.enabled, persistent, "{state}");
        assert!(!status.active, "{state}");
    }
}

#[test]
fn systemd_status_recognizes_native_activity_states_independent_of_exit_success() {
    let work = tempfile::tempdir().unwrap();
    let unit = work
        .path()
        .join(".config/systemd/user/car-go-clean.service");
    fs::create_dir_all(unit.parent().unwrap()).unwrap();
    fs::write(&unit, "unit").unwrap();

    for (state, success, active) in [
        ("active", true, true),
        ("reloading", true, true),
        ("refreshing", true, true),
        ("inactive", false, false),
        ("failed", false, false),
        ("activating", false, false),
        ("deactivating", false, false),
        ("maintenance", false, false),
        ("unknown", false, false),
    ] {
        let mut manager = ServiceManager::new(
            ServicePlatform::Linux,
            work.path().to_path_buf(),
            work.path().join("bin/car-go-clean"),
            FakeRunner::with_outputs([
                CommandOutput::new(true, String::new(), String::new()),
                CommandOutput::new(true, "enabled\n".to_string(), String::new()),
                CommandOutput::new(success, format!("{state}\n"), String::new()),
            ]),
        );

        let status = manager
            .status()
            .unwrap_or_else(|error| panic!("{state}: {error:#}"));
        assert!(status.enabled, "{state}");
        assert_eq!(status.active, active, "{state}");
    }
}

#[test]
fn captured_environment_is_whitelisted_and_round_trips_through_definitions() {
    let work = tempfile::tempdir().unwrap();
    let current = environment(&[
        ("HOME", "/Users/a & b"),
        ("CARGO_HOME", "/cargo/<shared>"),
        ("RUSTUP_HOME", "/rustup"),
        ("XDG_CACHE_HOME", "/cache"),
        ("XDG_DATA_HOME", "/data/\"quoted\"/%done\\slash"),
        ("GOMODCACHE", "/go/pkg/mod"),
        ("BUN_INSTALL", "/bun"),
        ("BUN_INSTALL_CACHE_DIR", "/bun-cache"),
        ("COLIMA_HOME", "/colima"),
        ("LIMA_HOME", "/lima"),
        ("AWS_SECRET_ACCESS_KEY", "do-not-capture"),
        ("PATH", "/also/not/captured"),
    ]);
    let captured = ServiceEnvironment::capture(&current);
    assert_eq!(
        captured.values.keys().cloned().collect::<Vec<_>>(),
        [
            "BUN_INSTALL",
            "BUN_INSTALL_CACHE_DIR",
            "CARGO_HOME",
            "COLIMA_HOME",
            "GOMODCACHE",
            "HOME",
            "LIMA_HOME",
            "RUSTUP_HOME",
            "XDG_CACHE_HOME",
            "XDG_DATA_HOME",
        ]
    );

    let binary = work.path().join("bin/car-go-clean");
    let mut mac = ServiceManager::new_with_environment(
        ServicePlatform::MacOs,
        work.path().to_path_buf(),
        binary.clone(),
        FakeRunner::default(),
        &current,
    );
    mac.install().unwrap();
    assert_eq!(mac.installed_environment().unwrap(), Some(captured.clone()));
    assert_eq!(mac.environment_divergence(&current).unwrap(), Some(false));

    let mut linux = ServiceManager::new_with_environment(
        ServicePlatform::Linux,
        work.path().to_path_buf(),
        binary,
        FakeRunner::default(),
        &current,
    );
    linux.install().unwrap();
    assert_eq!(
        linux.installed_environment().unwrap(),
        Some(captured.clone())
    );
    assert_eq!(linux.environment_divergence(&current).unwrap(), Some(false));
    let installed_roots = linux.installed_protected_roots().unwrap().unwrap();
    assert!(!installed_roots.is_empty());
    assert!(installed_roots
        .iter()
        .all(|root| root.provenance == RootProvenance::ServiceDefinition));
    assert!(installed_roots.iter().any(|root| {
        root.kind == ProtectedRootKind::Cargo && root.path == Path::new("/cargo/<shared>")
    }));
    assert!(installed_roots.iter().any(|root| {
        root.kind == ProtectedRootKind::Container
            && root.path == Path::new("/data/\"quoted\"/%done\\slash/docker")
    }));

    let changed = environment(&[
        ("HOME", "/Users/a & b"),
        ("CARGO_HOME", "/other-cargo"),
        ("RUSTUP_HOME", "/rustup"),
        ("XDG_CACHE_HOME", "/cache"),
        ("XDG_DATA_HOME", "/data/\"quoted\"/%done\\slash"),
        ("GOMODCACHE", "/go/pkg/mod"),
        ("BUN_INSTALL", "/bun"),
        ("BUN_INSTALL_CACHE_DIR", "/bun-cache"),
        ("COLIMA_HOME", "/colima"),
        ("LIMA_HOME", "/lima"),
    ]);
    assert_eq!(linux.environment_divergence(&changed).unwrap(), Some(true));

    let lexically_different_but_equivalent = environment(&[
        ("HOME", "/Users/a & b/"),
        ("CARGO_HOME", "/cargo/<shared>/"),
        ("RUSTUP_HOME", "/rustup/"),
        ("XDG_CACHE_HOME", "/cache/"),
        ("XDG_DATA_HOME", "/data/\"quoted\"/%done\\slash/"),
        ("GOMODCACHE", "/go/pkg/mod/"),
        ("BUN_INSTALL", "/bun/"),
        ("BUN_INSTALL_CACHE_DIR", "/bun-cache/"),
        ("COLIMA_HOME", "/colima/"),
        ("LIMA_HOME", "/lima/"),
    ]);
    assert_eq!(
        linux
            .environment_divergence(&lexically_different_but_equivalent)
            .unwrap(),
        Some(false)
    );
}

#[cfg(unix)]
#[test]
fn service_definition_captures_physical_manager_roots() {
    use std::os::unix::fs::symlink;

    let work = tempfile::tempdir().unwrap();
    let physical = work.path().join("physical/cargo");
    let alias = work.path().join("install-shell/cargo");
    fs::create_dir_all(&physical).unwrap();
    fs::create_dir_all(alias.parent().unwrap()).unwrap();
    symlink(&physical, &alias).unwrap();
    let physical = fs::canonicalize(&physical).unwrap();
    let current = environment(&[
        ("HOME", work.path().to_str().unwrap()),
        ("CARGO_HOME", alias.to_str().unwrap()),
    ]);
    let mut manager = ServiceManager::new_with_environment(
        ServicePlatform::MacOs,
        work.path().to_path_buf(),
        work.path().join("bin/car-go-clean"),
        FakeRunner::default(),
        &current,
    );

    manager.install().unwrap();
    let installed = manager.installed_environment().unwrap().unwrap();
    assert_eq!(
        installed.values.get("CARGO_HOME"),
        Some(&physical.as_os_str().to_os_string())
    );
    assert!(!fs::read_to_string(
        work.path()
            .join("Library/LaunchAgents/com.dcchuck.car-go-clean.plist")
    )
    .unwrap()
    .contains(alias.to_str().unwrap()));
    let physical_environment = environment(&[
        ("HOME", work.path().to_str().unwrap()),
        ("CARGO_HOME", physical.to_str().unwrap()),
    ]);
    assert_eq!(
        manager
            .environment_divergence(&physical_environment)
            .unwrap(),
        Some(false)
    );
}

#[test]
fn service_definition_rejects_ambiguous_manager_roots_before_manager_calls() {
    let work = tempfile::tempdir().unwrap();
    for (variable, value) in [
        ("CARGO_HOME", "relative/cargo"),
        ("RUSTUP_HOME", "/toolchains/../rustup"),
        ("LIMA_HOME", "/containers/./lima"),
    ] {
        let mut manager = ServiceManager::new_with_environment(
            ServicePlatform::MacOs,
            work.path().to_path_buf(),
            work.path().join("bin/car-go-clean"),
            FakeRunner::default(),
            &environment(&[("HOME", work.path().to_str().unwrap()), (variable, value)]),
        );

        let error = manager.install().unwrap_err().to_string();
        assert!(error.contains(variable), "{variable}: {error}");
        assert!(
            error.contains("absolute physical path"),
            "{variable}: {error}"
        );
        assert!(manager.into_runner().calls.is_empty(), "{variable}");
    }
}

#[test]
fn install_rejects_environment_values_that_cannot_be_rendered_safely() {
    let work = tempfile::tempdir().unwrap();
    let mut manager = ServiceManager::new_with_environment(
        ServicePlatform::Linux,
        work.path().to_path_buf(),
        work.path().join("bin/car-go-clean"),
        FakeRunner::default(),
        &environment(&[("HOME", "/home/tester\ninjected")]),
    );

    let error = manager.install().unwrap_err();
    assert!(error.to_string().contains("cannot be rendered safely"));
    assert!(!work
        .path()
        .join(".config/systemd/user/car-go-clean.service")
        .exists());
    assert!(manager.into_runner().calls.is_empty());
}

#[cfg(unix)]
#[test]
fn install_rejects_non_utf8_environment_values() {
    use std::os::unix::ffi::OsStringExt;

    let work = tempfile::tempdir().unwrap();
    let environment = TestEnvironment(BTreeMap::from([(
        "HOME".to_string(),
        OsString::from_vec(vec![b'/', b'h', 0xff]),
    )]));
    let mut manager = ServiceManager::new_with_environment(
        ServicePlatform::Linux,
        work.path().to_path_buf(),
        work.path().join("bin/car-go-clean"),
        FakeRunner::default(),
        &environment,
    );

    let error = manager.install().unwrap_err();
    assert!(error.to_string().contains("not valid UTF-8"));
    assert!(!work
        .path()
        .join(".config/systemd/user/car-go-clean.service")
        .exists());
}

#[test]
fn legacy_definition_environment_is_unknown_not_equal() {
    let work = tempfile::tempdir().unwrap();
    let unit = work
        .path()
        .join(".config/systemd/user/car-go-clean.service");
    fs::create_dir_all(unit.parent().unwrap()).unwrap();
    fs::write(&unit, "[Service]\nExecStart=/bin/true\n").unwrap();
    let manager = test_manager(
        ServicePlatform::Linux,
        work.path(),
        work.path().join("bin/car-go-clean"),
    );

    assert_eq!(manager.installed_environment().unwrap(), None);
    assert_eq!(
        manager
            .environment_divergence(&environment(&[("HOME", "/home/tester")]))
            .unwrap(),
        None
    );
}

#[test]
fn mac_install_renders_an_escaped_absolute_binary_and_bootstraps_the_agent() {
    let work = tempfile::tempdir().unwrap();
    let binary = work.path().join("bin/car & go-clean");
    let mut manager = test_manager(ServicePlatform::MacOs, work.path(), binary.clone());

    manager.install().unwrap();
    let runner = manager.into_runner();

    let plist = fs::read_to_string(
        work.path()
            .join("Library/LaunchAgents/com.dcchuck.car-go-clean.plist"),
    )
    .unwrap();
    assert!(plist.contains(&binary.display().to_string().replace('&', "&amp;")));
    assert_eq!(runner.calls[0].1[0], "enable");
    assert_eq!(
        runner.calls[2].1[2].to_string_lossy(),
        format!("gui/{}/com.dcchuck.car-go-clean", unsafe {
            libc::geteuid()
        })
    );
}

#[test]
fn linux_install_writes_user_unit_and_enables_it_without_sudo() {
    let work = tempfile::tempdir().unwrap();
    let mut manager = test_manager(
        ServicePlatform::Linux,
        work.path(),
        work.path().join("bin/car-go-clean"),
    );

    manager.install().unwrap();
    let runner = manager.into_runner();

    let unit = fs::read_to_string(
        work.path()
            .join(".config/systemd/user/car-go-clean.service"),
    )
    .unwrap();
    assert!(unit.contains("ExecStart="));
    assert!(unit.contains("daemon"));
    assert!(runner.calls.iter().any(|(_, args)| {
        strings(args) == ["--user", "enable", "--now", "car-go-clean.service"]
    }));
    assert!(!runner
        .calls
        .iter()
        .any(|(program, _)| program == Path::new("sudo")));
}

#[test]
fn uninstall_stops_and_removes_only_the_expected_service_file() {
    let work = tempfile::tempdir().unwrap();
    let expected = work
        .path()
        .join(".config/systemd/user/car-go-clean.service");
    let other = work.path().join(".config/systemd/user/other.service");
    fs::create_dir_all(expected.parent().unwrap()).unwrap();
    fs::write(&expected, "unit").unwrap();
    fs::write(&other, "other").unwrap();

    let config = work.path().join(".config/car-go-clean/config.toml");
    let state = work.path().join(".local/state/car-go-clean/state.db");
    fs::create_dir_all(config.parent().unwrap()).unwrap();
    fs::create_dir_all(state.parent().unwrap()).unwrap();
    fs::write(&config, "scan_dirs=[]").unwrap();
    fs::write(&state, "state").unwrap();

    let mut manager = ServiceManager::new(
        ServicePlatform::Linux,
        work.path().to_path_buf(),
        work.path().join("bin/car-go-clean"),
        FakeRunner::with_outputs([
            CommandOutput::new(true, String::new(), String::new()),
            CommandOutput::new(true, "enabled\n".to_string(), String::new()),
            CommandOutput::new(true, "active\n".to_string(), String::new()),
            CommandOutput::new(true, String::new(), String::new()),
            CommandOutput::new(true, String::new(), String::new()),
        ]),
    );
    manager.uninstall().unwrap();
    let runner = manager.into_runner();

    assert!(!expected.exists());
    assert!(other.exists());
    assert!(config.exists());
    assert!(state.exists());
    assert!(runner.calls.iter().any(|(_, args)| {
        strings(args) == ["--user", "disable", "--now", "car-go-clean.service"]
    }));
}

#[test]
fn linux_uninstall_keeps_the_unit_when_disable_fails_unexpectedly() {
    let work = tempfile::tempdir().unwrap();
    let unit = work
        .path()
        .join(".config/systemd/user/car-go-clean.service");
    fs::create_dir_all(unit.parent().unwrap()).unwrap();
    fs::write(&unit, "unit").unwrap();
    let mut manager = ServiceManager::new(
        ServicePlatform::Linux,
        work.path().to_path_buf(),
        work.path().join("bin/car-go-clean"),
        FakeRunner {
            outputs: [
                CommandOutput::new(true, String::new(), String::new()),
                CommandOutput::new(true, "enabled\n".to_string(), String::new()),
                CommandOutput::new(true, "active\n".to_string(), String::new()),
            ]
            .into_iter()
            .collect(),
            disable_output: Some(CommandOutput::new(
                false,
                String::new(),
                "Failed to connect to bus: Permission denied".to_string(),
            )),
            ..FakeRunner::default()
        },
    );

    let error = manager.uninstall().unwrap_err();
    assert!(error.to_string().contains("Permission denied"));
    assert!(unit.exists());
}

#[test]
fn linux_uninstall_allows_an_already_missing_systemd_unit() {
    let work = tempfile::tempdir().unwrap();
    let unit = work
        .path()
        .join(".config/systemd/user/car-go-clean.service");
    fs::create_dir_all(unit.parent().unwrap()).unwrap();
    fs::write(&unit, "unit").unwrap();
    let mut manager = ServiceManager::new(
        ServicePlatform::Linux,
        work.path().to_path_buf(),
        work.path().join("bin/car-go-clean"),
        FakeRunner {
            outputs: [
                CommandOutput::new(true, String::new(), String::new()),
                CommandOutput::new(false, "not-found\n".to_string(), String::new()),
                CommandOutput::new(false, "unknown\n".to_string(), String::new()),
            ]
            .into_iter()
            .collect(),
            disable_output: Some(CommandOutput::new(
                false,
                String::new(),
                "Failed to disable unit: Unit file car-go-clean.service does not exist."
                    .to_string(),
            )),
            ..FakeRunner::default()
        },
    );

    manager.uninstall().unwrap();
    assert!(!unit.exists());
}

#[test]
fn mac_uninstall_keeps_the_plist_when_bootout_fails_unexpectedly() {
    let work = tempfile::tempdir().unwrap();
    let plist = work
        .path()
        .join("Library/LaunchAgents/com.dcchuck.car-go-clean.plist");
    fs::create_dir_all(plist.parent().unwrap()).unwrap();
    fs::write(&plist, "plist").unwrap();
    let mut manager = ServiceManager::new(
        ServicePlatform::MacOs,
        work.path().to_path_buf(),
        work.path().join("bin/car-go-clean"),
        FakeRunner {
            outputs: [
                CommandOutput::new(
                    true,
                    "disabled services = {\n}\n".to_string(),
                    String::new(),
                ),
                CommandOutput::new(true, String::new(), String::new()),
            ]
            .into_iter()
            .collect(),
            bootout_output: Some(CommandOutput::new(
                false,
                String::new(),
                "Boot-out failed: 1: Operation not permitted".to_string(),
            )),
            ..FakeRunner::default()
        },
    );

    let error = manager.uninstall().unwrap_err();
    assert!(error.to_string().contains("Operation not permitted"));
    assert!(plist.exists());
}

#[test]
fn mac_uninstall_disables_before_bootout_and_retains_config_and_state() {
    let work = tempfile::tempdir().unwrap();
    let plist = work
        .path()
        .join("Library/LaunchAgents/com.dcchuck.car-go-clean.plist");
    let config = work.path().join(".config/car-go-clean/config.toml");
    let state = work.path().join(".local/state/car-go-clean/state.db");
    fs::create_dir_all(plist.parent().unwrap()).unwrap();
    fs::create_dir_all(config.parent().unwrap()).unwrap();
    fs::create_dir_all(state.parent().unwrap()).unwrap();
    fs::write(&plist, "definition").unwrap();
    fs::write(&config, "scan_dirs=[]").unwrap();
    fs::write(&state, "state").unwrap();
    let mut manager = ServiceManager::new(
        ServicePlatform::MacOs,
        work.path().to_path_buf(),
        work.path().join("bin/car-go-clean"),
        FakeRunner::with_outputs([
            CommandOutput::new(
                true,
                "disabled services = {\n}\n".to_string(),
                String::new(),
            ),
            CommandOutput::new(true, String::new(), String::new()),
            CommandOutput::new(true, String::new(), String::new()),
            CommandOutput::new(true, String::new(), String::new()),
        ]),
    );

    assert_eq!(
        manager.uninstall().unwrap(),
        ServiceStatus {
            installed: false,
            enabled: false,
            active: false,
        }
    );
    assert!(!plist.exists());
    assert!(config.exists());
    assert!(state.exists());
    let calls = call_arguments(&manager.into_runner());
    assert_eq!(calls[2][0], "disable");
    assert_eq!(calls[3][0], "bootout");
}

#[test]
fn status_is_not_installed_without_running_a_platform_command() {
    let work = tempfile::tempdir().unwrap();
    let mut manager = test_manager(
        ServicePlatform::MacOs,
        work.path(),
        work.path().join("bin/car-go-clean"),
    );

    assert_eq!(
        manager.status().unwrap(),
        ServiceStatus {
            installed: false,
            enabled: false,
            active: false,
        }
    );
    assert!(manager.into_runner().calls.is_empty());

    let mut linux = test_manager(
        ServicePlatform::Linux,
        work.path(),
        work.path().join("bin/car-go-clean"),
    );
    assert_eq!(
        linux.status().unwrap(),
        ServiceStatus {
            installed: false,
            enabled: false,
            active: false,
        }
    );
    assert!(linux.into_runner().calls.is_empty());
}

#[test]
fn linux_reports_when_systemd_user_is_unavailable() {
    let work = tempfile::tempdir().unwrap();
    let unit = work
        .path()
        .join(".config/systemd/user/car-go-clean.service");
    fs::create_dir_all(unit.parent().unwrap()).unwrap();
    fs::write(unit, "unit").unwrap();
    let mut manager = ServiceManager::new(
        ServicePlatform::Linux,
        work.path().to_path_buf(),
        work.path().join("bin/car-go-clean"),
        FakeRunner {
            fail_systemd_environment: true,
            ..FakeRunner::default()
        },
    );

    assert!(manager
        .status()
        .unwrap_err()
        .to_string()
        .contains("systemd --user is unavailable"));
}

#[test]
fn start_requires_an_installed_definition() {
    let work = tempfile::tempdir().unwrap();
    let mut manager = test_manager(
        ServicePlatform::MacOs,
        work.path(),
        work.path().join("bin/car-go-clean"),
    );
    let error = manager.start().unwrap_err();
    assert!(error.to_string().contains("not installed"));
    assert!(manager.into_runner().calls.is_empty());
}

#[test]
fn start_and_stop_are_idempotent_for_current_state() {
    let work = tempfile::tempdir().unwrap();
    let plist = work
        .path()
        .join("Library/LaunchAgents/com.dcchuck.car-go-clean.plist");
    fs::create_dir_all(plist.parent().unwrap()).unwrap();
    fs::write(&plist, "plist").unwrap();

    let mut active = ServiceManager::new(
        ServicePlatform::MacOs,
        work.path().to_path_buf(),
        work.path().join("bin/car-go-clean"),
        FakeRunner::with_outputs([
            CommandOutput::new(
                true,
                "disabled services = {\n}\n".to_string(),
                String::new(),
            ),
            CommandOutput::new(true, String::new(), String::new()),
        ]),
    );
    assert!(active.start().unwrap().active);
    assert_eq!(active.into_runner().calls.len(), 2);

    let mut inactive = ServiceManager::new(
        ServicePlatform::MacOs,
        work.path().to_path_buf(),
        work.path().join("bin/car-go-clean"),
        FakeRunner::with_outputs([
            CommandOutput::new(
                true,
                "disabled services = {\n  \"com.dcchuck.car-go-clean\" => true\n}\n".to_string(),
                String::new(),
            ),
            CommandOutput::new(
                false,
                String::new(),
                "Could not find specified service".to_string(),
            ),
        ]),
    );
    assert!(!inactive.stop().unwrap().active);
    assert_eq!(inactive.into_runner().calls.len(), 2);
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum MatrixState {
    Absent,
    EnabledActive,
    EnabledInactive,
    DisabledActive,
    DisabledInactive,
}

#[derive(Clone, Copy, Debug)]
enum MatrixAction {
    Status,
    Install,
    Start,
    Stop,
    Restart,
    Uninstall,
}

fn matrix_definition(platform: ServicePlatform, home: &Path) -> PathBuf {
    match platform {
        ServicePlatform::MacOs => home
            .join("Library/LaunchAgents")
            .join("com.dcchuck.car-go-clean.plist"),
        ServicePlatform::Linux => home.join(".config/systemd/user/car-go-clean.service"),
    }
}

fn matrix_status(state: MatrixState) -> ServiceStatus {
    match state {
        MatrixState::Absent => ServiceStatus {
            installed: false,
            enabled: false,
            active: false,
        },
        MatrixState::EnabledActive => ServiceStatus {
            installed: true,
            enabled: true,
            active: true,
        },
        MatrixState::EnabledInactive => ServiceStatus {
            installed: true,
            enabled: true,
            active: false,
        },
        MatrixState::DisabledActive => ServiceStatus {
            installed: true,
            enabled: false,
            active: true,
        },
        MatrixState::DisabledInactive => ServiceStatus {
            installed: true,
            enabled: false,
            active: false,
        },
    }
}

fn matrix_runner(
    platform: ServicePlatform,
    state: MatrixState,
    action: MatrixAction,
) -> FakeRunner {
    if state == MatrixState::Absent {
        return FakeRunner::default();
    }
    let enabled = matches!(
        state,
        MatrixState::EnabledActive | MatrixState::EnabledInactive
    );
    let active = matches!(
        state,
        MatrixState::EnabledActive | MatrixState::DisabledActive
    );
    match platform {
        ServicePlatform::MacOs => FakeRunner::with_outputs([
            CommandOutput::new(
                true,
                if enabled {
                    "disabled services = {\n}\n".to_string()
                } else {
                    "disabled services = {\n  \"com.dcchuck.car-go-clean\" => true\n}\n".to_string()
                },
                String::new(),
            ),
            if active {
                CommandOutput::new(true, String::new(), String::new())
            } else {
                CommandOutput::new(
                    false,
                    String::new(),
                    "Could not find specified service".to_string(),
                )
            },
        ]),
        ServicePlatform::Linux => {
            let enabled_output = || {
                CommandOutput::new(
                    enabled,
                    if enabled {
                        "enabled\n".to_string()
                    } else {
                        "disabled\n".to_string()
                    },
                    String::new(),
                )
            };
            let mut outputs = vec![
                CommandOutput::new(true, String::new(), String::new()),
                enabled_output(),
                CommandOutput::new(
                    active,
                    if active {
                        "active\n".to_string()
                    } else {
                        "inactive\n".to_string()
                    },
                    String::new(),
                ),
            ];
            if matches!(action, MatrixAction::Install) && active {
                outputs.extend([
                    CommandOutput::new(true, String::new(), String::new()),
                    CommandOutput::new(true, String::new(), String::new()),
                    enabled_output(),
                    CommandOutput::new(false, "inactive\n".to_string(), String::new()),
                ]);
            }
            FakeRunner::with_outputs(outputs)
        }
    }
}

fn matrix_status_calls(platform: ServicePlatform, state: MatrixState) -> Vec<Vec<String>> {
    if state == MatrixState::Absent {
        return Vec::new();
    }
    match platform {
        ServicePlatform::MacOs => vec![
            vec![
                "print-disabled".to_string(),
                format!("gui/{}", unsafe { libc::geteuid() }),
            ],
            vec![
                "print".to_string(),
                format!("gui/{}/com.dcchuck.car-go-clean", unsafe {
                    libc::geteuid()
                }),
            ],
        ],
        ServicePlatform::Linux => vec![
            vec!["--user".to_string(), "show-environment".to_string()],
            vec![
                "--user".to_string(),
                "is-enabled".to_string(),
                "car-go-clean.service".to_string(),
            ],
            vec![
                "--user".to_string(),
                "is-active".to_string(),
                "car-go-clean.service".to_string(),
            ],
        ],
    }
}

fn matrix_action_calls(
    platform: ServicePlatform,
    state: MatrixState,
    action: MatrixAction,
    definition: &Path,
) -> Vec<Vec<String>> {
    let enabled = matches!(
        state,
        MatrixState::EnabledActive | MatrixState::EnabledInactive
    );
    let active = matches!(
        state,
        MatrixState::EnabledActive | MatrixState::DisabledActive
    );
    let installed = state != MatrixState::Absent;
    let mut calls = if installed {
        matrix_status_calls(platform, state)
    } else {
        Vec::new()
    };
    let uid = unsafe { libc::geteuid() };
    let domain = format!("gui/{uid}");
    let target = format!("{domain}/com.dcchuck.car-go-clean");
    let enable = || vec!["enable".to_string(), target.clone()];
    let disable = || vec!["disable".to_string(), target.clone()];
    let bootstrap = || {
        vec![
            "bootstrap".to_string(),
            domain.clone(),
            definition.display().to_string(),
        ]
    };
    let bootout = || {
        vec![
            "bootout".to_string(),
            domain.clone(),
            definition.display().to_string(),
        ]
    };
    let kickstart = || vec!["kickstart".to_string(), "-k".to_string(), target.clone()];
    let show_environment = || vec!["--user".to_string(), "show-environment".to_string()];
    let daemon_reload = || vec!["--user".to_string(), "daemon-reload".to_string()];
    let enable_now = || {
        vec![
            "--user".to_string(),
            "enable".to_string(),
            "--now".to_string(),
            "car-go-clean.service".to_string(),
        ]
    };
    let disable_now = || {
        vec![
            "--user".to_string(),
            "disable".to_string(),
            "--now".to_string(),
            "car-go-clean.service".to_string(),
        ]
    };
    let restart = || {
        vec![
            "--user".to_string(),
            "restart".to_string(),
            "car-go-clean.service".to_string(),
        ]
    };
    let stop = || {
        vec![
            "--user".to_string(),
            "stop".to_string(),
            "car-go-clean.service".to_string(),
        ]
    };

    match (platform, action) {
        (_, MatrixAction::Status) => {}
        (ServicePlatform::MacOs, MatrixAction::Install) => {
            if active {
                calls.push(bootout());
            }
            calls.extend([enable(), bootstrap(), kickstart()]);
        }
        (ServicePlatform::Linux, MatrixAction::Install) => {
            if active {
                calls.push(stop());
                calls.extend(matrix_status_calls(platform, state));
            }
            calls.extend([show_environment(), daemon_reload(), enable_now()]);
        }
        (ServicePlatform::MacOs, MatrixAction::Start) if installed => {
            if !enabled {
                calls.push(enable());
            }
            if !active {
                calls.extend([bootstrap(), kickstart()]);
            }
        }
        (ServicePlatform::Linux, MatrixAction::Start) if installed && !(enabled && active) => {
            calls.extend([daemon_reload(), enable_now()]);
        }
        (ServicePlatform::MacOs, MatrixAction::Stop) if installed => {
            if enabled {
                calls.push(disable());
            }
            if active {
                calls.push(bootout());
            }
        }
        (ServicePlatform::Linux, MatrixAction::Stop) if installed && (enabled || active) => {
            calls.push(disable_now());
        }
        (ServicePlatform::MacOs, MatrixAction::Restart) if installed && enabled => {
            if !active {
                calls.push(bootstrap());
            }
            calls.push(kickstart());
        }
        (ServicePlatform::Linux, MatrixAction::Restart) if installed && enabled => {
            calls.push(restart());
        }
        (ServicePlatform::MacOs, MatrixAction::Uninstall) if installed => {
            if enabled {
                calls.push(disable());
            }
            if active {
                calls.push(bootout());
            }
        }
        (ServicePlatform::Linux, MatrixAction::Uninstall) if installed => {
            calls.extend([disable_now(), daemon_reload()]);
        }
        _ => {}
    }
    calls
}

#[test]
fn lifecycle_matrix_covers_every_definition_enablement_and_activity_state() {
    let states = [
        MatrixState::Absent,
        MatrixState::EnabledActive,
        MatrixState::EnabledInactive,
        MatrixState::DisabledActive,
        MatrixState::DisabledInactive,
    ];
    let actions = [
        MatrixAction::Status,
        MatrixAction::Install,
        MatrixAction::Start,
        MatrixAction::Stop,
        MatrixAction::Restart,
        MatrixAction::Uninstall,
    ];
    let mut cells = 0;

    for platform in [ServicePlatform::MacOs, ServicePlatform::Linux] {
        for state in states {
            for action in actions {
                let work = tempfile::tempdir().unwrap();
                let definition = matrix_definition(platform, work.path());
                if state != MatrixState::Absent {
                    fs::create_dir_all(definition.parent().unwrap()).unwrap();
                    fs::write(&definition, "definition").unwrap();
                }
                let mut manager = ServiceManager::new(
                    platform,
                    work.path().to_path_buf(),
                    work.path().join("bin/car-go-clean"),
                    matrix_runner(platform, state, action),
                );

                let result = match action {
                    MatrixAction::Status => manager.status(),
                    MatrixAction::Install => manager.install(),
                    MatrixAction::Start => manager.start(),
                    MatrixAction::Stop => manager.stop(),
                    MatrixAction::Restart => manager.restart(),
                    MatrixAction::Uninstall => manager.uninstall(),
                };
                let initial = matrix_status(state);
                let expected = match action {
                    MatrixAction::Status => Ok(initial.clone()),
                    MatrixAction::Install => Ok(ServiceStatus {
                        installed: true,
                        enabled: true,
                        active: true,
                    }),
                    MatrixAction::Start if !initial.installed => {
                        Err("car-go-clean service is not installed")
                    }
                    MatrixAction::Start => Ok(ServiceStatus {
                        installed: true,
                        enabled: true,
                        active: true,
                    }),
                    MatrixAction::Stop if !initial.installed => Ok(initial.clone()),
                    MatrixAction::Stop => Ok(ServiceStatus {
                        installed: true,
                        enabled: false,
                        active: false,
                    }),
                    MatrixAction::Restart if !initial.installed => {
                        Err("car-go-clean service is not installed")
                    }
                    MatrixAction::Restart if !initial.enabled => {
                        Err("car-go-clean service is not enabled")
                    }
                    MatrixAction::Restart => Ok(ServiceStatus {
                        installed: true,
                        enabled: true,
                        active: true,
                    }),
                    MatrixAction::Uninstall => Ok(ServiceStatus {
                        installed: false,
                        enabled: false,
                        active: false,
                    }),
                };
                match expected {
                    Ok(expected) => assert_eq!(
                        result.unwrap(),
                        expected,
                        "{platform:?} {state:?} {action:?}"
                    ),
                    Err(message) => assert!(
                        result.unwrap_err().to_string().contains(message),
                        "{platform:?} {state:?} {action:?}"
                    ),
                }

                assert_eq!(
                    call_arguments(&manager.into_runner()),
                    matrix_action_calls(platform, state, action, &definition),
                    "{platform:?} {state:?} {action:?}"
                );
                let definition_should_exist = match action {
                    MatrixAction::Install => true,
                    MatrixAction::Uninstall => false,
                    _ => initial.installed,
                };
                assert_eq!(
                    definition.exists(),
                    definition_should_exist,
                    "{platform:?} {state:?} {action:?}"
                );
                cells += 1;
            }
        }
    }

    assert_eq!(cells, 60);
}

#[test]
fn lifecycle_reports_unexpected_status_probe_failure() {
    let work = tempfile::tempdir().unwrap();
    let plist = work
        .path()
        .join("Library/LaunchAgents/com.dcchuck.car-go-clean.plist");
    fs::create_dir_all(plist.parent().unwrap()).unwrap();
    fs::write(&plist, "plist").unwrap();
    let mut manager = ServiceManager::new(
        ServicePlatform::MacOs,
        work.path().to_path_buf(),
        work.path().join("bin/car-go-clean"),
        FakeRunner::with_outputs([CommandOutput::new(
            true,
            "not a launchctl dictionary".to_string(),
            String::new(),
        )]),
    );

    let error = manager.stop().unwrap_err();
    assert!(error
        .to_string()
        .contains("malformed launchctl print-disabled output"));
    assert!(plist.exists());
    assert_eq!(manager.into_runner().calls.len(), 1);
}

#[cfg(unix)]
#[test]
fn path_resolved_absolute_binary_wins_over_current_exe() {
    use std::os::unix::fs::PermissionsExt;

    let work = tempfile::tempdir().unwrap();
    let bin_dir = work.path().join("bin");
    let path_binary = bin_dir.join("car-go-clean");
    let current_exe = work.path().join("current/car-go-clean");
    fs::create_dir_all(&bin_dir).unwrap();
    fs::create_dir_all(current_exe.parent().unwrap()).unwrap();
    fs::write(&path_binary, "#!/bin/sh\n").unwrap();
    fs::write(&current_exe, "#!/bin/sh\n").unwrap();
    fs::set_permissions(&path_binary, fs::Permissions::from_mode(0o755)).unwrap();
    fs::set_permissions(&current_exe, fs::Permissions::from_mode(0o755)).unwrap();

    let resolved = resolve_service_binary(
        OsString::from("car-go-clean").as_os_str(),
        Some(bin_dir.as_os_str()),
        current_exe,
    )
    .unwrap();
    assert_eq!(resolved, path_binary);
}

#[cfg(unix)]
#[test]
fn bare_argv0_ignores_a_non_executable_current_directory_impostor() {
    use std::os::unix::fs::PermissionsExt;

    let current_dir = std::env::current_dir().unwrap();
    let impostor = tempfile::Builder::new()
        .prefix("car-go-clean-impostor-")
        .tempfile_in(&current_dir)
        .unwrap();
    fs::set_permissions(impostor.path(), fs::Permissions::from_mode(0o644)).unwrap();
    let name = impostor.path().file_name().unwrap().to_os_string();

    let work = tempfile::tempdir().unwrap();
    let bin_dir = work.path().join("bin");
    let path_binary = bin_dir.join(&name);
    let current_exe = work.path().join("current/car-go-clean");
    fs::create_dir_all(&bin_dir).unwrap();
    fs::create_dir_all(current_exe.parent().unwrap()).unwrap();
    fs::write(&path_binary, "#!/bin/sh\n").unwrap();
    fs::write(&current_exe, "#!/bin/sh\n").unwrap();
    fs::set_permissions(&path_binary, fs::Permissions::from_mode(0o755)).unwrap();
    fs::set_permissions(&current_exe, fs::Permissions::from_mode(0o755)).unwrap();

    let resolved =
        resolve_service_binary(name.as_os_str(), Some(bin_dir.as_os_str()), current_exe).unwrap();
    assert_eq!(resolved, path_binary);
}

#[cfg(unix)]
#[test]
fn non_executable_absolute_argv0_falls_back_to_current_exe() {
    use std::os::unix::fs::PermissionsExt;

    let work = tempfile::tempdir().unwrap();
    let argv0 = work.path().join("argv0/car-go-clean");
    let current_exe = work.path().join("current/car-go-clean");
    fs::create_dir_all(argv0.parent().unwrap()).unwrap();
    fs::create_dir_all(current_exe.parent().unwrap()).unwrap();
    fs::write(&argv0, "#!/bin/sh\n").unwrap();
    fs::write(&current_exe, "#!/bin/sh\n").unwrap();
    fs::set_permissions(&argv0, fs::Permissions::from_mode(0o644)).unwrap();
    fs::set_permissions(&current_exe, fs::Permissions::from_mode(0o755)).unwrap();

    let resolved = resolve_service_binary(argv0.as_os_str(), None, current_exe.clone()).unwrap();
    assert_eq!(resolved, current_exe);
}
