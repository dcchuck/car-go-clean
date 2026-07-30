#!/bin/sh
set -eu

root=$(CDPATH='' cd -- "$(dirname -- "$0")/.." && pwd)
work_dir=$(mktemp -d)
fake_bin=$work_dir/bin
call_log=$work_dir/calls.log
tart_state=$work_dir/tart-state

cleanup() {
    rm -rf "$work_dir"
}
trap cleanup EXIT HUP INT TERM

mkdir -p "$fake_bin" "$tart_state"
: > "$call_log"

fail() {
    echo "release acceptance test: $*" >&2
    exit 1
}

assert_contains() {
    file=$1
    expected=$2
    grep -F -- "$expected" "$file" >/dev/null ||
        fail "$file does not contain: $expected"
}

assert_not_contains() {
    file=$1
    unexpected=$2
    if grep -F -- "$unexpected" "$file" >/dev/null; then
        fail "$file unexpectedly contains: $unexpected"
    fi
}

assert_status() {
    actual=$1
    expected=$2
    test "$actual" -eq "$expected" ||
        fail "expected exit $expected, got $actual (output: $output_file)"
}

run_capture() {
    output_file=$work_dir/output
    : > "$output_file"
    set +e
    "$@" > "$output_file" 2>&1
    run_status=$?
    set -e
}

cat > "$fake_bin/tart" <<'EOF'
#!/bin/sh
set -eu
{
    printf 'tart'
    printf ' %s' "$@"
    printf '\n'
} >> "$CALL_LOG"
case "${1-}" in
    list)
        python3 - "$TART_STATE" <<'PY'
import json
import pathlib
import sys

root = pathlib.Path(sys.argv[1])
items = []
for path in sorted(root.glob("*.vm")):
    name, state = path.read_text().strip().split("\t", 1)
    items.append({
        "Source": "local",
        "Name": name,
        "Disk": 64,
        "Size": 5,
        "Accessed": "2026-07-30T00:00:00Z",
        "Running": state == "running",
        "State": state,
    })
print(json.dumps(items))
PY
        ;;
    pull)
        ;;
    clone)
        source_ref=$2
        name=$3
        test ! -e "$TART_STATE/$name.vm"
        printf '%s\tstopped\n' "$name" > "$TART_STATE/$name.vm"
        printf '%s\n' "$source_ref" > "$TART_STATE/$name.source"
        ;;
    run)
        name=$2
        printf '%s\trunning\n' "$name" > "$TART_STATE/$name.vm"
        ;;
    ip)
        case "$2" in
            *macos*) printf '192.0.2.10\n' ;;
            *linux*) printf '192.0.2.20\n' ;;
            *) printf '192.0.2.30\n' ;;
        esac
        ;;
    stop)
        name=$2
        if test -e "$TART_STATE/$name.vm"; then
            printf '%s\tstopped\n' "$name" > "$TART_STATE/$name.vm"
        fi
        ;;
    delete)
        name=$2
        if test "${TART_KEEP_NAME-}" != "$name"; then
            rm -f "$TART_STATE/$name.vm" "$TART_STATE/$name.source"
        fi
        ;;
    prune)
        ;;
    *)
        exit 64
        ;;
esac
EOF

cat > "$fake_bin/sshpass" <<'EOF'
#!/bin/sh
set -eu
test "$1" = -p
shift 2
exec "$@"
EOF

cat > "$fake_bin/ssh" <<'EOF'
#!/bin/sh
set -eu
{
    printf 'ssh'
    printf ' %s' "$@"
    printf '\n'
} >> "$CALL_LOG"
host=
command=
for argument do
    case "$argument" in
        admin@*) host=$argument ;;
        *) command=$argument ;;
    esac
done
case "$command" in
    'printf ready')
        if test -e "$TART_STATE/rebooting"; then
            rm -f "$TART_STATE/rebooting"
            exit 255
        fi
        printf 'ready'
        ;;
    *'printf %s "$HOME"'*)
        printf '/home/admin'
        ;;
    *'acceptance.sh'*'pre-reboot'*)
        ;;
    *'sudo reboot'*)
        : > "$TART_STATE/rebooting"
        ;;
    *'acceptance.sh'*'post-reboot'*)
        case "$host:$FAIL_ACCEPTANCE_HOST" in
            admin@192.0.2.20:linux) exit 23 ;;
        esac
        ;;
    *)
        ;;
esac
EOF

cat > "$fake_bin/scp" <<'EOF'
#!/bin/sh
set -eu
{
    printf 'scp'
    printf ' %s' "$@"
    printf '\n'
} >> "$CALL_LOG"
for argument do
    destination=$argument
done
case "$*" in
    *admin@*:*evidence*/*)
        mkdir -p "$destination"
        printf 'sanitized fixture evidence\n' > "$destination/transcript.log"
        ;;
esac
EOF

chmod +x "$fake_bin/tart" "$fake_bin/sshpass" "$fake_bin/ssh" "$fake_bin/scp"

export CALL_LOG="$call_log"
export TART_STATE="$tart_state"

# Inventory uses Tart's JSON interface and joins a separately preserved source map.
printf 'alpha\trunning\n' > "$tart_state/alpha.vm"
printf 'legacy\tstopped\n' > "$tart_state/legacy.vm"
source_map=$work_dir/source-map.tsv
mac_digest=aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa
printf 'alpha\tghcr.io/cirruslabs/macos-sequoia-base@sha256:%s\t%s\n' \
    "$mac_digest" "$mac_digest" > "$source_map"
inventory=$work_dir/inventory.tsv
PATH="$fake_bin:/usr/bin:/bin" \
    CAR_GO_CLEAN_TART_HOME="$work_dir/tart-home" \
    "$root/scripts/release/tart-inventory.sh" "$inventory" "$source_map"
assert_contains "$inventory" \
    "alpha	running	ghcr.io/cirruslabs/macos-sequoia-base@sha256:$mac_digest	$mac_digest"
assert_contains "$inventory" "legacy	stopped	UNKNOWN_SOURCE	UNKNOWN_DIGEST"
assert_contains "$inventory" "# tart_storage_bytes	"
assert_contains "$inventory" "# host_df	"

# Cleanup is inert without the exact confirmation and touches only concrete names.
: > "$call_log"
run_capture env PATH="$fake_bin:/usr/bin:/bin" CALL_LOG="$call_log" \
    TART_STATE="$tart_state" \
    "$root/scripts/release/tart-cleanup.sh" "$inventory"
test "$run_status" -ne 0 || fail "cleanup ran without explicit confirmation"
assert_not_contains "$call_log" "tart delete"
assert_contains "$output_file" "alpha"
assert_contains "$output_file" "legacy"

run_capture env PATH="$fake_bin:/usr/bin:/bin" CALL_LOG="$call_log" \
    TART_STATE="$tart_state" CAR_GO_CLEAN_TART_HOME="$work_dir/tart-home" \
    CAR_GO_CLEAN_TART_DELETE_ALL=YES \
    "$root/scripts/release/tart-cleanup.sh" "$inventory"
assert_status "$run_status" 0
assert_contains "$call_log" "tart stop alpha"
assert_contains "$call_log" "tart stop legacy"
assert_contains "$call_log" "tart delete alpha"
assert_contains "$call_log" "tart delete legacy"
assert_contains "$call_log" "tart prune --entries caches --space-budget 0"
assert_not_contains "$call_log" "tart prune --entries vms"

# A VM that appeared after inventory is never broadened into deletion; nonempty final
# inventory is instead a hard failure.
printf 'listed\tstopped\n' > "$tart_state/listed.vm"
printf 'appeared-later\tstopped\n' > "$tart_state/appeared-later.vm"
late_inventory=$work_dir/late-inventory.tsv
printf 'listed\tstopped\tUNKNOWN_SOURCE\tUNKNOWN_DIGEST\n' > "$late_inventory"
: > "$call_log"
run_capture env PATH="$fake_bin:/usr/bin:/bin" CALL_LOG="$call_log" \
    TART_STATE="$tart_state" CAR_GO_CLEAN_TART_DELETE_ALL=YES \
    "$root/scripts/release/tart-cleanup.sh" "$late_inventory"
test "$run_status" -ne 0 || fail "cleanup accepted a nonempty final Tart inventory"
assert_contains "$call_log" "tart delete listed"
assert_not_contains "$call_log" "tart delete appeared-later"
assert_contains "$output_file" "appeared-later"
rm -f "$tart_state/appeared-later.vm"

# Rehearsal rejects tags, then uses exact refs, unique clones, guest hash
# verification, a real reboot boundary, and evidence extraction on failure.
artifacts=$work_dir/artifacts
evidence=$work_dir/evidence
mkdir -p "$artifacts"
printf 'candidate\n' > "$artifacts/candidate"
candidate_hash=$(shasum -a 256 "$artifacts/candidate" | awk '{ print $1 }')
printf '%s  candidate\n' "$candidate_hash" > "$artifacts/SHA256SUMS"

run_capture env PATH="$fake_bin:/usr/bin:/bin" CALL_LOG="$call_log" \
    TART_STATE="$tart_state" CAR_GO_CLEAN_TART_MACOS_IMAGE=ghcr.io/example/macos:latest \
    CAR_GO_CLEAN_TART_LINUX_IMAGE=ghcr.io/example/linux:latest \
    CAR_GO_CLEAN_ACCEPTANCE_VERSION=0.4.0 \
    CAR_GO_CLEAN_ACCEPTANCE_SHA=18e2b772698b5f9b67da64c4ad299beacfe219e9 \
    "$root/scripts/release/tart-rehearsal.sh" "$artifacts" "$evidence"
test "$run_status" -ne 0 || fail "rehearsal accepted movable image tags"
assert_contains "$output_file" "immutable ghcr.io"

linux_digest=bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb
mac_ref=ghcr.io/cirruslabs/macos-sequoia-base@sha256:$mac_digest
linux_ref=ghcr.io/cirruslabs/ubuntu@sha256:$linux_digest
: > "$call_log"
rm -rf "$evidence"
FAIL_ACCEPTANCE_HOST=linux
export FAIL_ACCEPTANCE_HOST
run_capture env PATH="$fake_bin:/usr/bin:/bin" CALL_LOG="$call_log" \
    TART_STATE="$tart_state" FAIL_ACCEPTANCE_HOST="$FAIL_ACCEPTANCE_HOST" \
    CAR_GO_CLEAN_TART_MACOS_IMAGE="$mac_ref" \
    CAR_GO_CLEAN_TART_LINUX_IMAGE="$linux_ref" \
    CAR_GO_CLEAN_ACCEPTANCE_VERSION=0.4.0 \
    CAR_GO_CLEAN_ACCEPTANCE_SHA=18e2b772698b5f9b67da64c4ad299beacfe219e9 \
    "$root/scripts/release/tart-rehearsal.sh" "$artifacts" "$evidence"
test "$run_status" -ne 0 || fail "rehearsal hid a guest acceptance failure"
assert_contains "$call_log" "tart pull $mac_ref"
assert_contains "$call_log" "tart pull $linux_ref"
assert_contains "$call_log" "tart clone $mac_ref car-go-clean-v040-macos-"
assert_contains "$call_log" "tart clone $linux_ref car-go-clean-v040-linux-"
assert_contains "$call_log" "acceptance.sh"
assert_contains "$call_log" "pre-reboot"
assert_contains "$call_log" "sudo reboot"
assert_contains "$call_log" "post-reboot"
assert_contains "$call_log" "scp"
test -f "$evidence/macos/transcript.log"
test -f "$evidence/linux/transcript.log"
assert_contains "$evidence/source-map.tsv" "$mac_ref"
assert_contains "$evidence/source-map.tsv" "$linux_ref"

# Guest acceptance exercises real script branches against fake Cargo and
# car-go-clean binaries. A failing assertion still sanitizes and preserves the
# transcript.
fake_acceptance=$work_dir/fake-acceptance
fake_guest_home=$work_dir/guest-home
fake_service_state=$work_dir/fake-service-state
fake_review_path=$work_dir/fake-review-path
fake_error=$work_dir/fake-error
fake_current_cgc=$fake_acceptance/car-go-clean-current
mkdir -p "$fake_acceptance" "$fake_guest_home"
printf 'no no no\n' > "$fake_service_state"
: > "$fake_error"

cat > "$fake_acceptance/uname" <<'EOF'
#!/bin/sh
case "${1-}" in
    -s) printf 'Linux\n' ;;
    -m) printf 'aarch64\n' ;;
    *) printf 'Linux\n' ;;
esac
EOF

cat > "$fake_acceptance/ruby" <<'EOF'
#!/bin/sh
test "$1" = -c
test -f "$2"
test "$(basename "$2")" = car-go-clean.rb
grep -F 'version "0.4.0"' "$2" >/dev/null
grep -F "url \"file://$FAKE_ACCEPTANCE_ARTIFACTS/car-go-clean-aarch64-unknown-linux-musl.tar.xz\"" \
    "$2" >/dev/null
test "$(grep -c 'url \"file://' "$2")" -eq 1
grep -F 'car-go-clean-aarch64-apple-darwin.tar.xz' "$2" >/dev/null
grep -F 'car-go-clean-x86_64-unknown-linux-musl.tar.xz' "$2" >/dev/null
echo "Syntax OK"
EOF

cat > "$fake_acceptance/cargo" <<'EOF'
#!/bin/sh
set -eu
printf 'cargo %s\n' "$*" >> "$CALL_LOG"
case "$1" in
    new)
        mkdir -p "$2/src"
        printf '[package]\nname = "fixture"\nversion = "0.1.0"\n' > "$2/Cargo.toml"
        ;;
    build)
        manifest=
        while test "$#" -gt 0; do
            if test "$1" = --manifest-path; then
                manifest=$2
                break
            fi
            shift
        done
        mkdir -p "$(dirname "$manifest")/target"
        dd if=/dev/zero of="$(dirname "$manifest")/target/fixture" bs=1024 count=1 \
            >/dev/null 2>&1
        ;;
    clean)
        exit 0
        ;;
esac
EOF

cat > "$fake_current_cgc" <<'EOF'
#!/bin/sh
set -eu
printf 'car-go-clean %s\n' "$*" >> "$CALL_LOG"
command=${1-}
shift || :
case "$command" in
    version)
        printf '0.4.0\n'
        ;;
    health)
        printf 'Health\n\nCleanup authority\n  Config source: fixture\n'
        ;;
    service)
        action=$1
        read -r installed enabled running < "$FAKE_SERVICE_STATE"
        case "$action" in
            status) ;;
            install) installed=yes; enabled=yes; running=yes ;;
            stop) installed=yes; enabled=no; running=no ;;
            start) installed=yes; enabled=yes; running=yes ;;
            uninstall) installed=no; enabled=no; running=no ;;
            *) exit 64 ;;
        esac
        printf '%s %s %s\n' "$installed" "$enabled" "$running" > "$FAKE_SERVICE_STATE"
        printf 'Service\n  Installed: %s\n  Enabled: %s\n  Running: %s\n' \
            "$installed" "$enabled" "$running"
        ;;
    config)
        subcommand=${1-}
        config_file=
        for argument do
            if test "$argument" = --config; then
                previous=--config
            elif test "${previous-}" = --config; then
                config_file=$argument
                previous=
            fi
        done
        if test "$subcommand" = migrate; then
            test -n "$config_file"
            sed 's/^excludes[[:space:]]*=/override_excludes =/' "$config_file" \
                > "$config_file.tmp"
            mv "$config_file.tmp" "$config_file"
            printf 'Migrated %s\n' "$config_file"
            exit 0
        fi
        test -n "$config_file"
        if grep -F 'scan_dirz' "$config_file" >/dev/null; then
            echo "unknown field scan_dirz" >&2
            exit 1
        fi
        if grep -F 'CAR_GO_CLEAN_ACCEPTANCE_UNDEFINED' "$config_file" >/dev/null; then
            echo "environment variable is not set" >&2
            exit 1
        fi
        if grep -E '^excludes[[:space:]]*=' "$config_file" >/dev/null; then
            echo 'warning: `excludes` is deprecated' >&2
        fi
        root=$(awk -F '"' '/^scan_dirs/ { print $2; exit }' "$config_file")
        printf 'scan_dirs = ["%s"]\n' "$root"
        printf 'project_dirs = []\nextra_excludes = []\n'
        if grep -E '^override_excludes[[:space:]]*=' "$config_file" >/dev/null; then
            printf 'override_excludes = ["node_modules"]\n'
        fi
        printf 'clean_interval = "24h"\nscan_interval = "24h"\n'
        printf 'target_quiet_period = "1ms"\nlog_level = "info"\n'
        ;;
    run)
        dry=false
        review=
        json=false
        config_file=
        while test "$#" -gt 0; do
            case "$1" in
                --dry-run) dry=true; shift ;;
                --review) review=$2; shift 2 ;;
                --json) json=true; shift ;;
                --config) config_file=$2; shift 2 ;;
                --state-dir) shift 2 ;;
                *) shift ;;
            esac
        done
        if test "$dry" = true; then
            root=$(awk -F '"' '/^scan_dirs/ { print $2; exit }' "$config_file")
            if test "${FAIL_ACCEPTANCE_STEP-}" = cargo-failure &&
                printf '%s\n' "$root" | grep -F 'cargo-failure' >/dev/null; then
                echo "fixture forced acceptance assertion failure" >&2
                exit 17
            fi
            manifests=$(find "$root" -name Cargo.toml -type f -print 2>/dev/null || :)
            project=$(printf '%s\n' "$manifests" | head -n 1)
            project=${project%/Cargo.toml}
            printf '%s\n' "$project" > "$FAKE_REVIEW_PATH"
            printf '%s\n' "$manifests" | while IFS= read -r manifest; do
                if test -n "$manifest"; then
                    printf 'Cleanable: %s\n' "${manifest%/Cargo.toml}"
                fi
            done
            printf 'Review ID: 42\nCandidate bytes: 1024\n'
            case "$root" in
                *incomplete)
                    echo "$root/denied: Permission denied"
                    exit 2
                    ;;
            esac
            exit 0
        fi
        if test -n "$review"; then
            project=$(cat "$FAKE_REVIEW_PATH")
            if ! cargo clean --manifest-path "$project/Cargo.toml" 2> "$FAKE_ERROR"; then
                cat "$FAKE_ERROR" >&2
                exit 1
            fi
            rm -rf "$project/target"
            if test "$json" = true; then
                printf '{"format_version":1,"event":"target","data":{"path":"%s"}}\n' "$project"
                printf '{"format_version":1,"command":"run","review_id":%s,"outcome":{"code":0},"data":{"bytes_recovered":1024}}\n' "$review"
            else
                printf 'Run complete: cleaned=1 skipped=0 recovered=1024 errors=0\n'
            fi
            exit 0
        fi
        exit 64
        ;;
    scan)
        exit 0
        ;;
    stats)
        printf '{"format_version":1,"command":"stats","outcome":{"code":0},"data":{"total_bytes":1024}}\n'
        ;;
    logs)
        error=$(cat "$FAKE_ERROR")
        printf '{"format_version":1,"command":"logs","outcome":{"code":0},"data":{"errors":[{"message":"%s"}]}}\n' "$error"
        ;;
    *)
        exit 64
        ;;
esac
EOF

cat > "$fake_acceptance/systemctl" <<'EOF'
#!/bin/sh
set -eu
printf 'systemctl %s\n' "$*" >> "$CALL_LOG"
case "$*" in
    "--user stop car-go-clean.service")
        printf 'yes yes no\n' > "$FAKE_SERVICE_STATE"
        ;;
    *)
        ;;
esac
EOF

cat > "$fake_acceptance/car-go-clean-installer.sh" <<'EOF'
#!/bin/sh
set -eu
install_dir=
while test "$#" -gt 0; do
    case "$1" in
        --install-dir) install_dir=$2; shift 2 ;;
        *) shift ;;
    esac
done
mkdir -p "$install_dir"
cp "$FAKE_CURRENT_CGC" "$install_dir/car-go-clean"
chmod +x "$install_dir/car-go-clean"
EOF

cat > "$fake_acceptance/car-go-clean-upgrade.sh" <<'EOF'
#!/bin/sh
set -eu
execute=
while test "$#" -gt 0; do
    case "$1" in
        --execute-review) execute=$2; shift 2 ;;
        *) shift ;;
    esac
done
session="$CAR_GO_CLEAN_UPGRADE_STATE_DIR/fake-session"
mkdir -p "$CAR_GO_CLEAN_UPGRADE_STATE_DIR"
if test -z "$execute"; then
    read -r installed enabled running < "$FAKE_SERVICE_STATE"
    printf '%s\n' "$running" > "$session"
    if test "$running" = yes; then
        printf 'yes yes no\n' > "$FAKE_SERVICE_STATE"
    fi
    destination=$(command -v car-go-clean)
    cp "$FAKE_CURRENT_CGC" "$destination"
    chmod +x "$destination"
    printf 'Review ID: 42\nCandidate bytes: 1024\n'
else
    original=$(cat "$session")
    if test "$original" = yes; then
        printf 'yes yes yes\n' > "$FAKE_SERVICE_STATE"
    fi
    rm -f "$session"
    echo "Upgrade to car-go-clean 0.4.0 completed."
fi
EOF

for old_version in 0.2.0 0.3.0; do
    old_fixture=$fake_acceptance/car-go-clean-v$old_version-aarch64-unknown-linux-musl
    {
        printf '%s\n' '#!/bin/sh'
        # shellcheck disable=SC2016 # These variables belong in the generated fixture.
        printf 'if test "${1-}" = version; then echo "%s"; exit 0; fi\n' "$old_version"
        # shellcheck disable=SC2016 # These variables belong in the generated fixture.
        printf 'exec "$FAKE_CURRENT_CGC" "$@"\n'
    } > "$old_fixture"
    chmod +x "$old_fixture"
done

printf 'archive\n' > "$fake_acceptance/car-go-clean-aarch64-unknown-linux-musl.tar.xz"
printf 'fixture  car-go-clean-aarch64-unknown-linux-musl.tar.xz\n' \
    > "$fake_acceptance/car-go-clean-aarch64-unknown-linux-musl.tar.xz.sha256"
printf 'fixture checksums\n' > "$fake_acceptance/car-go-clean-shell-assets.sha256"
cat > "$fake_acceptance/car-go-clean.rb" <<'EOF'
class CarGoClean < Formula
  on_macos do
    on_arm do
      url "https://github.com/dcchuck/car-go-clean/releases/download/v0.4.0/car-go-clean-aarch64-apple-darwin.tar.xz"
      sha256 "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb"
    end
  end
  on_linux do
    on_arm do
  url "https://github.com/dcchuck/car-go-clean/releases/download/v0.4.0/car-go-clean-aarch64-unknown-linux-musl.tar.xz"
  sha256 "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
    end
    on_intel do
      url "https://github.com/dcchuck/car-go-clean/releases/download/v0.4.0/car-go-clean-x86_64-unknown-linux-musl.tar.xz"
      sha256 "cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc"
    end
  end
end
EOF
chmod +x "$fake_acceptance/uname" "$fake_acceptance/ruby" \
    "$fake_acceptance/cargo" "$fake_current_cgc" \
    "$fake_acceptance/systemctl" "$fake_acceptance/car-go-clean-installer.sh" \
    "$fake_acceptance/car-go-clean-upgrade.sh"

acceptance_evidence=$work_dir/acceptance-evidence
: > "$call_log"
for phase in pre-reboot post-reboot; do
    run_capture env PATH="$fake_acceptance:/usr/bin:/bin" HOME="$fake_guest_home" \
        CALL_LOG="$call_log" FAKE_CURRENT_CGC="$fake_current_cgc" \
        FAKE_SERVICE_STATE="$fake_service_state" \
        FAKE_REVIEW_PATH="$fake_review_path" FAKE_ERROR="$fake_error" \
        FAKE_ACCEPTANCE_ARTIFACTS="$fake_acceptance" \
        CAR_GO_CLEAN_ACCEPTANCE_VERSION=0.4.0 \
        CAR_GO_CLEAN_ACCEPTANCE_SHA=18e2b772698b5f9b67da64c4ad299beacfe219e9 \
        "$root/scripts/release/acceptance.sh" \
        "$fake_acceptance" "$acceptance_evidence" "$phase"
    if test "$run_status" -ne 0; then
        test -f "$acceptance_evidence/transcript.log" &&
            cat "$acceptance_evidence/transcript.log" >&2
        fail "guest acceptance $phase fixture failed with exit $run_status"
    fi
done
for milestone in \
    shell-install formula-install version-health disposable-build dry-run review \
    no-scan narrowed-scope cargo-failure incomplete-scan complete-scan \
    strict-config migration-roundtrip service-pre-reboot service-post-reboot \
    upgrade-matrix macos-library-privacy
do
    assert_contains "$acceptance_evidence/milestones.tsv" "$milestone	PASS"
done
assert_contains "$call_log" "cargo new"
assert_contains "$call_log" "car-go-clean run --dry-run"
assert_contains "$call_log" "car-go-clean run --review"
assert_contains "$call_log" "car-go-clean service install"
assert_contains "$call_log" "car-go-clean service stop"
assert_contains "$call_log" "car-go-clean service start"
assert_contains "$call_log" "car-go-clean service uninstall"

failed_evidence=$work_dir/failed-evidence
run_capture env PATH="$fake_acceptance:/usr/bin:/bin" HOME="$fake_guest_home" \
    CALL_LOG="$call_log" FAKE_CURRENT_CGC="$fake_current_cgc" \
    FAKE_SERVICE_STATE="$fake_service_state" \
    FAKE_REVIEW_PATH="$fake_review_path" FAKE_ERROR="$fake_error" \
    FAKE_ACCEPTANCE_ARTIFACTS="$fake_acceptance" \
    CAR_GO_CLEAN_ACCEPTANCE_VERSION=0.4.0 \
    CAR_GO_CLEAN_ACCEPTANCE_SHA=18e2b772698b5f9b67da64c4ad299beacfe219e9 \
    FAIL_ACCEPTANCE_STEP=cargo-failure \
    "$root/scripts/release/acceptance.sh" \
    "$fake_acceptance" "$failed_evidence" pre-reboot
test "$run_status" -ne 0 || fail "guest acceptance hid a failed assertion"
test -s "$failed_evidence/transcript.log"
assert_contains "$failed_evidence/milestones.tsv" "cargo-failure	FAIL"
assert_not_contains "$failed_evidence/transcript.log" "$work_dir"

echo "release acceptance harness tests passed"
