#!/bin/sh
set -eu

root=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
upgrade="$root/packaging/release/car-go-clean-upgrade.sh"
work_dir=$(mktemp -d)
fake_bin="$work_dir/bin"
case_root="$work_dir/cases"

cleanup() {
    rm -rf "$work_dir"
}
trap cleanup EXIT HUP INT TERM

mkdir -p "$fake_bin" "$case_root"

cat > "$fake_bin/uname" <<'EOF'
#!/bin/sh
case "$1" in
    -s) printf '%s\n' "$TEST_PLATFORM" ;;
    -m) printf '%s\n' "${TEST_MACHINE-arm64}" ;;
    *) exit 64 ;;
esac
EOF

cat > "$fake_bin/id" <<'EOF'
#!/bin/sh
test "$1" = "-u"
printf '%s\n' 501
EOF

cat > "$fake_bin/car-go-clean" <<'EOF'
#!/bin/sh
set -eu
printf 'car-go-clean %s\n' "$*" >> "$CALL_LOG"
version=$(cat "$VERSION_FILE")
case "$*" in
    version)
        printf '%s\n' "$version"
        ;;
    "service status")
        state=$(cat "$SERVICE_STATE")
        printf 'Service\n  Platform: fixture\n  Binary: fixture\n  Definition: fixture\n  State: %s\n' "$state"
        ;;
    "service stop"|"service start")
        echo "upgrade helper called a v0.2/v0.3 lifecycle verb" >&2
        exit 95
        ;;
    config)
        if [ "${LEGACY_EXCLUDES-0}" = 1 ]; then
            echo 'warning: `excludes` is deprecated in v0.4' >&2
        fi
        echo 'scan_dirs = []'
        exit "${CONFIG_EXIT-0}"
        ;;
    "run --dry-run --all")
        if [ -n "${PREVIEW_TEXT-}" ]; then
            printf '%s\n' "$PREVIEW_TEXT"
        else
            printf 'Review ID: %s\nCandidate bytes: 1024\n' "${REVIEW_ID-42}"
        fi
        exit "${PREVIEW_EXIT-0}"
        ;;
    run\ --review\ *)
        review=${3-}
        printf '%s\n' "$review" > "$EXECUTED_REVIEW"
        if [ -n "${EXECUTE_ERROR-}" ]; then
            printf '%s\n' "$EXECUTE_ERROR" >&2
        fi
        exit "${EXECUTE_EXIT-0}"
        ;;
    *)
        echo "unexpected car-go-clean invocation: $*" >&2
        exit 64
        ;;
esac
EOF

cat > "$fake_bin/launchctl" <<'EOF'
#!/bin/sh
set -eu
printf 'launchctl %s\n' "$*" >> "$CALL_LOG"
case "$1" in
    bootout)
        printf 'stopped\n' > "$SERVICE_STATE"
        ;;
    bootstrap|kickstart)
        test "${RESTORE_FAIL-0}" != 1
        printf 'running\n' > "$SERVICE_STATE"
        ;;
    *)
        exit 64
        ;;
esac
EOF

cat > "$fake_bin/systemctl" <<'EOF'
#!/bin/sh
set -eu
printf 'systemctl %s\n' "$*" >> "$CALL_LOG"
case "$*" in
    "--user stop car-go-clean.service")
        printf 'stopped\n' > "$SERVICE_STATE"
        ;;
    "--user start car-go-clean.service")
        test "${RESTORE_FAIL-0}" != 1
        printf 'running\n' > "$SERVICE_STATE"
        ;;
    *)
        exit 64
        ;;
esac
EOF

cat > "$fake_bin/brew" <<'EOF'
#!/bin/sh
set -eu
printf 'brew %s\n' "$*" >> "$CALL_LOG"
case "$1" in
    update)
        test "${BREW_UPDATE_FAIL-0}" != 1
        ;;
    list)
        test "${BREW_INSTALLED-1}" = 1
        ;;
    upgrade|install)
        test "${BREW_REPLACE_FAIL-0}" != 1
        if [ "${WRONG_NEW_VERSION-0}" = 1 ]; then
            printf '0.4.1\n' > "$VERSION_FILE"
        else
            printf '0.4.0\n' > "$VERSION_FILE"
        fi
        ;;
    *)
        exit 64
        ;;
esac
EOF

cat > "$fake_bin/curl" <<'EOF'
#!/bin/sh
set -eu
output=
url=
while [ "$#" -gt 0 ]; do
    case "$1" in
        -o)
            output=$2
            shift 2
            ;;
        *)
            url=$1
            shift
            ;;
    esac
done
printf 'curl %s\n' "$url" >> "$CALL_LOG"
test "${SHELL_DOWNLOAD_FAIL-0}" != 1
case "$url" in
    */car-go-clean-installer.sh)
        cat > "$output" <<'INSTALLER'
#!/bin/sh
set -eu
printf 'installer %s\n' "$*" >> "$CALL_LOG"
test "${SHELL_REPLACE_FAIL-0}" != 1
if [ "${WRONG_NEW_VERSION-0}" = 1 ]; then
    printf '0.4.1\n' > "$VERSION_FILE"
else
    printf '0.4.0\n' > "$VERSION_FILE"
fi
INSTALLER
        ;;
    */car-go-clean-shell-assets.sha256)
        printf 'fixture-sha256  car-go-clean-installer.sh\n' > "$output"
        printf 'upgrade-fixture-sha256  car-go-clean-upgrade.sh\n' >> "$output"
        ;;
    *)
        exit 64
        ;;
esac
EOF

cat > "$fake_bin/shasum" <<'EOF'
#!/bin/sh
set -eu
for argument do
    file=$argument
done
printf 'fixture-sha256  %s\n' "$file"
EOF

cat > "$fake_bin/sha256sum" <<'EOF'
#!/bin/sh
set -eu
for argument do
    file=$argument
done
printf 'fixture-sha256  %s\n' "$file"
EOF

cat > "$fake_bin/stat" <<'EOF'
#!/bin/sh
set -eu
if [ "${GNU_STAT_FIXTURE-0}" = 1 ]; then
    case "$1" in
        -f)
            printf 'GNU filesystem status output\n'
            ;;
        -c)
            printf '600\n'
            ;;
        *)
            exit 64
            ;;
    esac
else
    exec /usr/bin/stat "$@"
fi
EOF

chmod +x "$fake_bin"/*

new_case() {
    name=$1
    platform=$2
    old_version=$3
    old_state=$4
    method=$5
    current_case="$case_root/$name"
    home="$current_case/home"
    state_dir="$current_case/state"
    call_log="$current_case/calls"
    version_file="$current_case/version"
    service_state="$current_case/service-state"
    executed_review="$current_case/executed-review"
    output_file="$current_case/output"
    mkdir -p "$home" "$state_dir"
    : > "$call_log"
    printf '%s\n' "$old_version" > "$version_file"
    printf '%s\n' "$old_state" > "$service_state"
    rm -f "$executed_review"

    TEST_PLATFORM=$platform
    VERSION_FILE=$version_file
    SERVICE_STATE=$service_state
    CALL_LOG=$call_log
    EXECUTED_REVIEW=$executed_review
    CAR_GO_CLEAN_UPGRADE_STATE_DIR=$state_dir
    REVIEW_ID=42
    PREVIEW_EXIT=0
    PREVIEW_TEXT=
    CONFIG_EXIT=0
    EXECUTE_EXIT=0
    EXECUTE_ERROR=
    LEGACY_EXCLUDES=0
    BREW_INSTALLED=1
    BREW_UPDATE_FAIL=0
    BREW_REPLACE_FAIL=0
    SHELL_DOWNLOAD_FAIL=0
    SHELL_REPLACE_FAIL=0
    WRONG_NEW_VERSION=0
    GNU_STAT_FIXTURE=0
    RESTORE_FAIL=0
    export TEST_PLATFORM VERSION_FILE SERVICE_STATE CALL_LOG EXECUTED_REVIEW
    export CAR_GO_CLEAN_UPGRADE_STATE_DIR REVIEW_ID PREVIEW_EXIT PREVIEW_TEXT
    export CONFIG_EXIT EXECUTE_EXIT LEGACY_EXCLUDES BREW_INSTALLED
    export EXECUTE_ERROR RESTORE_FAIL
    export BREW_UPDATE_FAIL BREW_REPLACE_FAIL SHELL_DOWNLOAD_FAIL
    export SHELL_REPLACE_FAIL WRONG_NEW_VERSION
    export GNU_STAT_FIXTURE
}

run_upgrade() {
    if PATH="$fake_bin:/usr/bin:/bin" HOME="$home" \
        "$upgrade" "$@" > "$output_file" 2>&1; then
        run_status=0
    else
        run_status=$?
    fi
}

assert_status() {
    expected=$1
    test "$run_status" -eq "$expected" || {
        echo "expected exit $expected, got $run_status" >&2
        cat "$output_file" >&2
        exit 1
    }
}

assert_output_has() {
    grep -F "$1" "$output_file" >/dev/null || {
        echo "missing output: $1" >&2
        cat "$output_file" >&2
        exit 1
    }
}

assert_calls_have() {
    grep -F "$1" "$call_log" >/dev/null || {
        echo "missing call: $1" >&2
        cat "$call_log" >&2
        exit 1
    }
}

assert_calls_lack() {
    if grep -F "$1" "$call_log" >/dev/null; then
        echo "unexpected call: $1" >&2
        cat "$call_log" >&2
        exit 1
    fi
}

assert_session_mode_600() {
    session=$state_dir/upgrade-session
    test -f "$session"
    mode=$(stat -f '%Lp' "$session" 2>/dev/null || :)
    case "$mode" in
        [0-7][0-7][0-7]) ;;
        *) mode=$(stat -c '%a' "$session") ;;
    esac
    test "$mode" = 600
}

complete_upgrade() {
    run_upgrade --version 0.4.0 --method "$method"
    assert_status 0
    assert_session_mode_600
    test "$(cat "$service_state")" = stopped || test "$old_state" != running
    if [ "$old_state" != running ]; then
        assert_calls_lack "launchctl bootout"
        assert_calls_lack "systemctl --user stop car-go-clean.service"
    fi
    : > "$call_log"
    run_upgrade --version 0.4.0 --method "$method" --execute-review 42
    assert_status 0
    test "$(cat "$executed_review")" = 42
    test ! -e "$state_dir/upgrade-session"
}

# v0.2/v0.3 and active/stopped/absent states stay exact across both platforms.
for fixture in \
    mac-v02-active:Darwin:0.2.0:running:homebrew \
    mac-v03-stopped:Darwin:0.3.0:stopped:homebrew \
    mac-v02-absent:Darwin:0.2.0:'not installed':homebrew \
    linux-v03-active:Linux:0.3.0:running:shell \
    linux-v02-stopped:Linux:0.2.0:stopped:shell \
    linux-v03-absent:Linux:0.3.0:'not installed':shell
do
    old_ifs=$IFS
    IFS=:
    set -- $fixture
    IFS=$old_ifs
    new_case "$1" "$2" "$3" "$4" "$5"
    old_state=$4
    method=$5
    if [ "$1" = linux-v03-active ]; then
        PREVIEW_EXIT=2
        export PREVIEW_EXIT
    fi
    complete_upgrade
    case "$old_state" in
        running)
            test "$(cat "$service_state")" = running
            if [ "$TEST_PLATFORM" = Darwin ]; then
                assert_calls_have "launchctl bootstrap"
            else
                assert_calls_have "systemctl --user start car-go-clean.service"
            fi
            ;;
        stopped)
            test "$(cat "$service_state")" = stopped
            assert_calls_lack " start "
            assert_calls_lack "bootstrap"
            ;;
        "not installed")
            test "$(cat "$service_state")" = "not installed"
            assert_calls_lack " start "
            assert_calls_lack "bootstrap"
            ;;
    esac
done

# GNU stat accepts `-f` with different semantics; mode inspection still resumes.
new_case gnu-stat-session Linux 0.3.0 stopped homebrew
GNU_STAT_FIXTURE=1
export GNU_STAT_FIXTURE
old_state=stopped
method=homebrew
complete_upgrade

# Replacement failures restore only a service that was active before mutation.
new_case brew-rollback Darwin 0.2.0 running homebrew
BREW_REPLACE_FAIL=1
export BREW_REPLACE_FAIL
run_upgrade --version 0.4.0 --method homebrew
test "$run_status" -ne 0
test "$(cat "$service_state")" = running
assert_calls_have "launchctl bootout"
assert_calls_have "launchctl bootstrap"

new_case version-rollback Linux 0.3.0 running homebrew
WRONG_NEW_VERSION=1
export WRONG_NEW_VERSION
run_upgrade --version 0.4.0 --method homebrew
test "$run_status" -ne 0
test "$(cat "$service_state")" = running
assert_output_has "expected car-go-clean 0.4.0"
assert_calls_have "systemctl --user start car-go-clean.service"

# Failures after exact replacement leave the new service stopped with guidance.
new_case config-failure Darwin 0.2.0 running homebrew
CONFIG_EXIT=1
export CONFIG_EXIT
run_upgrade --version 0.4.0 --method homebrew
test "$run_status" -ne 0
test "$(cat "$service_state")" = stopped
assert_output_has "service remains stopped"
assert_output_has "rollback"
test ! -e "$state_dir/upgrade-session"

new_case preview-failure Linux 0.3.0 running homebrew
PREVIEW_EXIT=1
export PREVIEW_EXIT
run_upgrade --version 0.4.0 --method homebrew
test "$run_status" -ne 0
test "$(cat "$service_state")" = stopped
assert_output_has "preview failed"
assert_output_has "service remains stopped"
test ! -e "$state_dir/upgrade-session"

# A complete/incomplete preview still requires exactly one usable review ID.
for preview in no-id duplicate-id nonnumeric-id
do
    new_case "$preview" Darwin 0.2.0 running homebrew
    case "$preview" in
        no-id) PREVIEW_TEXT='Candidate bytes: 1024' ;;
        duplicate-id) PREVIEW_TEXT='Review ID: 42
Review ID: 43' ;;
        nonnumeric-id) PREVIEW_TEXT='Review ID: nope' ;;
    esac
    export PREVIEW_TEXT
    run_upgrade --version 0.4.0 --method homebrew
    test "$run_status" -ne 0
    test "$(cat "$service_state")" = stopped
    test ! -e "$state_dir/upgrade-session"
done

# Legacy configuration receives actionable migration guidance.
new_case legacy-config Darwin 0.3.0 stopped homebrew
LEGACY_EXCLUDES=1
export LEGACY_EXCLUDES
run_upgrade --version 0.4.0 --method homebrew
assert_status 0
assert_output_has 'legacy `excludes`'
assert_output_has "car-go-clean config migrate"

# Execution is resumable, exact-ID bound, and never repeats replacement/preview.
new_case execute-failure Linux 0.2.0 running homebrew
run_upgrade --version 0.4.0 --method homebrew
assert_status 0
: > "$call_log"
EXECUTE_EXIT=1
EXECUTE_ERROR='review expired'
export EXECUTE_EXIT
export EXECUTE_ERROR
run_upgrade --version 0.4.0 --method homebrew --execute-review 42
test "$run_status" -ne 0
test "$(cat "$service_state")" = stopped
test -f "$state_dir/upgrade-session"
assert_output_has "service remains stopped"
assert_output_has "fresh preview"
assert_calls_lack "brew "
assert_calls_lack "run --dry-run"

run_upgrade --version 0.4.0 --method homebrew --execute-review 41
test "$run_status" -ne 0
assert_output_has "does not match"
test -f "$state_dir/upgrade-session"

EXECUTE_EXIT=0
export EXECUTE_EXIT
printf '0.4.1\n' > "$version_file"
: > "$call_log"
run_upgrade --version 0.4.0 --method homebrew --execute-review 42
test "$run_status" -ne 0
assert_output_has "expected car-go-clean 0.4.0"
assert_calls_lack "run --review"
test -f "$state_dir/upgrade-session"

new_case restore-failure Darwin 0.2.0 running homebrew
run_upgrade --version 0.4.0 --method homebrew
assert_status 0
RESTORE_FAIL=1
export RESTORE_FAIL
run_upgrade --version 0.4.0 --method homebrew --execute-review 42
test "$run_status" -ne 0
test "$(cat "$service_state")" = stopped
test -f "$state_dir/upgrade-session"
assert_output_has "service remains stopped"

# Missing, malformed, symlinked, and broadly readable sessions fail closed.
new_case missing-session Darwin 0.4.0 stopped homebrew
run_upgrade --version 0.4.0 --method homebrew --execute-review 42
test "$run_status" -ne 0
assert_calls_lack "run --review"

for bad_id in nope -1 4x
do
    run_upgrade --version 0.4.0 --method homebrew --execute-review "$bad_id"
    test "$run_status" -ne 0
done

printf 'format=1\n' > "$state_dir/unsafe"
ln -s "$state_dir/unsafe" "$state_dir/upgrade-session"
run_upgrade --version 0.4.0 --method homebrew --execute-review 42
test "$run_status" -ne 0
assert_output_has "symlink"
rm "$state_dir/upgrade-session"

printf 'format=1\nversion=0.4.0\nmethod=homebrew\nservice_state=stopped\nreview_id=42\n' \
    > "$state_dir/upgrade-session"
chmod 644 "$state_dir/upgrade-session"
run_upgrade --version 0.4.0 --method homebrew --execute-review 42
test "$run_status" -ne 0
assert_output_has "mode 0600"

chmod 600 "$state_dir/upgrade-session"
printf 'unexpected=value\n' >> "$state_dir/upgrade-session"
run_upgrade --version 0.4.0 --method homebrew --execute-review 42
test "$run_status" -ne 0
assert_output_has "malformed"

# Exact v0.4.0, known methods, complete options, and supported OSes only.
new_case argument-validation Darwin 0.2.0 stopped homebrew
for invocation in \
    '--method homebrew' \
    '--version 0.4.1 --method homebrew' \
    '--version 0.4.0 --method package' \
    '--version 0.4.0 --method homebrew --bogus' \
    '--version 0.4.0' \
    '--version 0.4.0 --version 0.4.0 --method homebrew' \
    '--version 0.4.0 --method homebrew --method homebrew'
do
    old_ifs=$IFS
    IFS=' '
    set -- $invocation
    IFS=$old_ifs
    run_upgrade "$@"
    test "$run_status" -ne 0
done
TEST_PLATFORM=FreeBSD
export TEST_PLATFORM
run_upgrade --version 0.4.0 --method homebrew
test "$run_status" -ne 0
