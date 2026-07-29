use anyhow::Result;
use car_go_clean::service::{
    resolve_service_binary, CommandOutput, CommandRunner, ServiceManager, ServicePlatform,
    ServiceStatus,
};
use std::collections::VecDeque;
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
    assert_eq!(runner.calls[0].1[0], "bootout");
    assert_eq!(
        runner.calls[0].1[1].to_string_lossy(),
        format!("gui/{}", unsafe { libc::geteuid() })
    );
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
fn restart_uses_platform_specific_user_service_command() {
    let work = tempfile::tempdir().unwrap();
    let mut mac = test_manager(
        ServicePlatform::MacOs,
        work.path(),
        work.path().join("bin/car-go-clean"),
    );
    mac.restart().unwrap();
    assert_eq!(strings(&mac.into_runner().calls[0].1)[0], "kickstart");

    let mut linux = test_manager(
        ServicePlatform::Linux,
        work.path(),
        work.path().join("bin/car-go-clean"),
    );
    linux.restart().unwrap();
    assert_eq!(
        strings(&linux.into_runner().calls[1].1),
        ["--user", "restart", "car-go-clean.service"]
    );
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

    let mut manager = test_manager(
        ServicePlatform::Linux,
        work.path(),
        work.path().join("bin/car-go-clean"),
    );
    manager.uninstall().unwrap();
    let runner = manager.into_runner();

    assert!(!expected.exists());
    assert!(other.exists());
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
fn mac_uninstall_removes_the_plist_when_service_is_not_loaded() {
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
            bootout_output: Some(CommandOutput::new(
                false,
                String::new(),
                "Boot-out failed: 3: No such process".to_string(),
            )),
            ..FakeRunner::default()
        },
    );

    manager.uninstall().unwrap();
    assert!(!plist.exists());
}

#[test]
fn status_is_not_installed_without_running_a_platform_command() {
    let work = tempfile::tempdir().unwrap();
    let mut manager = test_manager(
        ServicePlatform::MacOs,
        work.path(),
        work.path().join("bin/car-go-clean"),
    );

    let status = manager.status().unwrap();
    assert!(!status.installed);
    assert!(manager.into_runner().calls.is_empty());
}

#[test]
fn linux_reports_when_systemd_user_is_unavailable() {
    let work = tempfile::tempdir().unwrap();
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
fn mac_stop_preserves_plist_and_start_bootstraps_it() {
    let work = tempfile::tempdir().unwrap();
    let plist = work
        .path()
        .join("Library/LaunchAgents/com.dcchuck.car-go-clean.plist");
    fs::create_dir_all(plist.parent().unwrap()).unwrap();
    fs::write(&plist, "plist").unwrap();

    let mut stop = ServiceManager::new(
        ServicePlatform::MacOs,
        work.path().to_path_buf(),
        work.path().join("bin/car-go-clean"),
        FakeRunner::with_outputs([
            CommandOutput::new(true, String::new(), String::new()),
            CommandOutput::new(true, String::new(), String::new()),
        ]),
    );
    assert_eq!(
        stop.stop().unwrap(),
        ServiceStatus {
            installed: true,
            active: false,
        }
    );
    let stop_runner = stop.into_runner();
    assert_eq!(strings(&stop_runner.calls[0].1)[0], "print");
    assert_eq!(strings(&stop_runner.calls[1].1)[0], "bootout");
    assert!(plist.exists());

    let mut start = ServiceManager::new(
        ServicePlatform::MacOs,
        work.path().to_path_buf(),
        work.path().join("bin/car-go-clean"),
        FakeRunner::with_outputs([
            CommandOutput::new(
                false,
                String::new(),
                "Could not find specified service".to_string(),
            ),
            CommandOutput::new(true, String::new(), String::new()),
            CommandOutput::new(true, String::new(), String::new()),
        ]),
    );
    assert_eq!(
        start.start().unwrap(),
        ServiceStatus {
            installed: true,
            active: true,
        }
    );
    let start_runner = start.into_runner();
    assert_eq!(strings(&start_runner.calls[0].1)[0], "print");
    assert_eq!(strings(&start_runner.calls[1].1)[0], "bootstrap");
    assert_eq!(strings(&start_runner.calls[2].1)[0], "kickstart");
}

#[test]
fn linux_stop_and_start_use_user_service_commands() {
    let work = tempfile::tempdir().unwrap();
    let unit = work
        .path()
        .join(".config/systemd/user/car-go-clean.service");
    fs::create_dir_all(unit.parent().unwrap()).unwrap();
    fs::write(&unit, "unit").unwrap();

    let mut stop = ServiceManager::new(
        ServicePlatform::Linux,
        work.path().to_path_buf(),
        work.path().join("bin/car-go-clean"),
        FakeRunner::with_outputs([
            CommandOutput::new(true, String::new(), String::new()),
            CommandOutput::new(true, String::new(), String::new()),
            CommandOutput::new(true, String::new(), String::new()),
        ]),
    );
    stop.stop().unwrap();
    let stop_runner = stop.into_runner();
    assert!(stop_runner
        .calls
        .iter()
        .any(|(_, args)| { strings(args) == ["--user", "stop", "car-go-clean.service"] }));
    assert!(!stop_runner
        .calls
        .iter()
        .any(|(program, _)| program == Path::new("sudo")));

    let mut start = ServiceManager::new(
        ServicePlatform::Linux,
        work.path().to_path_buf(),
        work.path().join("bin/car-go-clean"),
        FakeRunner::with_outputs([
            CommandOutput::new(true, String::new(), String::new()),
            CommandOutput::new(false, "inactive\n".to_string(), String::new()),
            CommandOutput::new(true, String::new(), String::new()),
        ]),
    );
    start.start().unwrap();
    let start_runner = start.into_runner();
    assert!(start_runner
        .calls
        .iter()
        .any(|(_, args)| { strings(args) == ["--user", "start", "car-go-clean.service"] }));
    assert!(!start_runner
        .calls
        .iter()
        .any(|(program, _)| program == Path::new("sudo")));
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
        FakeRunner::with_outputs([CommandOutput::new(true, String::new(), String::new())]),
    );
    assert!(active.start().unwrap().active);
    assert_eq!(active.into_runner().calls.len(), 1);

    let mut inactive = ServiceManager::new(
        ServicePlatform::MacOs,
        work.path().to_path_buf(),
        work.path().join("bin/car-go-clean"),
        FakeRunner::with_outputs([CommandOutput::new(
            false,
            String::new(),
            "Could not find specified service".to_string(),
        )]),
    );
    assert!(!inactive.stop().unwrap().active);
    assert_eq!(inactive.into_runner().calls.len(), 1);
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
            false,
            String::new(),
            "Operation not permitted".to_string(),
        )]),
    );

    let error = manager.stop().unwrap_err();
    assert!(error.to_string().contains("Operation not permitted"));
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
