use std::fs;
use std::path::Path;

fn repo_file(path: &str) -> String {
    fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join(path)).unwrap()
}

#[test]
fn systemd_service_runs_daemon_with_configurable_paths() {
    let service = repo_file("packaging/systemd/car-go-clean.service");

    assert!(service.contains("ExecStart="));
    assert!(service.contains("car-go-clean daemon"));
    assert!(service.contains("CAR_GO_CLEAN_CONFIG"));
}

#[test]
fn launchd_plist_runs_daemon_with_configurable_paths() {
    let plist = repo_file("packaging/launchd/com.dcchuck.car-go-clean.plist");

    assert!(plist.contains("<key>ProgramArguments</key>"));
    assert!(plist.contains("__CAR_GO_CLEAN_BIN__"));
    assert!(plist.contains("__CAR_GO_CLEAN_LOG_DIR__"));
    assert!(plist.contains("daemon"));
    assert!(!plist.contains("/Users/charlesdanielsson"));
    assert!(!plist.contains("/usr/local/bin/car-go-clean"));
    assert!(!plist.contains("/tmp/car-go-clean.launchd"));
}

#[test]
fn launchd_installer_renders_user_specific_plist() {
    let installer = repo_file("packaging/launchd/install.sh");

    assert!(installer.contains("CAR_GO_CLEAN_BIN"));
    assert!(installer.contains("CAR_GO_CLEAN_LOG_DIR"));
    assert!(installer.contains("Library/LaunchAgents"));
    assert!(installer.contains("launchctl bootstrap"));
}

#[test]
fn release_packaging_documents_cargo_install_as_primary_channel() {
    let release = repo_file("packaging/release/README.md");

    assert!(release.contains("Primary distribution channel: `cargo install`"));
    assert!(release.contains("Homebrew"));
}

#[test]
fn cargo_dist_metadata_declares_the_public_release_contract() {
    let manifest = repo_file("Cargo.toml");
    let dist = repo_file("dist-workspace.toml");
    for value in [
        "version = \"0.2.0\"",
        "repository = \"https://github.com/dcchuck/car-go-clean\"",
        "homepage = \"https://github.com/dcchuck/car-go-clean\"",
    ] {
        assert!(manifest.contains(value), "missing {value}");
    }
    for value in [
        "cargo-dist-version = \"0.32.0\"",
        "aarch64-apple-darwin",
        "x86_64-apple-darwin",
        "aarch64-unknown-linux-musl",
        "x86_64-unknown-linux-musl",
        "github-attestations = true",
        "tap = \"dcchuck/homebrew-tap\"",
        "publish-jobs = [\"homebrew\", \"./publish-shell-installer\"]",
        "github-custom-job-permissions = { \"publish-shell-installer\" = { contents = \"write\", attestations = \"write\", id-token = \"write\" } }",
    ] {
        assert!(dist.contains(value), "missing {value}");
    }
}

#[test]
fn release_workflow_is_tag_only_and_uses_dist() {
    let workflow = repo_file(".github/workflows/release.yml");
    assert!(workflow.contains("push:"));
    assert!(workflow.contains("tags:"));
    assert!(!workflow.contains("pull_request:"));
    assert!(workflow.contains("dist plan"));
    assert!(workflow.contains("dist build"));
    assert!(workflow.contains("HOMEBREW_TAP_TOKEN"));
    assert!(workflow.contains("\"attestations\": \"write\""));
    assert!(workflow.contains("Enforce annotated vX.Y.Z release tag"));
}
