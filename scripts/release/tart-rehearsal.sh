#!/bin/sh
set -eu

usage() {
    echo "usage: $0 /absolute/path/to/rehearsal-artifacts /absolute/path/to/aggregate-evidence /absolute/path/to/evidence" >&2
}

die() {
    echo "tart rehearsal: $*" >&2
    exit 1
}

immutable_digest() {
    reference=$1
    case "$reference" in
        ghcr.io/*@sha256:*) ;;
        *) return 1 ;;
    esac
    prefix=${reference%@sha256:*}
    digest=${reference##*@sha256:}
    test -n "$prefix" || return 1
    case "$prefix" in
        *@*) return 1 ;;
    esac
    case "$digest" in
        *[!0-9a-f]*) return 1 ;;
    esac
    test "${#digest}" -eq 64 || return 1
    printf '%s\n' "$digest"
}

test "$#" -eq 3 || {
    usage
    exit 2
}
artifact_dir=$1
aggregate_dir=$2
evidence_dir=$3
case "$artifact_dir:$aggregate_dir:$evidence_dir" in
    /*:/*:/*) ;;
    *) die "artifact, aggregate evidence, and output evidence paths must be absolute" ;;
esac
test -d "$artifact_dir" || die "artifact directory does not exist: $artifact_dir"
test -d "$aggregate_dir" || die "aggregate evidence directory does not exist: $aggregate_dir"
test -f "$artifact_dir/SHA256SUMS" ||
    die "artifact directory must contain the exact rehearsal SHA256SUMS"
test ! -e "$evidence_dir" ||
    die "output evidence directory must be new and absent: $evidence_dir"

macos_image=${CAR_GO_CLEAN_TART_MACOS_IMAGE-}
linux_image=${CAR_GO_CLEAN_TART_LINUX_IMAGE-}
macos_digest=$(immutable_digest "$macos_image") ||
    die "CAR_GO_CLEAN_TART_MACOS_IMAGE must be an immutable ghcr.io/...@sha256:<64 lowercase hex> reference"
linux_digest=$(immutable_digest "$linux_image") ||
    die "CAR_GO_CLEAN_TART_LINUX_IMAGE must be an immutable ghcr.io/...@sha256:<64 lowercase hex> reference"

version=${CAR_GO_CLEAN_ACCEPTANCE_VERSION-}
case "$version" in
    ''|*[!0-9.]*) die "CAR_GO_CLEAN_ACCEPTANCE_VERSION must be X.Y.Z" ;;
esac
version_fields=$(printf '%s\n' "$version" | awk -F . 'NF == 3 &&
    $1 ~ /^[0-9]+$/ && $2 ~ /^[0-9]+$/ && $3 ~ /^[0-9]+$/ { print "valid" }')
test "$version_fields" = valid ||
    die "CAR_GO_CLEAN_ACCEPTANCE_VERSION must be X.Y.Z"

exact_sha=${CAR_GO_CLEAN_ACCEPTANCE_SHA-}
case "$exact_sha" in
    *[!0-9a-f]*|'') die "CAR_GO_CLEAN_ACCEPTANCE_SHA must be a lowercase commit SHA" ;;
esac
test "${#exact_sha}" -eq 40 ||
    die "CAR_GO_CLEAN_ACCEPTANCE_SHA must be an exact 40-character Git commit"

command -v tart >/dev/null 2>&1 || die "tart is not available"
command -v ssh >/dev/null 2>&1 || die "ssh is not available"
command -v python3 >/dev/null 2>&1 || die "python3 is required"
command -v tar >/dev/null 2>&1 || die "tar is required"

script_dir=$(CDPATH='' cd -- "$(dirname -- "$0")" && pwd)
repo_root=$(CDPATH='' cd -- "$script_dir/../.." && pwd)
acceptance_script=$script_dir/acceptance.sh
test -f "$acceptance_script" || die "guest acceptance script is missing"

(CDPATH='' cd "$repo_root" &&
    "$repo_root/scripts/validate-release-inputs.sh" "$exact_sha" "$version") ||
    die "local checkout did not validate as the exact clean release commit"

mkdir "$evidence_dir"
chmod 700 "$evidence_dir"
bindings=$evidence_dir/verified-input-bindings.tsv

python3 - "$artifact_dir" "$aggregate_dir" "$repo_root" \
    "$exact_sha" "$version" "$bindings" <<'PY'
import hashlib
import json
import os
import pathlib
import re
import stat
import sys

artifacts = pathlib.Path(sys.argv[1])
aggregate = pathlib.Path(sys.argv[2])
repo = pathlib.Path(sys.argv[3])
exact_sha, version = sys.argv[4:6]
bindings = pathlib.Path(sys.argv[6])
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
for entry in os.scandir(artifacts):
    mode = entry.stat(follow_symlinks=False).st_mode
    if stat.S_ISLNK(mode) or not stat.S_ISREG(mode):
        raise SystemExit(f"artifact is not a regular non-symlink file: {entry.name}")
    actual.add(entry.name)
if actual != expected | {"SHA256SUMS"}:
    raise SystemExit(
        "artifact set does not equal closed allowlist; "
        f"missing={sorted(expected - actual)}, "
        f"extra={sorted(actual - expected - {'SHA256SUMS'})}"
    )

manifest = artifacts / "SHA256SUMS"
seen = {}
for line_number, line in enumerate(manifest.read_text().splitlines(), 1):
    match = re.fullmatch(r"([0-9a-f]{64}) [ *]([^\r\n/]+)", line)
    if not match or match.group(2) in seen:
        raise SystemExit(f"malformed, nested, or duplicate SHA256SUMS line {line_number}")
    seen[match.group(2)] = match.group(1)
if set(seen) != expected:
    raise SystemExit("SHA256SUMS names do not equal the closed artifact allowlist")
for name, expected_hash in seen.items():
    actual_hash = hashlib.sha256((artifacts / name).read_bytes()).hexdigest()
    if actual_hash != expected_hash:
        raise SystemExit(f"SHA256 mismatch for {name}")

for target in ("aarch64-apple-darwin", "aarch64-unknown-linux-gnu"):
    binary = f"rustup-init-{target}"
    proof = artifacts / f"{binary}.sha256"
    # rustup currently emits `*./rustup-init`; retain the historical
    # no-prefix spelling while rejecting any other path.
    match = re.fullmatch(
        r"([0-9a-f]{64}) [ *](?:\./)?rustup-init\n?",
        proof.read_text(),
    )
    if not match or match.group(1) != seen[binary]:
        raise SystemExit(f"official rustup checksum proof does not bind {binary}")

source_files = {
    "acceptance.sh": repo / "scripts/release/acceptance.sh",
    "car-go-clean-installer.sh": repo / "packaging/release/car-go-clean-installer.sh",
    "car-go-clean-upgrade.sh": repo / "packaging/release/car-go-clean-upgrade.sh",
}
for name, source in source_files.items():
    if (artifacts / name).read_bytes() != source.read_bytes():
        raise SystemExit(f"{name} is not byte-identical to the exact checkout")

shell_manifest = {}
for line in (artifacts / "car-go-clean-shell-assets.sha256").read_text().splitlines():
    match = re.fullmatch(
        r"([0-9a-f]{64}) [ *](car-go-clean-(?:installer|upgrade)\.sh)", line
    )
    if not match or match.group(2) in shell_manifest:
        raise SystemExit("shell asset checksum proof is malformed")
    shell_manifest[match.group(2)] = match.group(1)
if set(shell_manifest) != {
    "car-go-clean-installer.sh", "car-go-clean-upgrade.sh"
}:
    raise SystemExit("shell asset checksum proof is incomplete")
for name, digest in shell_manifest.items():
    if seen[name] != digest:
        raise SystemExit(f"shell asset proof disagrees with closed manifest for {name}")

def load_required(relative):
    path = aggregate / relative
    mode = path.lstat().st_mode
    if not stat.S_ISREG(mode):
        raise SystemExit(f"aggregate evidence is not a regular file: {relative}")
    return json.loads(path.read_text())

status_path = aggregate / "aggregate-status.txt"
if not stat.S_ISREG(status_path.lstat().st_mode) or status_path.read_text() != "ready\n":
    raise SystemExit("aggregate-status.txt is not exactly ready")
inventory = load_required("aggregate-inventory.json")
if (
    inventory.get("format_version") != 1
    or inventory.get("exact_sha") != exact_sha
    or inventory.get("version") != version
    or inventory.get("complete") is not True
    or inventory.get("sanitized") is not True
):
    raise SystemExit("aggregate inventory does not bind exact SHA/version/ready state")

validate = load_required("jobs/validate.json")
validate_keys = {
    "evidence_key",
    "checkout",
    "fetch_refs",
    "validation",
    "rust_toolchain",
    "install_cargo_dist",
    "dist_plan",
}
if (
    validate.get("format_version") != 1
    or validate.get("phase") != "validate"
    or validate.get("exact_sha") != exact_sha
    or validate.get("version") != version
    or set(validate.get("outcomes", {})) != validate_keys
    or any(value != "success" for value in validate["outcomes"].values())
):
    raise SystemExit("validate evidence is not exact and completely successful")

targets = (
    "aarch64-apple-darwin",
    "x86_64-apple-darwin",
    "aarch64-unknown-linux-musl",
    "x86_64-unknown-linux-musl",
)
archive_hashes = {}
for target in targets:
    for phase in ("build", "smoke"):
        record = load_required(f"jobs/{phase}-{target}.json")
        if (
            record.get("format_version") != 1
            or record.get("phase") != phase
            or record.get("exact_sha") != exact_sha
            or record.get("version") != version
            or record.get("target") != target
        ):
            raise SystemExit(f"{phase} evidence is not exact for {target}")
        digest = record.get("archive_sha256")
        if not isinstance(digest, str) or not re.fullmatch(r"[0-9a-f]{64}", digest):
            raise SystemExit(f"{phase} archive hash is invalid for {target}")
        if target in archive_hashes and archive_hashes[target] != digest:
            raise SystemExit(f"build/smoke archive hashes differ for {target}")
        archive_hashes[target] = digest
        outcomes = record.get("outcomes", {})
        if phase == "build":
            expected_keys = {
                "checkout",
                "fetch_refs",
                "revalidation",
                "rust_toolchain",
                "linux_dependencies",
                "install_cargo_dist",
                "build",
                "attestation",
                "archive_upload",
            }
            expected_outcomes = {name: "success" for name in expected_keys}
            if "apple-darwin" in target:
                expected_outcomes["linux_dependencies"] = "skipped"
        else:
            expected_keys = {
                "checkout",
                "artifact_download",
                "installer_and_formula",
            }
            expected_outcomes = {name: "success" for name in expected_keys}
        if outcomes != expected_outcomes:
            raise SystemExit(
                f"{phase} outcomes are not the exact successful set for {target}"
            )

for target in ("aarch64-apple-darwin", "aarch64-unknown-linux-musl"):
    archive = f"car-go-clean-{target}.tar.xz"
    if archive_hashes[target] != seen[archive]:
        raise SystemExit(f"aggregate archive hash does not bind local {archive}")

runner = load_required("jobs/runner-resolution.json")
if (
    runner.get("exact_sha") != exact_sha
    or runner.get("version") != version
    or runner.get("resolution") != "verified"
):
    raise SystemExit("runner-resolution evidence is not verified")
tap = load_required("jobs/tap-capability.json")
if (
    tap.get("exact_sha") != exact_sha
    or tap.get("version") != version
    or any(tap.get("outcomes", {}).get(name) != "success"
           for name in ("checkout", "capability", "cleanup"))
):
    raise SystemExit("tap-capability evidence is not successful")

template = (repo / "packaging/release/homebrew/car-go-clean.rb.in").read_text()
rendered = (
    template.replace("__TAG__", f"v{version}")
    .replace("__AARCH64_APPLE_SHA256__", archive_hashes["aarch64-apple-darwin"])
    .replace("__X86_64_APPLE_SHA256__", archive_hashes["x86_64-apple-darwin"])
    .replace("__AARCH64_LINUX_SHA256__", archive_hashes["aarch64-unknown-linux-musl"])
    .replace("__X86_64_LINUX_SHA256__", archive_hashes["x86_64-unknown-linux-musl"])
)
if (artifacts / "car-go-clean.rb").read_text() != rendered:
    raise SystemExit("car-go-clean.rb is not the exact-checkout aggregate-bound render")

rows = [
    ("exact_sha", exact_sha),
    ("version", version),
    ("aggregate_status", "ready"),
]
rows.extend((f"artifact:{name}", seen[name]) for name in sorted(seen))
rows.extend(
    (
        f"rustup_source:{target}",
        f"https://static.rust-lang.org/rustup/dist/{target}/rustup-init",
    )
    for target in ("aarch64-apple-darwin", "aarch64-unknown-linux-gnu")
)
bindings.write_text("".join(f"{key}\t{value}\n" for key, value in rows))
PY

source_map=$evidence_dir/source-map.tsv
: > "$source_map"
chmod 600 "$source_map"

ssh_password=${CAR_GO_CLEAN_TART_SSH_PASSWORD:-admin}
ssh_user=${CAR_GO_CLEAN_TART_SSH_USER:-admin}
ssh_options="-F /dev/null -T -o StrictHostKeyChecking=no -o UserKnownHostsFile=/dev/null -o LogLevel=ERROR -o ConnectTimeout=5 -o ConnectionAttempts=1 -o ControlMaster=no -o ControlPath=none -o ControlPersist=no -o RequestTTY=no -o BatchMode=no -o PreferredAuthentications=password -o PasswordAuthentication=yes -o KbdInteractiveAuthentication=no -o PubkeyAuthentication=no -o ForwardX11=no -o ForwardAgent=no -o ClearAllForwardings=yes -o NumberOfPasswordPrompts=1"
askpass_script=$evidence_dir/.ssh-askpass
transfer_archive=$evidence_dir/.artifacts-transfer.tar
active_phase_archive=

active_command_pid=
active_command_launching=false
active_command_previous_pid=
active_vm_name=
active_vm_evidence=
active_vm_run_pid=
active_vm_run_launching=false
active_vm_run_previous_pid=
cleanup_in_progress=false
signal_handling=false
pending_signal_status=0
lifecycle_failure=0
session_exec_code='import os, sys; os.setsid(); os.execvp(sys.argv[1], sys.argv[1:])'

# Keep $! initialized under `set -u`, then distinguish a pre-launch signal
# from one delivered after a new asynchronous process actually exists.
: &
last_launched_pid=$!
wait "$last_launched_pid"

run_interruptible() {
    active_command_pid=
    active_command_previous_pid=$last_launched_pid
    active_command_launching=true
    python3 -c "$session_exec_code" "$@" &
    active_command_pid=$!
    last_launched_pid=$active_command_pid
    active_command_launching=false
    if wait "$active_command_pid"; then
        interruptible_status=0
    else
        interruptible_status=$?
    fi
    active_command_pid=
    return "$interruptible_status"
}

run_interruptible_input() {
    command_input=$1
    shift
    active_command_pid=
    active_command_previous_pid=$last_launched_pid
    active_command_launching=true
    python3 -c "$session_exec_code" "$@" < "$command_input" &
    active_command_pid=$!
    last_launched_pid=$active_command_pid
    active_command_launching=false
    if wait "$active_command_pid"; then
        interruptible_status=0
    else
        interruptible_status=$?
    fi
    active_command_pid=
    return "$interruptible_status"
}

ssh_guest() {
    guest_host=$1
    shift
    # shellcheck disable=SC2086 # The fixed option string must become separate ssh arguments.
    if CAR_GO_CLEAN_TART_ASKPASS_PASSWORD=$ssh_password \
        SSH_ASKPASS=$askpass_script SSH_ASKPASS_REQUIRE=force \
        DISPLAY=car-go-clean-tart:0 \
        run_interruptible ssh $ssh_options "$ssh_user@$guest_host" "$@"; then
        ssh_status=0
    else
        ssh_status=$?
    fi
    unset CAR_GO_CLEAN_TART_ASKPASS_PASSWORD SSH_ASKPASS \
        SSH_ASKPASS_REQUIRE DISPLAY
    return "$ssh_status"
}

ssh_guest_input() {
    guest_host=$1
    command_input=$2
    shift 2
    # shellcheck disable=SC2086 # The fixed option string must become separate ssh arguments.
    if CAR_GO_CLEAN_TART_ASKPASS_PASSWORD=$ssh_password \
        SSH_ASKPASS=$askpass_script SSH_ASKPASS_REQUIRE=force \
        DISPLAY=car-go-clean-tart:0 \
        run_interruptible_input "$command_input" \
            ssh $ssh_options "$ssh_user@$guest_host" "$@"; then
        ssh_status=0
    else
        ssh_status=$?
    fi
    unset CAR_GO_CLEAN_TART_ASKPASS_PASSWORD SSH_ASKPASS \
        SSH_ASKPASS_REQUIRE DISPLAY
    return "$ssh_status"
}

terminate_tracked_group() {
    tree_pid=$1
    tree_signal=$2
    if kill -0 "-$tree_pid" 2>/dev/null; then
        kill -"$tree_signal" "-$tree_pid" 2>/dev/null || :
    elif kill -0 "$tree_pid" 2>/dev/null; then
        # The launcher may race from pre-setsid into its new group after this
        # probe, so direct-PID termination is followed by unconditional PGID
        # termination and verification below.
        kill -"$tree_signal" "$tree_pid" 2>/dev/null || :
    fi
    kill -KILL "-$tree_pid" 2>/dev/null || :
    kill -KILL "$tree_pid" 2>/dev/null || :
    wait "$tree_pid" 2>/dev/null || :

    tree_attempts=0
    while kill -0 "-$tree_pid" 2>/dev/null &&
        test "$tree_attempts" -lt 5; do
        kill -KILL "-$tree_pid" 2>/dev/null || :
        sleep 1
        tree_attempts=$((tree_attempts + 1))
    done
    ! kill -0 "-$tree_pid" 2>/dev/null
}

terminate_active_command() {
    command_signal=$1
    command_pid=$active_command_pid
    if test -z "$command_pid" &&
        test "$active_command_launching" = true; then
        launch_candidate=$!
        if test "$launch_candidate" != "$active_command_previous_pid"; then
            command_pid=$launch_candidate
        fi
    fi
    test -n "$command_pid" || return 0
    if ! terminate_tracked_group "$command_pid" "$command_signal"; then
        echo "could not terminate active command process group $command_pid" >&2
        lifecycle_failure=1
    fi
    active_command_pid=
    active_command_launching=false
}

reap_active_vm_runner() {
    runner_pid=$active_vm_run_pid
    if test -z "$runner_pid" &&
        test "$active_vm_run_launching" = true; then
        runner_candidate=$!
        if test "$runner_candidate" != "$active_vm_run_previous_pid"; then
            runner_pid=$runner_candidate
        fi
    fi
    test -n "$runner_pid" || return 0
    if ! terminate_tracked_group "$runner_pid" TERM; then
        echo "could not terminate Tart runner process group $runner_pid" >&2
        lifecycle_failure=1
    fi
    active_vm_run_pid=
    active_vm_run_launching=false
}

cleanup_active_vm() {
    test -n "$active_vm_name" || return 0
    cleanup_in_progress=true
    cleanup_name=$active_vm_name
    cleanup_evidence=$active_vm_evidence
    cleanup_status=0

    if ! tart stop "$cleanup_name" \
        > "$cleanup_evidence/vm-stop.log" 2>&1; then
        echo "could not stop preserved VM $cleanup_name" >&2
        cleanup_status=1
    fi
    reap_active_vm_runner
    if ! tart get "$cleanup_name" --format json \
        > "$cleanup_evidence/vm-state-after-stop.json" 2>&1 ||
        ! python3 -c '
import json
import sys

state = json.load(sys.stdin)
if state.get("Running") is not False or state.get("State") != "stopped":
    raise SystemExit(1)
' < "$cleanup_evidence/vm-state-after-stop.json"; then
        echo "preserved VM $cleanup_name is not verifiably stopped" >&2
        cleanup_status=1
    fi

    if test "$cleanup_status" -ne 0; then
        lifecycle_failure=1
        if ! printf 'lifecycle cleanup failed for %s\n' "$cleanup_name" \
            > "$cleanup_evidence/lifecycle-failure.txt"; then
            echo "could not persist lifecycle failure marker for $cleanup_name" >&2
        fi
    fi

    active_vm_name=
    active_vm_evidence=
    active_vm_run_pid=
    active_vm_run_launching=false
    cleanup_in_progress=false
}

handle_signal() {
    signal=$1
    signal_status=$2
    if test "$pending_signal_status" -eq 0; then
        pending_signal_status=$signal_status
    fi
    if test "$signal_handling" = true ||
        test "$cleanup_in_progress" = true; then
        return 0
    fi

    signal_handling=true
    terminate_active_command "$signal"
    cleanup_active_vm
    signal_handling=false
    if test "$lifecycle_failure" -ne 0; then
        exit 97
    fi
    exit "$pending_signal_status"
}

finish_on_exit() {
    original_status=$?
    trap - 0
    terminate_active_command TERM
    cleanup_active_vm
    if ! rm -f "$askpass_script" "$transfer_archive" \
        ${active_phase_archive:+"$active_phase_archive"}; then
        echo "could not remove temporary SSH transport files" >&2
        test "$original_status" -ne 0 || original_status=1
    fi
    if test "$lifecycle_failure" -ne 0; then
        exit 97
    fi
    if test "$pending_signal_status" -ne 0; then
        exit "$pending_signal_status"
    fi
    exit "$original_status"
}

trap finish_on_exit 0
trap 'handle_signal HUP 129' HUP
trap 'handle_signal INT 130' INT
trap 'handle_signal TERM 143' TERM

{
    printf '%s\n' '#!/bin/sh'
    # shellcheck disable=SC2016 # The generated helper expands this at runtime.
    printf '%s\n' 'printf "%s\n" "$CAR_GO_CLEAN_TART_ASKPASS_PASSWORD"'
} > "$askpass_script"
chmod 700 "$askpass_script"

read_single_line() {
    line_file=$1
    output_line=
    exec 3< "$line_file" || return 1
    if IFS= read -r output_line <&3; then
        :
    elif test -n "$output_line"; then
        :
    else
        exec 3<&-
        return 1
    fi
    extra_line=
    if IFS= read -r extra_line <&3 || test -n "$extra_line"; then
        exec 3<&-
        return 1
    fi
    exec 3<&-
}

vm_fault_checkpoint() {
    platform=$1
    position=$2
    if test "${CAR_GO_CLEAN_TART_FAULT-}" = "$platform:$position"; then
        echo "injected Tart rehearsal failure: $platform:$position" >&2
        return 96
    fi
}

wait_for_ssh() {
    vm_name=$1
    ip_output=$active_vm_evidence/tart-ip.txt
    resolved_guest_ip=
    attempts=0
    while test "$attempts" -lt 60; do
        : > "$ip_output"
        if run_interruptible tart ip "$vm_name" --wait 5 \
            > "$ip_output" 2>/dev/null &&
            read_single_line "$ip_output"; then
            guest_ip_candidate=$output_line
        else
            guest_ip_candidate=
        fi
        if test -n "$guest_ip_candidate" &&
            ssh_guest "$guest_ip_candidate" "printf ready" \
                >/dev/null 2>&1; then
            resolved_guest_ip=$guest_ip_candidate
            return 0
        fi
        attempts=$((attempts + 1))
        run_interruptible sleep 2 || return $?
    done
    return 1
}

wait_for_reboot() {
    vm_name=$1
    old_ip=$2
    saw_disconnect=false
    attempts=0
    while test "$attempts" -lt 30; do
        if ! ssh_guest "$old_ip" "printf ready" >/dev/null 2>&1; then
            saw_disconnect=true
            break
        fi
        attempts=$((attempts + 1))
        sleep 2
    done
    test "$saw_disconnect" = true || return 1
    wait_for_ssh "$vm_name"
}

guest_boot_identity() {
    platform=$1
    guest_ip=$2
    identity_output=$active_vm_evidence/boot-identity.txt
    case "$platform" in
        macos)
            ssh_guest "$guest_ip" "/usr/sbin/sysctl -n kern.boottime" \
                > "$identity_output" || return 1
            ;;
        linux)
            ssh_guest "$guest_ip" "cat /proc/sys/kernel/random/boot_id" \
                > "$identity_output" || return 1
            ;;
        *) return 1 ;;
    esac
    read_single_line "$identity_output" || return 1
    resolved_boot_identity=$output_line
    case "$resolved_boot_identity" in
        ''|*'	'*|*'
'*) return 1 ;;
    esac
}

copy_phase_evidence() {
    guest_ip=$1
    remote_root=$2
    destination=$3
    phase_name=${destination##*/}
    phase_archive=$destination/guest-evidence.tar
    active_phase_archive=$phase_archive
    mkdir "$destination"
    chmod 700 "$destination"
    if ! ssh_guest "$guest_ip" \
        "phase='$phase_name'; cd '$remote_root/evidence' && tar -cf - ." \
        > "$phase_archive"; then
        rm -f "$phase_archive"
        active_phase_archive=
        return 1
    fi
    if ! run_interruptible tar -C "$destination" -xf "$phase_archive"; then
        rm -f "$phase_archive"
        active_phase_archive=
        return 1
    fi
    rm -f "$phase_archive"
    active_phase_archive=
}

manifest_hash() {
    if command -v sha256sum >/dev/null 2>&1; then
        sha256sum "$artifact_dir/SHA256SUMS" | awk '{ print $1 }'
    else
        shasum -a 256 "$artifact_dir/SHA256SUMS" | awk '{ print $1 }'
    fi
}

finish_vm_run() {
    completed_status=$1
    cleanup_active_vm
    if test "$lifecycle_failure" -ne 0; then
        run_vm_result=97
    elif test "$pending_signal_status" -ne 0; then
        exit "$pending_signal_status"
    else
        run_vm_result=$completed_status
    fi
}

run_vm() {
    platform=$1
    image=$2
    digest=$3
    run_vm_result=1
    guest_ip=
    new_guest_ip=
    pre_boot_identity=
    post_boot_identity=
    resolved_guest_ip=
    resolved_boot_identity=
    case "$platform" in
        macos)
            rustup_target=aarch64-apple-darwin
            ;;
        linux)
            rustup_target=aarch64-unknown-linux-gnu
            ;;
        *) return 1 ;;
    esac
    rustup_artifact=rustup-init-$rustup_target
    rustup_digest=$(awk -v name="$rustup_artifact" \
        '$2 == name || $2 == "*" name { print $1; found++ }
         END { if (found != 1) exit 1 }' "$artifact_dir/SHA256SUMS")
    rustup_source=https://static.rust-lang.org/rustup/dist/$rustup_target/rustup-init
    stamp=$(date -u +%Y%m%d%H%M%S)
    vm_name=car-go-clean-v040-$platform-$stamp-$$
    platform_evidence=$evidence_dir/$platform
    mkdir -p "$platform_evidence"
    chmod 700 "$platform_evidence"

    {
        printf 'format_version=1\n'
        printf 'platform=%s\n' "$platform"
        printf 'exact_sha=%s\n' "$exact_sha"
        printf 'version=%s\n' "$version"
        printf 'image_reference=%s\n' "$image"
        printf 'image_digest=%s\n' "$digest"
        printf 'artifact_manifest_sha256=%s\n' "$(manifest_hash)"
        printf 'rustup_source=%s\n' "$rustup_source"
        printf 'rustup_sha256=%s\n' "$rustup_digest"
        printf 'vm_name=%s\n' "$vm_name"
    } > "$platform_evidence/host-metadata.txt"

    if vm_fault_checkpoint "$platform" early; then
        :
    else
        run_vm_result=$?
        return 0
    fi
    if run_interruptible tart pull "$image"; then
        :
    else
        run_vm_result=$?
        return 0
    fi

    # Register the intended VM before clone so an interrupted or ambiguous
    # clone is treated as lifecycle uncertainty rather than silently orphaned.
    active_vm_name=$vm_name
    active_vm_evidence=$platform_evidence
    if run_interruptible tart clone "$image" "$vm_name"; then
        :
    else
        clone_status=$?
        finish_vm_run "$clone_status"
        return 0
    fi
    printf '%s\t%s\t%s\n' "$vm_name" "$image" "$digest" >> "$source_map"

    active_vm_run_previous_pid=$last_launched_pid
    active_vm_run_launching=true
    python3 -c "$session_exec_code" tart run "$vm_name" --no-graphics \
        > "$platform_evidence/tart-run.log" 2>&1 &
    active_vm_run_pid=$!
    last_launched_pid=$active_vm_run_pid
    active_vm_run_launching=false

    if wait_for_ssh "$vm_name"; then
        guest_ip=$resolved_guest_ip
    else
        echo "VM $vm_name did not become reachable" >&2
        finish_vm_run 1
        return 0
    fi

    guest_home_output=$platform_evidence/guest-home.txt
    # shellcheck disable=SC2016 # HOME must expand in the guest, not on the host.
    if ! ssh_guest "$guest_ip" 'printf %s "$HOME"' \
        > "$guest_home_output"; then
        echo "could not resolve guest HOME for $vm_name" >&2
        finish_vm_run 1
        return 0
    fi
    if ! read_single_line "$guest_home_output"; then
        echo "guest HOME is not a single line for $vm_name" >&2
        finish_vm_run 1
        return 0
    fi
    guest_home=$output_line
    case "$guest_home" in
        /*[!A-Za-z0-9_./-]*|*/)
            echo "guest HOME contains unsafe remote-shell characters for $vm_name" >&2
            finish_vm_run 1
            return 0
            ;;
        /*) ;;
        *)
            echo "guest HOME is not absolute for $vm_name" >&2
            finish_vm_run 1
            return 0
            ;;
    esac
    remote_root=$guest_home/car-go-clean-v040-acceptance
    if ! ssh_guest "$guest_ip" "mkdir -p '$remote_root/evidence'"; then
        echo "could not create guest evidence root for $vm_name" >&2
        finish_vm_run 1
        return 0
    fi
    if ! ssh_guest_input "$guest_ip" "$transfer_archive" \
        "mkdir -p '$remote_root/artifacts' && tar -xf - -C '$remote_root/artifacts'"; then
        echo "could not copy rehearsal artifacts to $vm_name" >&2
        finish_vm_run 1
        return 0
    fi

    verify_command="cd '$remote_root/artifacts' && if command -v sha256sum >/dev/null 2>&1; then sha256sum -c SHA256SUMS; else shasum -a 256 -c SHA256SUMS; fi"
    phase_status=0
    if ! ssh_guest "$guest_ip" "$verify_command" \
        > "$platform_evidence/guest-hash-verification.log" 2>&1; then
        phase_status=1
    fi

    case "$platform" in
        macos)
            base_path=/opt/homebrew/bin:/usr/local/bin:/usr/bin:/bin:/usr/sbin:/sbin
            dependency_preflight="command -v python3 >/dev/null && test -x /opt/homebrew/bin/brew && launchctl print gui/\$(id -u) >/dev/null"
            normalized_inventory="printf 'platform=macos\\n'; printf 'python=%s\\n' \"\$(python3 --version 2>&1)\"; printf 'brew=%s\\n' \"\$(/opt/homebrew/bin/brew --version | sed -n '1p')\""
            ;;
        linux)
            base_path=/usr/local/bin:/usr/bin:/bin:/usr/sbin:/sbin
            dependency_preflight="command -v python3 >/dev/null && command -v cc >/dev/null && systemctl --user show-environment >/dev/null"
            normalized_inventory="printf 'platform=linux\\n'; printf 'python=%s\\n' \"\$(python3 --version 2>&1)\"; cc_version=\$(cc --version 2>&1) || exit 1; printf 'cc=%s\\n' \"\$(printf '%s\\n' \"\$cc_version\" | sed -n '1p')\""
            ;;
    esac
    acceptance_path=$guest_home/.cargo/bin:$base_path
    bootstrap_command="set -eu; export PATH='$base_path'; if ! { $dependency_preflight; }; then echo 'required guest dependency preflight failed' >&2; exit 1; fi; cd '$remote_root/artifacts'; expected=\$(awk '{print \$1}' '$rustup_artifact.sha256'); test \"\$expected\" = '$rustup_digest'; if command -v sha256sum >/dev/null 2>&1; then actual=\$(sha256sum '$rustup_artifact' | awk '{print \$1}'); else actual=\$(shasum -a 256 '$rustup_artifact' | awk '{print \$1}'); fi; test \"\$actual\" = \"\$expected\"; chmod +x '$rustup_artifact'; bootstrap_log='$remote_root/rustup-bootstrap.log'; if ! './$rustup_artifact' -y --default-toolchain 1.95.0 --profile minimal --no-modify-path > \"\$bootstrap_log\" 2>&1; then sed 's#$guest_home#\$HOME#g' \"\$bootstrap_log\" | tail -n 40; exit 1; fi; rm -f \"\$bootstrap_log\"; export PATH='$acceptance_path'; test \"\$(rustc --version | awk '{print \$2}')\" = 1.95.0; test \"\$(cargo --version | awk '{print \$2}')\" = 1.95.0; $normalized_inventory; printf 'rustc=%s\\n' \"\$(rustc --version)\"; printf 'cargo=%s\\n' \"\$(cargo --version)\""
    if test "$phase_status" -eq 0 &&
        ! ssh_guest "$guest_ip" "$bootstrap_command" \
            > "$platform_evidence/tool-inventory.txt" 2>&1; then
        echo "deterministic toolchain bootstrap/preflight failed for $vm_name" >&2
        phase_status=1
    fi

    if test "$phase_status" -eq 0; then
        pre_command="PATH='$acceptance_path' CAR_GO_CLEAN_ACCEPTANCE_VERSION='$version' CAR_GO_CLEAN_ACCEPTANCE_SHA='$exact_sha' sh '$remote_root/artifacts/acceptance.sh' '$remote_root/artifacts' '$remote_root/evidence' pre-reboot"
        if ! ssh_guest "$guest_ip" "$pre_command" \
            > "$platform_evidence/pre-reboot-ssh.log" 2>&1; then
            phase_status=1
        fi
    fi

    if ! copy_phase_evidence "$guest_ip" "$remote_root" \
        "$platform_evidence/pre-reboot"; then
        echo "could not copy pre-reboot acceptance evidence from $vm_name" >&2
        phase_status=1
    fi

    if vm_fault_checkpoint "$platform" middle; then
        :
    else
        phase_status=$?
        finish_vm_run "$phase_status"
        return 0
    fi

    if test "$phase_status" -eq 0; then
        if guest_boot_identity "$platform" "$guest_ip"; then
            pre_boot_identity=$resolved_boot_identity
        else
            echo "could not capture pre-reboot boot identity for $vm_name" >&2
            phase_status=1
        fi
    fi

    if test "$phase_status" -eq 0; then
        printf '%s\n' "$pre_boot_identity" \
            > "$platform_evidence/pre-reboot-boot-identity.txt"
        if ssh_guest "$guest_ip" "sudo reboot" \
            > "$platform_evidence/reboot-ssh.log" 2>&1; then
            reboot_status=0
        else
            reboot_status=$?
        fi
        case "$reboot_status" in
            0|255) ;;
            *) phase_status=1 ;;
        esac
        if test "$phase_status" -eq 0; then
            if wait_for_reboot "$vm_name" "$guest_ip"; then
                new_guest_ip=$resolved_guest_ip
            else
                phase_status=1
            fi
        fi
    fi

    if test "$phase_status" -eq 0; then
        if guest_boot_identity "$platform" "$new_guest_ip"; then
            post_boot_identity=$resolved_boot_identity
        else
            echo "could not capture post-reboot boot identity for $vm_name" >&2
            phase_status=1
        fi
        if test "$phase_status" -eq 0 &&
            test "$post_boot_identity" = "$pre_boot_identity"; then
            echo "boot identity did not change for $vm_name" >&2
            phase_status=1
        fi
    fi

    if test "$phase_status" -eq 0; then
        printf '%s\n' "$post_boot_identity" \
            > "$platform_evidence/post-reboot-boot-identity.txt"
        post_command="PATH='$acceptance_path' CAR_GO_CLEAN_ACCEPTANCE_VERSION='$version' CAR_GO_CLEAN_ACCEPTANCE_SHA='$exact_sha' sh '$remote_root/artifacts/acceptance.sh' '$remote_root/artifacts' '$remote_root/evidence' post-reboot"
        if ! ssh_guest "$new_guest_ip" "$post_command" \
            > "$platform_evidence/post-reboot-ssh.log" 2>&1; then
            phase_status=1
        fi
        guest_ip=$new_guest_ip
    fi

    # Post-reboot extraction is unconditional once the reconnect succeeded.
    # The pre-reboot copy above is already durable even if this phase fails.
    if test -n "${new_guest_ip-}" &&
        ! copy_phase_evidence "$new_guest_ip" "$remote_root" \
            "$platform_evidence/post-reboot"; then
        echo "could not copy post-reboot acceptance evidence from $vm_name" >&2
        phase_status=1
    fi
    if vm_fault_checkpoint "$platform" late; then
        :
    else
        phase_status=$?
    fi
    finish_vm_run "$phase_status"
    return 0
}

run_interruptible tar -C "$artifact_dir" -cf "$transfer_archive" . ||
    die "could not create the closed guest artifact transfer"

overall_status=0
run_vm macos "$macos_image" "$macos_digest"
macos_status=$run_vm_result
if test "$macos_status" -eq 97 ||
    test -f "$evidence_dir/macos/lifecycle-failure.txt"; then
    echo "tart rehearsal: macOS VM lifecycle cleanup failed; refusing to start another VM" >&2
    exit 97
fi
if test "$macos_status" -ne 0; then
    overall_status=1
fi

run_vm linux "$linux_image" "$linux_digest"
linux_status=$run_vm_result
if test "$linux_status" -eq 97 ||
    test -f "$evidence_dir/linux/lifecycle-failure.txt"; then
    echo "tart rehearsal: Linux VM lifecycle cleanup failed" >&2
    exit 97
fi
if test "$linux_status" -ne 0; then
    overall_status=1
fi

if test "$overall_status" -ne 0; then
    die "one or more guest acceptance runs failed; sanitized evidence was copied before return"
fi
echo "Fresh Tart acceptance passed for macOS and Linux."
echo "VMs were intentionally preserved. Inventory and review them before explicit cleanup."
