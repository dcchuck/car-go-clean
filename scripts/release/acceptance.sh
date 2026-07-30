#!/bin/sh
set -eu

usage() {
    echo "usage: $0 /absolute/path/to/artifacts /absolute/path/to/evidence pre-reboot|post-reboot" >&2
}

die() {
    echo "guest acceptance: $*" >&2
    exit 1
}

test "$#" -eq 3 || {
    usage
    exit 2
}
artifact_dir=$1
evidence_dir=$2
phase=$3
case "$artifact_dir:$evidence_dir" in
    /*:/*) ;;
    *) die "artifact and evidence paths must be absolute guest-local paths" ;;
esac
case "$phase" in
    pre-reboot|post-reboot) ;;
    *) usage; exit 2 ;;
esac
test -d "$artifact_dir" || die "artifact directory does not exist"

version=${CAR_GO_CLEAN_ACCEPTANCE_VERSION-}
case "$version" in
    ''|*[!0-9.]*) die "CAR_GO_CLEAN_ACCEPTANCE_VERSION must be X.Y.Z" ;;
esac
test "$(printf '%s\n' "$version" | awk -F . 'NF == 3 &&
    $1 ~ /^[0-9]+$/ && $2 ~ /^[0-9]+$/ && $3 ~ /^[0-9]+$/ { print "valid" }')" = valid ||
    die "CAR_GO_CLEAN_ACCEPTANCE_VERSION must be X.Y.Z"

exact_sha=${CAR_GO_CLEAN_ACCEPTANCE_SHA-}
case "$exact_sha" in
    *[!0-9a-f]*|'') die "CAR_GO_CLEAN_ACCEPTANCE_SHA must be lowercase hexadecimal" ;;
esac
test "${#exact_sha}" -eq 40 ||
    die "CAR_GO_CLEAN_ACCEPTANCE_SHA must be an exact 40-character Git commit"

command -v python3 >/dev/null 2>&1 || die "python3 is required"
mkdir -p "$evidence_dir"
chmod 700 "$evidence_dir"
raw_log=$evidence_dir/.$phase-transcript.raw
transcript=$evidence_dir/$phase-transcript.log
milestones=$evidence_dir/milestones.tsv
work_root=$HOME/car-go-clean-v040-acceptance-work
session_marker=$work_root/pre-reboot-complete

sanitize_transcript() {
    test -f "$raw_log" || : > "$raw_log"
    python3 - "$raw_log" "$transcript" "$artifact_dir" "$work_root" "$HOME" <<'PY'
import pathlib
import re
import sys

source, destination, artifacts, work, home = sys.argv[1:]
text = pathlib.Path(source).read_text(errors="replace")
for value, replacement in sorted(
    [(artifacts, "$ARTIFACT_DIR"), (work, "$WORK"), (home, "$HOME")],
    key=lambda item: len(item[0]),
    reverse=True,
):
    if value:
        text = text.replace(value, replacement)
text = re.sub(r"(?i)(authorization:\s*)(\S+)", r"\1<redacted>", text)
text = re.sub(r"(ghp_|github_pat_)[A-Za-z0-9_]+", r"\1<redacted>", text)
text = re.sub(r"(?i)(password[=:]\s*)(\S+)", r"\1<redacted>", text)
pathlib.Path(destination).write_text(text)
PY
    chmod 600 "$transcript"
}

on_exit() {
    status=$?
    trap - EXIT HUP INT TERM
    set +e
    sanitize_transcript
    rm -f "$raw_log"
    exit "$status"
}
trap on_exit EXIT HUP INT TERM

record_step() {
    step=$1
    shift
    current_acceptance_step=$step
    printf 'BEGIN %s\n' "$step" >> "$raw_log"
    set +e
    (
        set -e
        fault_checkpoint early
        "$@"
        fault_checkpoint late
    ) >> "$raw_log" 2>&1
    step_status=$?
    set -e
    if test "$step_status" -eq 0; then
        printf '%s\tPASS\n' "$step" >> "$milestones"
        printf 'PASS %s\n' "$step" >> "$raw_log"
        return 0
    fi
    printf '%s\tFAIL\texit=%s\n' "$step" "$step_status" >> "$milestones"
    printf 'FAIL %s exit=%s\n' "$step" "$step_status" >> "$raw_log"
    return "$step_status"
}

fault_checkpoint() {
    position=$1
    if test "${CAR_GO_CLEAN_ACCEPTANCE_FAULT-}" = \
        "$current_acceptance_step:$position"; then
        echo "injected acceptance failure: $current_acceptance_step:$position" >&2
        return 97
    fi
}

required_file() {
    test -f "$artifact_dir/$1" && test ! -L "$artifact_dir/$1" || {
        echo "required copied artifact is missing: $1" >&2
        return 1
    }
}

verify_artifact_set() {
    python3 - "$artifact_dir" <<'PY'
import hashlib
import os
import pathlib
import re
import stat
import sys

root = pathlib.Path(sys.argv[1])
expected = {
    "acceptance.sh",
    "car-go-clean-installer.sh",
    "car-go-clean-upgrade.sh",
    "car-go-clean-shell-assets.sha256",
    "car-go-clean.rb",
    "car-go-clean-aarch64-apple-darwin.tar.xz",
    "car-go-clean-aarch64-apple-darwin.tar.xz.sha256",
    "car-go-clean-aarch64-unknown-linux-musl.tar.xz",
    "car-go-clean-aarch64-unknown-linux-musl.tar.xz.sha256",
    "car-go-clean-v0.2.0-aarch64-apple-darwin",
    "car-go-clean-v0.3.0-aarch64-apple-darwin",
    "car-go-clean-v0.2.0-aarch64-unknown-linux-musl",
    "car-go-clean-v0.3.0-aarch64-unknown-linux-musl",
    "rustup-init-aarch64-apple-darwin",
    "rustup-init-aarch64-apple-darwin.sha256",
    "rustup-init-aarch64-unknown-linux-gnu",
    "rustup-init-aarch64-unknown-linux-gnu.sha256",
}
actual = set()
for entry in os.scandir(root):
    mode = entry.stat(follow_symlinks=False).st_mode
    if stat.S_ISLNK(mode) or not stat.S_ISREG(mode):
        raise SystemExit(f"artifact is not a regular non-symlink file: {entry.name}")
    actual.add(entry.name)
if actual != expected | {"SHA256SUMS"}:
    missing = sorted(expected - actual)
    extra = sorted(actual - expected - {"SHA256SUMS"})
    raise SystemExit(f"artifact set mismatch; missing={missing}, extra={extra}")

manifest = root / "SHA256SUMS"
seen = {}
for line_number, line in enumerate(manifest.read_text().splitlines(), 1):
    match = re.fullmatch(r"([0-9a-f]{64}) [ *]([^\r\n/]+)", line)
    if not match or match.group(2) in seen:
        raise SystemExit(f"malformed, nested, or duplicate SHA256SUMS line {line_number}")
    seen[match.group(2)] = match.group(1)
if set(seen) != expected:
    raise SystemExit("SHA256SUMS names do not equal the closed artifact allowlist")
for name, expected_hash in seen.items():
    digest = hashlib.sha256((root / name).read_bytes()).hexdigest()
    if digest != expected_hash:
        raise SystemExit(f"SHA256 mismatch for {name}")
PY
}

assert_output_has() {
    haystack=$1
    needle=$2
    printf '%s\n' "$haystack" | grep -F -- "$needle" >/dev/null || {
        echo "expected output to contain: $needle" >&2
        return 1
    }
}

assert_output_line() {
    haystack=$1
    expected=$2
    printf '%s\n' "$haystack" | grep -Fx -- "$expected" >/dev/null || {
        echo "expected output to contain exact line: $expected" >&2
        return 1
    }
}

assert_service_state() {
    binary=$1
    installed=$2
    enabled=$3
    running=$4
    output=$("$binary" service status)
    assert_output_has "$output" "Installed: $installed"
    assert_output_has "$output" "Enabled: $enabled"
    assert_output_has "$output" "Running: $running"
}

capture_status() {
    capture_file=$1
    shift
    if "$@" > "$capture_file" 2>&1; then
        captured_status=0
    else
        captured_status=$?
    fi
}

platform=$(uname -s)
machine=$(uname -m)
case "$platform:$machine" in
    Darwin:arm64) target=aarch64-apple-darwin ;;
    Linux:aarch64|Linux:arm64) target=aarch64-unknown-linux-musl ;;
    *) die "acceptance requires Apple Silicon macOS or Linux, found $platform/$machine" ;;
esac

installer=$artifact_dir/car-go-clean-installer.sh
upgrade=$artifact_dir/car-go-clean-upgrade.sh
archive_name=car-go-clean-$target.tar.xz
archive=$artifact_dir/$archive_name
shell_binary=$work_root/shell/bin/car-go-clean
config_dir=$work_root/xdg-config
state_home=$work_root/xdg-state
config=$config_dir/car-go-clean/config.toml
state_dir=$state_home/car-go-clean-acceptance
project_root=$work_root/projects
original_path=$PATH

export XDG_CONFIG_HOME="$config_dir"
export XDG_STATE_HOME="$state_home"

write_config() {
    root=$1
    mkdir -p "$(dirname "$config")"
    printf 'scan_dirs = ["%s"]\ntarget_quiet_period = "1ms"\n' "$root" > "$config"
}

step_shell_install() {
    required_file car-go-clean-installer.sh
    required_file "$archive_name"
    required_file "$archive_name.sha256"
    mkdir -p "$work_root/shell/bin"
    CAR_GO_CLEAN_ALLOW_INSECURE_TEST_URL=1 \
        sh "$installer" \
        --version "$version" \
        --install-dir "$work_root/shell/bin" \
        --download-base-url "file://$artifact_dir"
    fault_checkpoint middle
    test -x "$shell_binary"
    test "$("$shell_binary" version)" = "$version"
}

formula_file() {
    for candidate in car-go-clean.rb car-go-clean.release.rb; do
        if test -f "$artifact_dir/$candidate"; then
            printf '%s\n' "$artifact_dir/$candidate"
            return 0
        fi
    done
    return 1
}

step_formula_install() {
    formula=$(formula_file) || {
        echo "required copied local formula is missing" >&2
        return 1
    }
    required_file "$archive_name"
    formula_dir=$work_root/formula
    local_formula=$formula_dir/car-go-clean.rb
    mkdir -p "$formula_dir"
    expected_url=https://github.com/dcchuck/car-go-clean/releases/download/v$version/$archive_name
    python3 - "$formula" "$local_formula" "$archive" "$expected_url" "$version" <<'PY'
import pathlib
import re
import sys

source, destination, archive, expected_url, version = sys.argv[1:]
text = pathlib.Path(source).read_text()
text, count = re.subn(
    rf'url "{re.escape(expected_url)}"',
    f'url "file://{archive}"',
    text,
)
if count != 1:
    raise SystemExit("formula did not contain exactly one exact current-target URL")
version_lines = re.findall(r'(?m)^  version "([^"]+)"$', text)
if version_lines:
    if version_lines != [version]:
        raise SystemExit("formula contains a conflicting explicit version")
else:
    text, class_count = re.subn(
        r"(?m)^(class CarGoClean < Formula)$",
        rf'\1\n  version "{version}"',
        text,
        count=1,
    )
    if class_count != 1:
        raise SystemExit("formula did not contain exactly one CarGoClean class")
pathlib.Path(destination).write_text(text)
PY
    fault_checkpoint middle
    case "$platform" in
        Darwin)
            command -v brew >/dev/null 2>&1 || {
                echo "Homebrew is required for the macOS formula acceptance path" >&2
                return 1
            }
            HOMEBREW_NO_AUTO_UPDATE=1 HOMEBREW_NO_INSTALL_CLEANUP=1 \
                brew install --formula "$local_formula"
            formula_binary=$(brew --prefix car-go-clean)/bin/car-go-clean
            test "$("$formula_binary" version)" = "$version"
            HOMEBREW_NO_AUTO_UPDATE=1 brew test car-go-clean
            brew uninstall --force car-go-clean
            ;;
        Linux)
            echo "Linux guest: Homebrew formula execution is not applicable; hosted smoke evidence is aggregate-bound."
            grep -F -- "sha256" "$local_formula" >/dev/null
            ;;
    esac
}

step_version_health() {
    empty_root=$work_root/health-root
    mkdir -p "$empty_root"
    write_config "$empty_root"
    test "$("$shell_binary" version)" = "$version"
    fault_checkpoint middle
    health_file=$work_root/health.out
    capture_status "$health_file" "$shell_binary" health --skip-cargo \
        --config "$config" --state-dir "$state_dir"
    health_output=$(cat "$health_file")
    printf '%s\n' "$health_output"
    test "$captured_status" -eq 2
    assert_output_has "$health_output" "Cleanup authority"
    assert_output_has "$health_output" "Config source"
    assert_output_has "$health_output" "Generation state: missing"
    assert_output_has "$health_output" "Current generation: <none>"
    assert_output_has "$health_output" "Outcome: incomplete (code=2)"
    assert_output_line "$health_output" \
        "Reasons: generation_missing, scan_incomplete"
    assert_service_state "$shell_binary" no no no
}

step_disposable_build() {
    command -v cargo >/dev/null 2>&1 || {
        echo "Cargo is required to build the disposable Rust fixture" >&2
        return 1
    }
    mkdir -p "$project_root"
    cargo new "$project_root/sample"
    fault_checkpoint middle
    cargo build --manifest-path "$project_root/sample/Cargo.toml"
    sleep 1
    test -d "$project_root/sample/target"
}

step_dry_run() {
    write_config "$project_root"
    preview=$work_root/preview.out
    capture_status "$preview" "$shell_binary" run --dry-run --all \
        --config "$config" --state-dir "$state_dir"
    test "$captured_status" -eq 0
    fault_checkpoint middle
    review_ids=$(sed -n 's/^Review ID: \([0-9][0-9]*\)$/\1/p' "$preview")
    test "$(printf '%s\n' "$review_ids" | awk 'NF { count++ } END { print count + 0 }')" -eq 1
    review_id=$review_ids
    candidate_bytes=$(sed -n 's/^Candidate bytes: \([0-9][0-9]*\)$/\1/p' "$preview")
    test -n "$candidate_bytes" && test "$candidate_bytes" -gt 0
    grep -F -- "$project_root/sample" "$preview" >/dev/null
    test -d "$project_root/sample/target"
    printf '%s\n' "$review_id" > "$work_root/review-id"
}

step_review() {
    review_id=$(cat "$work_root/review-id")
    reviewed=$work_root/reviewed.ndjson
    capture_status "$reviewed" "$shell_binary" run --review "$review_id" --json \
        --config "$config" --state-dir "$state_dir"
    test "$captured_status" -eq 0
    fault_checkpoint middle
    test ! -d "$project_root/sample/target"
    python3 - "$reviewed" "$review_id" <<'PY'
import json
import pathlib
import sys

lines = [json.loads(line) for line in pathlib.Path(sys.argv[1]).read_text().splitlines()]
review_id = int(sys.argv[2])
if not any(line.get("event") == "target" for line in lines[:-1]):
    raise SystemExit("reviewed cleanup did not emit a target event")
terminal = lines[-1]
if terminal.get("format_version") != 1:
    raise SystemExit("terminal envelope format was not 1")
if terminal.get("command") != "run" or terminal.get("review_id") != review_id:
    raise SystemExit("terminal envelope did not bind the exact reviewed run")
if terminal.get("outcome", {}).get("code") != 0:
    raise SystemExit("terminal outcome did not match exit 0")
PY
    stats=$work_root/stats.json
    "$shell_binary" stats --json --state-dir "$state_dir" > "$stats"
    python3 - "$stats" <<'PY'
import json
import pathlib
import sys

report = json.loads(pathlib.Path(sys.argv[1]).read_text())
if report.get("format_version") != 1 or report.get("outcome", {}).get("code") != 0:
    raise SystemExit("stats envelope was not a complete format-v1 report")
if report.get("data", {}).get("total_bytes", 0) <= 0:
    raise SystemExit("reviewed cleanup recorded no recovered bytes")
PY
}

step_no_scan() {
    no_scan_root=$work_root/no-scan
    cargo new "$no_scan_root/project"
    cargo build --manifest-path "$no_scan_root/project/Cargo.toml"
    sleep 1
    write_config "$no_scan_root"
    seeded=$work_root/no-scan-seeded.out
    "$shell_binary" run --dry-run --all \
        --config "$config" --state-dir "$state_dir" > "$seeded"
    grep -F -- "Cleanable: $no_scan_root/project" "$seeded" >/dev/null
    seeded_bytes=$(sed -n 's/^Candidate bytes: \([0-9][0-9]*\)$/\1/p' "$seeded")
    test -n "$seeded_bytes" && test "$seeded_bytes" -gt 0
    fault_checkpoint middle

    # Do not touch the project or target between discovery and cache-only use:
    # the persisted identities must remain unchanged.
    output=$work_root/no-scan.out
    capture_status "$output" "$shell_binary" run --dry-run --no-scan --all \
        --config "$config" --state-dir "$state_dir"
    test "$captured_status" -eq 0
    grep -F -- "Cleanable: $no_scan_root/project" "$output" >/dev/null
    candidate_bytes=$(sed -n 's/^Candidate bytes: \([0-9][0-9]*\)$/\1/p' "$output")
    test -n "$candidate_bytes" && test "$candidate_bytes" -gt 0
    review=$(sed -n 's/^Review ID: \([0-9][0-9]*\)$/\1/p' "$output")
    test -n "$review"
    "$shell_binary" run --review "$review" \
        --config "$config" --state-dir "$state_dir"
    test ! -d "$no_scan_root/project/target"
}

step_narrowed_scope() {
    scope=$work_root/narrow/in-scope
    outside=$work_root/narrow/out-of-scope
    cargo new "$scope/project"
    cargo build --manifest-path "$scope/project/Cargo.toml"
    cargo new "$outside/sentinel-project"
    cargo build --manifest-path "$outside/sentinel-project/Cargo.toml"
    printf 'retain\n' > "$outside/sentinel-project/target/SENTINEL"
    sleep 1

    # Cache both genuine projects first. Narrowing the policy must revoke the
    # formerly cached outside project rather than treating cache history as
    # cleanup authority.
    write_config "$work_root/narrow"
    "$shell_binary" run --dry-run --all \
        --config "$config" --state-dir "$state_dir" \
        > "$work_root/narrow-broad-preview.out"
    grep -F -- "$scope/project" "$work_root/narrow-broad-preview.out" >/dev/null
    grep -F -- "$outside/sentinel-project" \
        "$work_root/narrow-broad-preview.out" >/dev/null

    write_config "$scope"
    fault_checkpoint middle
    output=$work_root/narrow-preview.out
    capture_status "$output" "$shell_binary" run --dry-run --no-scan --all \
        --config "$config" --state-dir "$state_dir"
    test "$captured_status" -eq 2
    grep -F -- \
        "No review ID was created because no valid matching discovery generation exists." \
        "$output" >/dev/null
    if grep -E '^Review ID: [0-9]+$' "$output" >/dev/null; then
        echo "narrowed cache-only policy unexpectedly created cleanup authority" >&2
        return 1
    fi
    printf 'skipped:out_of_scope cached target=%s (policy generation rejected)\n' \
        "$outside/sentinel-project"
    narrowed_scan=$work_root/narrow-authorized-preview.out
    "$shell_binary" run --dry-run --all \
        --config "$config" --state-dir "$state_dir" > "$narrowed_scan"
    review=$(sed -n 's/^Review ID: \([0-9][0-9]*\)$/\1/p' "$narrowed_scan")
    test -n "$review"
    grep -F -- "Cleanable: $scope/project" "$narrowed_scan" >/dev/null
    if grep -F -- "$outside/sentinel-project" "$narrowed_scan" >/dev/null; then
        echo "normal narrowed scan reauthorized the outside cached target" >&2
        return 1
    fi
    "$shell_binary" run --review "$review" \
        --config "$config" --state-dir "$state_dir"
    test ! -d "$scope/project/target"
    test -f "$outside/sentinel-project/target/SENTINEL"
}

step_cargo_failure() {
    failure_root=$work_root/cargo-failure
    failure_home=$failure_root/home
    fail_bin=$failure_home/.cargo/bin
    clean_marker=$failure_root/clean-shim-hit
    delegate_marker=$failure_root/non-clean-delegated
    real_cargo=$(command -v cargo)
    mkdir -p "$fail_bin"
    cat > "$fail_bin/cargo" <<EOF
#!/bin/sh
if test "\${1-}" = clean; then
    : > "$clean_marker"
    echo "intentional acceptance Cargo failure" >&2
    exit 42
fi
: > "$delegate_marker"
exec "$real_cargo" "\$@"
EOF
    chmod +x "$fail_bin/cargo"
    env HOME="$failure_home" PATH="$fail_bin:$original_path" \
        cargo new "$failure_root/project"
    env HOME="$failure_home" PATH="$fail_bin:$original_path" \
        cargo build --manifest-path "$failure_root/project/Cargo.toml"
    test -f "$delegate_marker"
    sleep 1
    write_config "$failure_root"
    preview=$work_root/cargo-failure-preview.out
    env HOME="$failure_home" PATH="$fail_bin:$original_path" \
        "$shell_binary" run --dry-run --all \
        --config "$config" --state-dir "$state_dir" > "$preview"
    review=$(sed -n 's/^Review ID: \([0-9][0-9]*\)$/\1/p' "$preview")
    test -n "$review"
    fault_checkpoint middle
    output=$work_root/cargo-failure-run.out
    capture_status "$output" env HOME="$failure_home" \
        PATH="$fail_bin:$original_path" \
        "$shell_binary" run --review "$review" \
        --config "$config" --state-dir "$state_dir"
    test "$captured_status" -eq 1
    test -f "$clean_marker"
    test -f "$delegate_marker"
    test -d "$failure_root/project/target"
    errors=$work_root/cargo-errors.json
    "$shell_binary" logs --errors-only --json --state-dir "$state_dir" > "$errors"
    grep -F -- "intentional acceptance Cargo failure" "$errors" >/dev/null
}

step_incomplete_scan() {
    incomplete_root=$work_root/incomplete
    mkdir -p "$incomplete_root/denied"
    chmod 000 "$incomplete_root/denied"
    write_config "$incomplete_root"
    output=$work_root/incomplete.out
    capture_status "$output" "$shell_binary" run --dry-run --all \
        --config "$config" --state-dir "$state_dir"
    chmod 700 "$incomplete_root/denied"
    fault_checkpoint middle
    cat "$output"
    test "$captured_status" -eq 2
    grep -E '^Review ID: [0-9]+$' "$output" >/dev/null
    grep -E 'Permission denied|Operation not permitted' "$output" >/dev/null
}

step_complete_scan() {
    complete_root=$work_root/complete
    mkdir -p "$complete_root"
    write_config "$complete_root"
    output=$work_root/complete.out
    capture_status "$output" "$shell_binary" scan \
        --config "$config" --state-dir "$state_dir"
    fault_checkpoint middle
    test "$captured_status" -eq 0
}

step_strict_config() {
    typo=$work_root/typo.toml
    undefined=$work_root/undefined.toml
    printf 'scan_dirz = ["%s"]\n' "$work_root" > "$typo"
    # shellcheck disable=SC2016 # The literal variable reference is the fixture.
    printf '%s\n' 'scan_dirs = ["$CAR_GO_CLEAN_ACCEPTANCE_UNDEFINED"]' > "$undefined"
    unset CAR_GO_CLEAN_ACCEPTANCE_UNDEFINED 2>/dev/null || :
    capture_status "$work_root/typo.out" "$shell_binary" config --config "$typo"
    test "$captured_status" -eq 1
    fault_checkpoint middle
    grep -E 'unknown field|unknown key' "$work_root/typo.out" >/dev/null
    capture_status "$work_root/undefined.out" "$shell_binary" config --config "$undefined"
    test "$captured_status" -eq 1
    grep -E 'not set|undefined' "$work_root/undefined.out" >/dev/null
}

step_migration_roundtrip() {
    legacy=$work_root/legacy.toml
    roundtrip=$work_root/roundtrip.toml
    printf 'scan_dirs = ["%s"]\nexcludes = ["node_modules"]\n' \
        "$work_root/complete" > "$legacy"
    capture_status "$work_root/legacy.out" "$shell_binary" config --config "$legacy"
    test "$captured_status" -eq 0
    fault_checkpoint middle
    grep -F -- 'deprecated' "$work_root/legacy.out" >/dev/null
    "$shell_binary" config migrate --config "$legacy" > "$work_root/migrate.out"
    grep -F -- 'override_excludes' "$legacy" >/dev/null
    if grep -E '^excludes[[:space:]]*=' "$legacy" >/dev/null; then
        return 1
    fi
    "$shell_binary" config --config "$legacy" > "$roundtrip"
    "$shell_binary" config --config "$roundtrip" > "$work_root/roundtrip-again.toml"
    cmp "$roundtrip" "$work_root/roundtrip-again.toml"
}

step_service_pre_reboot() {
    write_config "$work_root/complete"
    mkdir -p "$state_dir"
    printf 'retain-config\n' > "$config_dir/retention-marker"
    printf 'retain-state\n' > "$state_dir/retention-marker"
    assert_service_state "$shell_binary" no no no
    "$shell_binary" service install
    fault_checkpoint middle
    assert_service_state "$shell_binary" yes yes yes
    "$shell_binary" service stop
    assert_service_state "$shell_binary" yes no no
    : > "$session_marker"
}

step_service_post_reboot() {
    test -f "$session_marker"
    assert_service_state "$shell_binary" yes no no
    "$shell_binary" service start
    fault_checkpoint middle
    assert_service_state "$shell_binary" yes yes yes
    "$shell_binary" service uninstall
    assert_service_state "$shell_binary" no no no
    test "$(cat "$config_dir/retention-marker")" = retain-config
    test "$(cat "$state_dir/retention-marker")" = retain-state
}

native_stop_old_service() {
    case "$platform" in
        Darwin)
            launchctl bootout "gui/$(id -u)/com.dcchuck.car-go-clean"
            ;;
        Linux)
            systemctl --user stop car-go-clean.service
            ;;
    esac
}

native_enable_old_service_fixture() {
    if test "$platform" = Darwin; then
        label=gui/$(id -u)/com.dcchuck.car-go-clean
        echo "Fixture setup: launchctl enable $label"
        launchctl enable "$label"
    fi
}

step_upgrade_matrix() {
    required_file car-go-clean-upgrade.sh
    required_file car-go-clean-installer.sh
    required_file car-go-clean-shell-assets.sha256
    required_file "$archive_name"
    required_file "$archive_name.sha256"
    for old_version in 0.2.0 0.3.0; do
        old_fixture=car-go-clean-v$old_version-$target
        required_file "$old_fixture"
        for old_state in active stopped absent; do
            case_dir=$work_root/upgrade-$old_version-$old_state
            rm -rf "$case_dir"
            mkdir -p "$case_dir/bin" "$case_dir/curl-bin" "$case_dir/project"
            cp "$artifact_dir/$old_fixture" "$case_dir/bin/car-go-clean"
            chmod +x "$case_dir/bin/car-go-clean"
            export XDG_CONFIG_HOME="$case_dir/config"
            export XDG_STATE_HOME="$case_dir/state"
            export CAR_GO_CLEAN_UPGRADE_STATE_DIR="$case_dir/upgrade-state"
            mkdir -p "$XDG_CONFIG_HOME/car-go-clean"
            case_config=$XDG_CONFIG_HOME/car-go-clean/config.toml
            empty_service_root=$case_dir/empty-service-root
            mkdir -p "$empty_service_root"
            printf 'scan_dirs = ["%s"]\ntarget_quiet_period = "1ms"\n' \
                "$empty_service_root" > "$case_config"
            cargo new "$case_dir/project/sample"
            cargo build --manifest-path "$case_dir/project/sample/Cargo.toml"

            "$shell_binary" service uninstall >/dev/null 2>&1 || :
            case "$old_state" in
                active)
                    native_enable_old_service_fixture
                    "$case_dir/bin/car-go-clean" service install
                    ;;
                stopped)
                    native_enable_old_service_fixture
                    "$case_dir/bin/car-go-clean" service install
                    native_stop_old_service
                    ;;
                absent)
                    ;;
            esac
            # An installed old daemon loaded only the empty root. Point the
            # operator's config at the disposable candidate immediately before
            # upgrade, so no old daemon can clean it before phase-one preview.
            printf 'scan_dirs = ["%s"]\ntarget_quiet_period = "1ms"\n' \
                "$case_dir/project" > "$case_config"
            sleep 1

            cat > "$case_dir/curl-bin/curl" <<EOF
#!/bin/sh
set -eu
output=
url=
while test "\$#" -gt 0; do
    case "\$1" in
        -o) output=\$2; shift 2 ;;
        *) url=\$1; shift ;;
    esac
done
test -n "\$output"
case "\$url" in
    https://github.com/dcchuck/car-go-clean/releases/download/v0.4.0/*)
        name=\${url##*/}
        test -f "$artifact_dir/\$name"
        cp "$artifact_dir/\$name" "\$output"
        ;;
    *) echo "acceptance blocked external download: \$url" >&2; exit 97 ;;
esac
EOF
            chmod +x "$case_dir/curl-bin/curl"
            upgrade_path=$original_path
            PATH="$case_dir/curl-bin:$case_dir/bin:$upgrade_path" \
                "$upgrade" --version "$version" --method shell \
                > "$case_dir/preview.out" 2>&1
            review=$(sed -n 's/^Review ID: \([0-9][0-9]*\)$/\1/p' "$case_dir/preview.out")
            test -n "$review"
            fault_checkpoint middle
            PATH="$case_dir/curl-bin:$case_dir/bin:$upgrade_path" \
                "$upgrade" --version "$version" --method shell \
                --execute-review "$review" > "$case_dir/execute.out" 2>&1
            test "$("$case_dir/bin/car-go-clean" version)" = "$version"
            test ! -d "$case_dir/project/sample/target"
            case "$old_state" in
                active) assert_service_state "$case_dir/bin/car-go-clean" yes yes yes ;;
                stopped) assert_service_state "$case_dir/bin/car-go-clean" yes yes no ;;
                absent) assert_service_state "$case_dir/bin/car-go-clean" no no no ;;
            esac
            "$case_dir/bin/car-go-clean" service uninstall >/dev/null 2>&1 || :
            unset CAR_GO_CLEAN_UPGRADE_STATE_DIR
        done
    done
    export XDG_CONFIG_HOME="$config_dir"
    export XDG_STATE_HOME="$state_home"
}

step_macos_library_privacy() {
    if test "$platform" != Darwin; then
        fault_checkpoint middle
        echo "Linux guest: macOS Library/TCC assertion is not applicable."
        return 0
    fi
    library_project="$HOME/Library/Application Support/car-go-clean-acceptance"
    denied="$HOME/car-go-clean-acceptance-privacy-denied"
    mkdir -p "$library_project/target" "$denied"
    printf '[package]\nname="library-sentinel"\nversion="0.1.0"\n' \
        > "$library_project/Cargo.toml"
    printf 'retain\n' > "$library_project/target/SENTINEL"
    chmod 000 "$denied"
    privacy_config_home=$work_root/privacy-config
    privacy_state=$work_root/privacy-state
    output=$work_root/privacy.out
    capture_status "$output" env XDG_CONFIG_HOME="$privacy_config_home" \
        XDG_STATE_HOME="$privacy_state" "$shell_binary" run --dry-run --all \
        --state-dir "$privacy_state"
    chmod 700 "$denied"
    fault_checkpoint middle
    test "$captured_status" -eq 2
    grep -E 'Permission denied|Operation not permitted' "$output" >/dev/null
    test -f "$library_project/target/SENTINEL"
    if grep -F -- "$library_project" "$output" >/dev/null; then
        echo "macOS Library project was visible as a cleanup candidate" >&2
        return 1
    fi
}

verify_artifact_set

if test "$phase" = pre-reboot; then
    rm -rf "$work_root"
    mkdir -p "$work_root"
    : > "$milestones"
    {
        printf 'format_version=1\n'
        printf 'exact_sha=%s\n' "$exact_sha"
        printf 'version=%s\n' "$version"
        printf 'platform=%s\n' "$platform"
        printf 'target=%s\n' "$target"
    } > "$evidence_dir/guest-metadata.txt"
    record_step shell-install step_shell_install
    record_step formula-install step_formula_install
    record_step version-health step_version_health
    record_step disposable-build step_disposable_build
    record_step dry-run step_dry_run
    record_step review step_review
    record_step no-scan step_no_scan
    record_step narrowed-scope step_narrowed_scope
    record_step cargo-failure step_cargo_failure
    record_step incomplete-scan step_incomplete_scan
    record_step complete-scan step_complete_scan
    record_step strict-config step_strict_config
    record_step migration-roundtrip step_migration_roundtrip
    record_step service-pre-reboot step_service_pre_reboot
else
    test -f "$milestones" || die "pre-reboot evidence is missing"
    record_step service-post-reboot step_service_post_reboot
    record_step upgrade-matrix step_upgrade_matrix
    record_step macos-library-privacy step_macos_library_privacy
fi

sanitize_transcript
printf 'Guest acceptance phase %s passed.\n' "$phase"
