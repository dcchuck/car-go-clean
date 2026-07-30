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
if [ "${BREW_ROLLBACK_FIXTURE-0}" = 1 ]; then
    test "$(cat "$BREW_LINKED_FORMULA")" != unlinked
    version=$(cat "$BREW_LINKED_VERSION_FILE")
fi
case "$*" in
    version)
        printf '%s\n' "$version"
        ;;
    "service status")
        state=$(cat "$SERVICE_STATE")
        printf 'Service\n  Platform: fixture\n  Binary: fixture\n  Definition: fixture\n  State: %s\n' "$state"
        ;;
    "service start")
        if [ "${BREW_ROLLBACK_FIXTURE-0}" = 1 ]; then
            test "$version" = "$ROLLBACK_EXPECTED_VERSION"
            printf 'running\n' > "$SERVICE_STATE"
            exit 0
        fi
        echo "upgrade helper called a v0.2/v0.3 lifecycle verb" >&2
        exit 95
        ;;
    "service stop")
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
        if [ -n "${EXECUTE_MARKER-}" ]; then
            : > "$EXECUTE_MARKER"
        fi
        if [ -n "${EXECUTE_FIFO-}" ]; then
            IFS= read -r release < "$EXECUTE_FIFO"
            test "$release" = release
        fi
        if [ -n "${EXECUTE_ERROR-}" ]; then
            printf '%s\n' "$EXECUTE_ERROR" >&2
        fi
        if [ "${EXECUTE_SIGNAL-0}" = 1 ]; then
            kill -TERM "$PPID"
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
    bootstrap)
        test "${RESTORE_FAIL-0}" != 1
        printf 'running\n' > "$SERVICE_STATE"
        ;;
    kickstart)
        test "${RESTORE_FAIL-0}" != 1
        printf 'running\n' > "$SERVICE_STATE"
        if [ "${RESTORE_SIGNAL-0}" = 1 ] &&
            [ ! -e "$RESTORE_SIGNAL_MARKER" ]; then
            : > "$RESTORE_SIGNAL_MARKER"
            kill -TERM "$PPID"
        fi
        ;;
    print)
        test "$(cat "$SERVICE_STATE")" = running
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
    "--user is-active --quiet car-go-clean.service")
        test "$(cat "$SERVICE_STATE")" = running
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
    tap)
        test "$#" -eq 1
        cat "$BREW_TAPS_FILE"
        ;;
    tap-new)
        test "$#" -eq 2
        printf '%s\n' "$2" >> "$BREW_TAPS_FILE"
        ;;
    extract)
        test "${BREW_ROLLBACK_FIXTURE-0}" = 1
        test "$#" -eq 5
        test "$2" = --force
        case "$3" in
            --version=*) extracted_version=${3#--version=} ;;
            *) exit 64 ;;
        esac
        test "$4" = dcchuck/tap/car-go-clean
        grep -Fqx -- "$5" "$BREW_TAPS_FILE"
        printf '%s\n' "$extracted_version" > "$BREW_EXTRACTED_VERSION_FILE"
        ;;
    unlink)
        test "${BREW_ROLLBACK_FIXTURE-0}" = 1
        test "$#" -eq 2
        test "$2" = car-go-clean
        printf 'unlinked\n' > "$BREW_LINKED_FORMULA"
        ;;
    link)
        test "${BREW_ROLLBACK_FIXTURE-0}" = 1
        test "$#" -eq 4
        test "$2" = --force
        test "$3" = --overwrite
        test "$4" = "$(cat "$BREW_INSTALLED_FORMULA_FILE")"
        test "${BREW_LINK_FAIL-0}" != 1
        printf '%s\n' "$4" > "$BREW_LINKED_FORMULA"
        if [ "${BREW_LINK_WRONG_VERSION-0}" = 1 ]; then
            printf '0.4.0\n' > "$BREW_LINKED_VERSION_FILE"
        else
            cat "$BREW_EXTRACTED_VERSION_FILE" > "$BREW_LINKED_VERSION_FILE"
        fi
        ;;
    update)
        test "${BREW_UPDATE_FAIL-0}" != 1
        ;;
    list)
        test "${BREW_INSTALLED-1}" = 1
        ;;
    upgrade)
        test "${BREW_REPLACE_FAIL-0}" != 1
        if [ "${WRONG_NEW_VERSION-0}" = 1 ]; then
            printf '0.4.1\n' > "$VERSION_FILE"
            printf '0.4.1\n' > "$BREW_LINKED_VERSION_FILE"
        else
            printf '0.4.0\n' > "$VERSION_FILE"
            printf '0.4.0\n' > "$BREW_LINKED_VERSION_FILE"
        fi
        printf 'car-go-clean\n' > "$BREW_LINKED_FORMULA"
        ;;
    install)
        case "${2-}" in
            */car-go-clean@*)
                test "${BREW_ROLLBACK_FIXTURE-0}" = 1
                test "$(cat "$BREW_LINKED_FORMULA")" = unlinked
                extracted_version=$(cat "$BREW_EXTRACTED_VERSION_FILE")
                test "$2" = "${USER}/car-go-clean-rollback/car-go-clean@$extracted_version"
                printf '%s\n' "$2" > "$BREW_INSTALLED_FORMULA_FILE"
                ;;
            *)
                test "${BREW_REPLACE_FAIL-0}" != 1
                if [ "${WRONG_NEW_VERSION-0}" = 1 ]; then
                    printf '0.4.1\n' > "$VERSION_FILE"
                    printf '0.4.1\n' > "$BREW_LINKED_VERSION_FILE"
                else
                    printf '0.4.0\n' > "$VERSION_FILE"
                    printf '0.4.0\n' > "$BREW_LINKED_VERSION_FILE"
                fi
                printf 'car-go-clean\n' > "$BREW_LINKED_FORMULA"
                ;;
        esac
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
    execute_marker="$current_case/execute-marker"
    restore_signal_marker="$current_case/restore-signal-marker"
    brew_taps_file="$current_case/brew-taps"
    brew_linked_formula="$current_case/brew-linked-formula"
    brew_linked_version_file="$current_case/brew-linked-version"
    brew_extracted_version_file="$current_case/brew-extracted-version"
    brew_installed_formula_file="$current_case/brew-installed-formula"
    output_file="$current_case/output"
    mkdir -p "$home" "$state_dir"
    : > "$call_log"
    printf '%s\n' "$old_version" > "$version_file"
    printf '%s\n' "$old_state" > "$service_state"
    printf 'dcchuck/tap\n' > "$brew_taps_file"
    printf 'car-go-clean\n' > "$brew_linked_formula"
    printf '%s\n' "$old_version" > "$brew_linked_version_file"
    : > "$brew_extracted_version_file"
    : > "$brew_installed_formula_file"
    rm -f "$executed_review"

    USER=cgc-fixture
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
    EXECUTE_FIFO=
    EXECUTE_MARKER=
    EXECUTE_SIGNAL=0
    BREW_ROLLBACK_FIXTURE=0
    BREW_TAPS_FILE=$brew_taps_file
    BREW_LINKED_FORMULA=$brew_linked_formula
    BREW_LINKED_VERSION_FILE=$brew_linked_version_file
    BREW_EXTRACTED_VERSION_FILE=$brew_extracted_version_file
    BREW_INSTALLED_FORMULA_FILE=$brew_installed_formula_file
    BREW_LINK_FAIL=0
    BREW_LINK_WRONG_VERSION=0
    ROLLBACK_EXPECTED_VERSION=$old_version
    LEGACY_EXCLUDES=0
    BREW_INSTALLED=1
    BREW_UPDATE_FAIL=0
    BREW_REPLACE_FAIL=0
    SHELL_DOWNLOAD_FAIL=0
    SHELL_REPLACE_FAIL=0
    WRONG_NEW_VERSION=0
    GNU_STAT_FIXTURE=0
    RESTORE_FAIL=0
    RESTORE_SIGNAL=0
    RESTORE_SIGNAL_MARKER=$restore_signal_marker
    export USER TEST_PLATFORM VERSION_FILE SERVICE_STATE CALL_LOG EXECUTED_REVIEW
    export CAR_GO_CLEAN_UPGRADE_STATE_DIR REVIEW_ID PREVIEW_EXIT PREVIEW_TEXT
    export CONFIG_EXIT EXECUTE_EXIT LEGACY_EXCLUDES BREW_INSTALLED
    export EXECUTE_ERROR RESTORE_FAIL
    export EXECUTE_FIFO EXECUTE_MARKER EXECUTE_SIGNAL
    export BREW_ROLLBACK_FIXTURE BREW_TAPS_FILE BREW_LINKED_FORMULA
    export BREW_LINKED_VERSION_FILE BREW_EXTRACTED_VERSION_FILE
    export BREW_INSTALLED_FORMULA_FILE BREW_LINK_FAIL BREW_LINK_WRONG_VERSION
    export ROLLBACK_EXPECTED_VERSION
    export RESTORE_SIGNAL RESTORE_SIGNAL_MARKER
    export BREW_UPDATE_FAIL BREW_REPLACE_FAIL SHELL_DOWNLOAD_FAIL
    export SHELL_REPLACE_FAIL WRONG_NEW_VERSION
    export GNU_STAT_FIXTURE
}

session_value() {
    field=$1
    awk -F= -v field="$field" '$1 == field { print substr($0, length(field) + 2) }' \
        "$state_dir/upgrade-session"
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

capture_homebrew_rollback() {
    rollback_script="$current_case/homebrew-rollback.sh"
    sed -n \
        '/^# BEGIN car-go-clean exact Homebrew rollback$/,/^# END car-go-clean exact Homebrew rollback$/p' \
        "$output_file" > "$rollback_script"
    test "$(grep -c '^# BEGIN car-go-clean exact Homebrew rollback$' "$rollback_script")" -eq 1
    test "$(grep -c '^# END car-go-clean exact Homebrew rollback$' "$rollback_script")" -eq 1
}

run_captured_homebrew_rollback() {
    BREW_ROLLBACK_FIXTURE=1
    export BREW_ROLLBACK_FIXTURE
    rollback_output="$current_case/homebrew-rollback.out"
    if PATH="$fake_bin:/usr/bin:/bin" HOME="$home" USER="$USER" \
        sh "$rollback_script" > "$rollback_output" 2>&1; then
        rollback_status=0
    else
        rollback_status=$?
    fi
}

complete_upgrade() {
    run_upgrade --version 0.4.0 --method "$method"
    assert_status 0
    assert_session_mode_600
    test "$(cat "$service_state")" = stopped || test "$old_state" != running
    case "$old_state" in
        running)
            assert_output_has "originally active service remains stopped"
            ;;
        stopped)
            assert_output_has "originally stopped service remains stopped"
            assert_calls_lack "launchctl bootout"
            assert_calls_lack "systemctl --user stop car-go-clean.service"
            ;;
        "not installed")
            assert_output_has "No service was installed or started"
            assert_calls_lack "launchctl bootout"
            assert_calls_lack "systemctl --user stop car-go-clean.service"
            ;;
    esac
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

# Reviewed exit 2 is a completed execution outcome for every original state.
for state in running stopped 'not installed'
do
    case "$state" in
        running) name=execute-two-active ;;
        stopped) name=execute-two-stopped ;;
        "not installed") name=execute-two-absent ;;
    esac
    new_case "$name" Linux 0.3.0 "$state" homebrew
    run_upgrade --version 0.4.0 --method homebrew
    assert_status 0
    EXECUTE_EXIT=2
    export EXECUTE_EXIT
    : > "$call_log"
    run_upgrade --version 0.4.0 --method homebrew --execute-review 42
    assert_status 0
    test ! -e "$state_dir/upgrade-session"
    case "$state" in
        running)
            test "$(cat "$service_state")" = running
            assert_calls_have "systemctl --user start car-go-clean.service"
            ;;
        stopped|"not installed")
            test "$(cat "$service_state")" = "$state"
            assert_calls_lack "systemctl --user start car-go-clean.service"
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
test -f "$state_dir/upgrade-session"
test "$(session_value phase)" = preview_pending

# Exact replacement persists a resumable preview session before config/preview.
new_case resumable-config-failure Darwin 0.2.0 running homebrew
CONFIG_EXIT=1
export CONFIG_EXIT
run_upgrade --version 0.4.0 --method homebrew
test "$run_status" -ne 0
test -f "$state_dir/upgrade-session"
test "$(session_value phase)" = preview_pending
test "$(session_value old_version)" = 0.2.0
test "$(session_value review_id)" = none
test "$(cat "$service_state")" = stopped
assert_output_has "$upgrade --version 0.4.0 --method homebrew"
assert_output_has "brew extract --force --version=0.2.0"
assert_output_has "car-go-clean service start"

CONFIG_EXIT=0
export CONFIG_EXIT
: > "$call_log"
run_upgrade --version 0.4.0 --method homebrew
assert_status 0
test "$(session_value phase)" = review_pending
test "$(session_value review_id)" = 42
assert_calls_lack "brew "
assert_calls_lack "service status"
test "$(cat "$service_state")" = stopped

new_case stopped-preview-failure Linux 0.3.0 stopped shell
PREVIEW_EXIT=1
export PREVIEW_EXIT
run_upgrade --version 0.4.0 --method shell
test "$run_status" -ne 0
test "$(session_value phase)" = preview_pending
assert_output_has "releases/download/v0.3.0/car-go-clean-installer.sh"
if grep -F "car-go-clean service start" "$output_file" >/dev/null; then
    echo "stopped service received start guidance" >&2
    exit 1
fi
PREVIEW_EXIT=0
export PREVIEW_EXIT
: > "$call_log"
run_upgrade --version 0.4.0 --method shell
assert_status 0
test "$(session_value phase)" = review_pending
assert_calls_lack "curl "

# Printed Homebrew rollback blocks are executable, exact, and state preserving.
for fixture in \
    rollback-v02-active:0.2.0:running:create \
    rollback-v03-active:0.3.0:running:reuse \
    rollback-v02-stopped:0.2.0:stopped:reuse \
    rollback-v03-absent:0.3.0:'not installed':create
do
    old_ifs=$IFS
    IFS=:
    set -- $fixture
    IFS=$old_ifs
    new_case "$1" Darwin "$2" "$3" homebrew
    tap_mode=$4
    if [ "$tap_mode" = reuse ]; then
        printf '%s/car-go-clean-rollback\n' "$USER" >> "$brew_taps_file"
    fi
    CONFIG_EXIT=1
    export CONFIG_EXIT
    run_upgrade --version 0.4.0 --method homebrew
    test "$run_status" -ne 0
    capture_homebrew_rollback

    : > "$call_log"
    run_captured_homebrew_rollback
    test "$rollback_status" -eq 0
    test "$(cat "$brew_linked_version_file")" = "$2"
    test "$(cat "$brew_linked_formula")" = \
        "$USER/car-go-clean-rollback/car-go-clean@$2"
    resolved_version=$(PATH="$fake_bin:/usr/bin:/bin" car-go-clean version)
    test "$resolved_version" = "$2"
    assert_calls_have "brew extract --force --version=$2 dcchuck/tap/car-go-clean $USER/car-go-clean-rollback"
    assert_calls_have "brew unlink car-go-clean"
    assert_calls_have "brew install $USER/car-go-clean-rollback/car-go-clean@$2"
    assert_calls_have "brew link --force --overwrite $USER/car-go-clean-rollback/car-go-clean@$2"
    case "$tap_mode" in
        create) assert_calls_have "brew tap-new $USER/car-go-clean-rollback" ;;
        reuse) assert_calls_lack "brew tap-new $USER/car-go-clean-rollback" ;;
    esac
    case "$3" in
        running)
            test "$(cat "$service_state")" = running
            awk '
                /^car-go-clean version$/ { validated = NR }
                /^car-go-clean service start$/ {
                    if (!validated || validated >= NR) exit 1
                    started = 1
                }
                END { if (!started) exit 1 }
            ' "$call_log"
            ;;
        stopped|"not installed")
            test "$(cat "$service_state")" = "$3"
            assert_calls_have "car-go-clean version"
            assert_calls_lack "car-go-clean service start"
            ;;
    esac
done

# Link and exact-version failures short-circuit before active-service restart.
for failure in link version
do
    case "$failure" in
        link) old_version=0.2.0 ;;
        version) old_version=0.3.0 ;;
    esac
    new_case "rollback-$failure-failure" Darwin "$old_version" running homebrew
    CONFIG_EXIT=1
    export CONFIG_EXIT
    run_upgrade --version 0.4.0 --method homebrew
    test "$run_status" -ne 0
    capture_homebrew_rollback
    case "$failure" in
        link) BREW_LINK_FAIL=1 ;;
        version) BREW_LINK_WRONG_VERSION=1 ;;
    esac
    export BREW_LINK_FAIL BREW_LINK_WRONG_VERSION
    : > "$call_log"
    run_captured_homebrew_rollback
    test "$rollback_status" -ne 0
    test "$(cat "$service_state")" = stopped
    assert_calls_lack "car-go-clean service start"
    case "$failure" in
        link) assert_calls_lack "car-go-clean version" ;;
        version) assert_calls_have "car-go-clean version" ;;
    esac
done

new_case preview-failure Linux 0.3.0 running homebrew
PREVIEW_EXIT=1
export PREVIEW_EXIT
run_upgrade --version 0.4.0 --method homebrew
test "$run_status" -ne 0
test "$(cat "$service_state")" = stopped
assert_output_has "preview failed"
assert_output_has "service remains stopped"
test -f "$state_dir/upgrade-session"
test "$(session_value phase)" = preview_pending

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
    test -f "$state_dir/upgrade-session"
    test "$(session_value phase)" = preview_pending
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
test "$(session_value phase)" = executing
assert_output_has "service remains stopped"
assert_output_has "will not run review 42 again"
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
test "$(session_value phase)" = executed
assert_output_has "service remains stopped"

RESTORE_FAIL=0
export RESTORE_FAIL
: > "$call_log"
run_upgrade --version 0.4.0 --method homebrew --execute-review 42
assert_status 0
assert_calls_lack "run --review"
assert_calls_have "launchctl bootstrap"
test ! -e "$state_dir/upgrade-session"

# A signal after reviewed execution leaves `executing`; resume never reruns it.
new_case execute-signal Linux 0.3.0 running homebrew
run_upgrade --version 0.4.0 --method homebrew
assert_status 0
EXECUTE_SIGNAL=1
export EXECUTE_SIGNAL
: > "$call_log"
run_upgrade --version 0.4.0 --method homebrew --execute-review 42
test "$run_status" -ne 0
test "$(session_value phase)" = executing
test "$(cat "$service_state")" = stopped
EXECUTE_SIGNAL=0
export EXECUTE_SIGNAL
run_upgrade --version 0.4.0 --method homebrew --execute-review 42
test "$run_status" -ne 0
test "$(grep -c '^car-go-clean run --review 42$' "$call_log")" -eq 1
test "$(cat "$service_state")" = stopped
test "$(session_value phase)" = executing
assert_output_has "will not run review 42 again"

# A signal after native restoration is finalized without cleanup or a second start.
new_case restored-before-signal Darwin 0.2.0 running homebrew
run_upgrade --version 0.4.0 --method homebrew
assert_status 0
RESTORE_SIGNAL=1
export RESTORE_SIGNAL
: > "$call_log"
run_upgrade --version 0.4.0 --method homebrew --execute-review 42
test "$run_status" -ne 0
test "$(session_value phase)" = executed
test "$(cat "$service_state")" = running
RESTORE_SIGNAL=0
export RESTORE_SIGNAL
run_upgrade --version 0.4.0 --method homebrew --execute-review 42
assert_status 0
test "$(grep -c '^car-go-clean run --review 42$' "$call_log")" -eq 1
test "$(grep -c '^launchctl bootstrap ' "$call_log")" -eq 1
test ! -e "$state_dir/upgrade-session"

# An exclusive claim rejects a concurrent resume without a second reviewed run.
new_case concurrent-resume Linux 0.3.0 stopped homebrew
run_upgrade --version 0.4.0 --method homebrew
assert_status 0
execute_fifo="$current_case/execute.fifo"
mkfifo "$execute_fifo"
EXECUTE_FIFO=$execute_fifo
EXECUTE_MARKER=$execute_marker
export EXECUTE_FIFO EXECUTE_MARKER
: > "$call_log"
PATH="$fake_bin:/usr/bin:/bin" HOME="$home" \
    "$upgrade" --version 0.4.0 --method homebrew --execute-review 42 \
    > "$current_case/first-resume.out" 2>&1 &
first_resume_pid=$!
attempts=0
while [ ! -e "$execute_marker" ] && kill -0 "$first_resume_pid" 2>/dev/null; do
    attempts=$((attempts + 1))
    if [ "$attempts" -ge 100 ]; then
        kill "$first_resume_pid" 2>/dev/null || :
        wait "$first_resume_pid" 2>/dev/null || :
        cat "$current_case/first-resume.out" >&2
        echo "background resume did not reach reviewed execution" >&2
        exit 1
    fi
    sleep 0.05
done
if [ ! -e "$execute_marker" ]; then
    wait "$first_resume_pid" 2>/dev/null || :
    cat "$current_case/first-resume.out" >&2
    echo "background resume exited before reviewed execution" >&2
    exit 1
fi
EXECUTE_FIFO=
EXECUTE_MARKER=
export EXECUTE_FIFO EXECUTE_MARKER
run_upgrade --version 0.4.0 --method homebrew --execute-review 42
second_resume_status=$run_status
printf 'release\n' > "$execute_fifo"
if wait "$first_resume_pid"; then
    first_resume_status=0
else
    first_resume_status=$?
fi
test "$first_resume_status" -eq 0
test "$second_resume_status" -ne 0
assert_output_has "already in progress"
test "$(grep -c '^car-go-clean run --review 42$' "$call_log")" -eq 1
test ! -e "$state_dir/upgrade-session"

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

printf 'format=2\nversion=0.4.0\nmethod=homebrew\nold_version=0.3.0\nservice_state=stopped\nphase=review_pending\nreview_id=42\n' \
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
