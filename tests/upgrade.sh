#!/bin/sh
set -eu

root=$(CDPATH='' cd -- "$(dirname -- "$0")/.." && pwd)
upgrade="$root/packaging/release/car-go-clean-upgrade.sh"
work_dir=$(mktemp -d)
work_dir=$(CDPATH='' cd -P "$work_dir" && pwd -P)
fake_bin="$work_dir/bin"
case_root="$work_dir/cases"
car_go_clean_fixture="$work_dir/car-go-clean-fixture"

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

cat > "$car_go_clean_fixture" <<'EOF'
#!/bin/sh
set -eu
printf 'car-go-clean %s\n' "$*" >> "$CALL_LOG"
printf '%s\n' "$0" >> "$BINARY_PATH_LOG"
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
    "service refresh")
        test "$version" = 0.4.0
        test "$(cat "$SERVICE_STATE")" != "not installed"
        test ! -e "$SERVICE_ENABLED"
        test "$(cat "$SERVICE_STATE")" = stopped
        case "$TEST_PLATFORM" in
            Darwin)
                {
                    printf '%s\n' \
                        '<?xml version="1.0" encoding="UTF-8"?>' \
                        '<plist version="1.0">' \
                        '<dict>' \
                        '<key>ProgramArguments</key>' \
                        '<array>' \
                        "<string>$0</string>" \
                        '<string>daemon</string>' \
                        '</array>' \
                        '<!-- # car-go-clean-service-environment-v1 -->' \
                        '<key>EnvironmentVariables</key>' \
                        '<dict>' \
                        '<key>CARGO_HOME</key>' \
                        "<string>${CARGO_HOME-}</string>" \
                        '<key>COLIMA_HOME</key>' \
                        "<string>${COLIMA_HOME-}</string>" \
                        '</dict>' \
                        "<!-- CARGO_HOME=${CARGO_HOME-} -->" \
                        '</dict>' \
                        '</plist>'
                } > "$SERVICE_DEFINITION"
                ;;
            Linux)
                {
                    printf '%s\n' \
                        '[Service]' \
                        "ExecStart=\"$0\" daemon" \
                        '# car-go-clean-service-environment-v1' \
                        "Environment=\"CARGO_HOME=${CARGO_HOME-}\"" \
                        "Environment=\"COLIMA_HOME=${COLIMA_HOME-}\""
                } > "$SERVICE_DEFINITION"
                ;;
        esac
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
        if [ "${EXECUTE_MUTATE_BINARY-0}" = 1 ]; then
            printf '%s\n' '# changed during reviewed execution' >> "$0"
        fi
        if [ "${EXECUTE_MUTATE_DEFINITION-0}" = 1 ]; then
            printf '%s\n' '# changed during reviewed execution' >> "$SERVICE_DEFINITION"
        fi
        if [ "${4-}" = --json ]; then
            if [ "${EXECUTE_TARGET_EVENT-0}" = 1 ]; then
                printf '%s\n' \
                    '{"format_version":1,"event":"target","data":{"project":"/tmp/project","target":"/tmp/project/target"}}'
            fi
            case "${EXECUTE_REJECTION-}" in
                missing)
                    printf '{"format_version":1,"command":"run","outcome":{"code":1,"kind":"failed","reasons":["review_plan_missing"]},"policy_hash":null,"generation":null,"review_id":%s,"scan_errors":[],"data":{"review_plan_rejection":{"kind":"missing"}}}\n' "$review"
                    exit 1
                    ;;
                expired)
                    printf '{"format_version":1,"command":"run","outcome":{"code":1,"kind":"failed","reasons":["review_plan_expired"]},"policy_hash":null,"generation":null,"review_id":%s,"scan_errors":[],"data":{"review_plan_rejection":{"kind":"expired"}}}\n' "$review"
                    exit 1
                    ;;
                policy)
                    printf '{"format_version":1,"command":"run","outcome":{"code":1,"kind":"failed","reasons":["review_policy_mismatch"]},"policy_hash":null,"generation":null,"review_id":%s,"scan_errors":[],"data":{"review_plan_rejection":{"kind":"policy_mismatch"}}}\n' "$review"
                    exit 1
                    ;;
                generation)
                    printf '{"format_version":1,"command":"run","outcome":{"code":1,"kind":"failed","reasons":["review_generation_mismatch"]},"policy_hash":null,"generation":null,"review_id":%s,"scan_errors":[],"data":{"review_plan_rejection":{"kind":"generation_mismatch","replacing_generation":43}}}\n' "$review"
                    exit 1
                    ;;
                malformed)
                    printf '%s\n' '{"format_version":1,"command":"run"'
                    exit 1
                    ;;
                missing-terminal)
                    exit 1
                    ;;
                unknown)
                    printf '{"format_version":1,"command":"run","outcome":{"code":1,"kind":"failed","reasons":["command_failed"]},"policy_hash":null,"generation":null,"review_id":%s,"scan_errors":[],"data":null}\n' "$review"
                    exit 1
                    ;;
            esac
            case "${EXECUTE_EXIT-0}" in
                0)
                    printf '{"format_version":1,"command":"run","outcome":{"code":0,"kind":"complete","reasons":[]},"policy_hash":"fixture","generation":1,"review_id":%s,"scan_errors":[],"data":{"run_id":1,"cleaned":1,"skipped":0,"bytes_recovered":1,"errors":0,"cargo_failures":0,"measurement_failures":0,"cleanup_failures":0,"coverage_incomplete":false}}\n' "$review"
                    ;;
                2)
                    printf '{"format_version":1,"command":"run","outcome":{"code":2,"kind":"incomplete","reasons":["scan_incomplete"]},"policy_hash":"fixture","generation":1,"review_id":%s,"scan_errors":[],"data":{"run_id":1,"cleaned":1,"skipped":0,"bytes_recovered":1,"errors":0,"cargo_failures":0,"measurement_failures":0,"cleanup_failures":0,"coverage_incomplete":true}}\n' "$review"
                    ;;
            esac
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
    disable)
        rm -f "$SERVICE_ENABLED"
        ;;
    enable)
        : > "$SERVICE_ENABLED"
        if [ "${DEFINITION_ENABLE_SWAP-0}" = 1 ] &&
            [ ! -e "$DEFINITION_ENABLE_SWAP_MARKER" ]; then
            : > "$DEFINITION_ENABLE_SWAP_MARKER"
            rm -f "$VISIBLE_CGC_PATH"
            ln -s "$NEW_FORMULA_BINARY" "$VISIBLE_CGC_PATH"
        fi
        ;;
    bootout)
        printf 'stopped\n' > "$SERVICE_STATE"
        ;;
    bootstrap)
        test "${RESTORE_FAIL-0}" != 1
        test -e "$SERVICE_ENABLED"
        printf 'running\n' > "$SERVICE_STATE"
        ;;
    kickstart)
        test "${RESTORE_FAIL-0}" != 1
        test -e "$SERVICE_ENABLED"
        printf 'running\n' > "$SERVICE_STATE"
        if [ "${RESTORE_SIGNAL-0}" = 1 ] &&
            [ ! -e "$RESTORE_SIGNAL_MARKER" ]; then
            : > "$RESTORE_SIGNAL_MARKER"
            kill -TERM "$PPID"
        fi
        ;;
    print)
        if [ "${DEFINITION_PREFLIGHT_SWAP-0}" = 1 ] &&
            [ ! -e "$DEFINITION_PREFLIGHT_SWAP_MARKER" ]; then
            : > "$DEFINITION_PREFLIGHT_SWAP_MARKER"
            rm -f "$VISIBLE_CGC_PATH"
            ln -s "$NEW_FORMULA_BINARY" "$VISIBLE_CGC_PATH"
        fi
        if [ "${MANAGER_ACTIVITY_QUERY_ERROR-0}" = 1 ]; then
            echo "launchctl query transport failed" >&2
            exit 71
        fi
        if [ -n "${MANAGER_ACTIVITY_STATUS-}" ]; then
            printf '%s\n' "$MANAGER_ACTIVITY_OUTPUT" >&2
            exit "$MANAGER_ACTIVITY_STATUS"
        fi
        if [ "$(cat "$SERVICE_STATE")" = running ]; then
            exit 0
        fi
        echo "Could not find specified service" >&2
        exit 113
        ;;
    print-disabled)
        test "$#" -eq 2
        if [ -e "$SERVICE_ENABLED" ]; then
            printf 'disabled services = {\n    "com.dcchuck.car-go-clean" => false\n}\n'
        else
            printf 'disabled services = {\n    "com.dcchuck.car-go-clean" => true\n}\n'
        fi
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
    "--user disable --now car-go-clean.service")
        rm -f "$SERVICE_ENABLED"
        printf 'stopped\n' > "$SERVICE_STATE"
        ;;
    "--user enable --now car-go-clean.service")
        test "${RESTORE_FAIL-0}" != 1
        : > "$SERVICE_ENABLED"
        printf 'running\n' > "$SERVICE_STATE"
        ;;
    "--user enable car-go-clean.service")
        test "${RESTORE_FAIL-0}" != 1
        : > "$SERVICE_ENABLED"
        if [ "${DEFINITION_ENABLE_SWAP-0}" = 1 ] &&
            [ ! -e "$DEFINITION_ENABLE_SWAP_MARKER" ]; then
            : > "$DEFINITION_ENABLE_SWAP_MARKER"
            rm -f "$VISIBLE_CGC_PATH"
            ln -s "$NEW_FORMULA_BINARY" "$VISIBLE_CGC_PATH"
        fi
        ;;
    "--user disable car-go-clean.service")
        rm -f "$SERVICE_ENABLED"
        ;;
    "--user start car-go-clean.service")
        test "${RESTORE_FAIL-0}" != 1
        printf 'running\n' > "$SERVICE_STATE"
        ;;
    "--user stop car-go-clean.service")
        printf 'stopped\n' > "$SERVICE_STATE"
        ;;
    "--user is-enabled car-go-clean.service")
        if [ -e "$SERVICE_ENABLED" ]; then
            printf 'enabled\n'
        else
            printf 'disabled\n'
            exit 1
        fi
        ;;
    "--user daemon-reload")
        ;;
    "--user is-active car-go-clean.service"|"--user is-active --quiet car-go-clean.service")
        if [ "${DEFINITION_PREFLIGHT_SWAP-0}" = 1 ] &&
            [ ! -e "$DEFINITION_PREFLIGHT_SWAP_MARKER" ]; then
            : > "$DEFINITION_PREFLIGHT_SWAP_MARKER"
            rm -f "$VISIBLE_CGC_PATH"
            ln -s "$NEW_FORMULA_BINARY" "$VISIBLE_CGC_PATH"
        fi
        if [ "${MANAGER_ACTIVITY_QUERY_ERROR-0}" = 1 ]; then
            echo "systemctl query transport failed" >&2
            exit 71
        fi
        if [ -n "${MANAGER_ACTIVITY_STATUS-}" ]; then
            case "$*" in
                *" --quiet "*) ;;
                *) printf '%s\n' "$MANAGER_ACTIVITY_OUTPUT" ;;
            esac
            exit "$MANAGER_ACTIVITY_STATUS"
        fi
        if [ "$(cat "$SERVICE_STATE")" = running ]; then
            case "$*" in
                *" --quiet "*) ;;
                *) printf 'active\n' ;;
            esac
            exit 0
        fi
        case "$*" in
            *" --quiet "*) ;;
            *) printf 'inactive\n' ;;
        esac
        exit 3
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
    --prefix)
        test "$#" -eq 2
        case "$2" in
            car-go-clean)
                test "${BREW_INSTALLED-1}" = 1
                if [ "${BREW_RESOLVE_FAIL_AFTER_SUCCESS-0}" = 1 ] &&
                    [ "$(cat "$VERSION_FILE")" = 0.4.0 ]; then
                    exit 73
                fi
                printf '%s\n' "$BREW_PREFIX"
                ;;
            "$USER"/car-go-clean-rollback/car-go-clean@*)
                test "${BREW_ROLLBACK_FIXTURE-0}" = 1
                test "$2" = "$(cat "$BREW_INSTALLED_FORMULA_FILE")"
                printf '%s\n' "$BREW_ROLLBACK_PREFIX"
                ;;
            *)
                exit 64
                ;;
        esac
        ;;
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
        rm -f "$VISIBLE_CGC_PATH"
        ln -s "$BREW_ROLLBACK_PREFIX/bin/car-go-clean" "$VISIBLE_CGC_PATH"
        ;;
    update)
        test "${BREW_UPDATE_FAIL-0}" != 1
        ;;
    list)
        test "${BREW_INSTALLED-1}" = 1
        ;;
    upgrade)
        case "${BREW_RECOVERY_DEFINITION_FAILURE-}" in
            moved-executable)
                rm -f "$VISIBLE_CGC_PATH"
                ln -s "$NEW_FORMULA_BINARY" "$VISIBLE_CGC_PATH"
                exit 74
                ;;
            missing-executable)
                rm -f "$VISIBLE_CGC_PATH"
                exit 74
                ;;
            missing-definition)
                rm -f "$SERVICE_DEFINITION"
                exit 74
                ;;
            relative-definition)
                case "$TEST_PLATFORM" in
                    Darwin)
                        printf '%s\n' \
                            '<key>ProgramArguments</key>' \
                            '<array>' \
                            '<string>relative/car-go-clean</string>' \
                            '<string>daemon</string>' \
                            '</array>' > "$SERVICE_DEFINITION"
                        ;;
                    Linux)
                        printf '%s\n' \
                            '[Service]' \
                            'ExecStart="relative/car-go-clean" daemon' \
                            > "$SERVICE_DEFINITION"
                        ;;
                esac
                exit 74
                ;;
            unparseable-definition)
                printf '%s\n' '# malformed service definition' \
                    > "$SERVICE_DEFINITION"
                exit 74
                ;;
        esac
        if [ "${BREW_PARTIAL_REPLACE_FAIL-0}" = 1 ]; then
            printf '0.4.0\n' > "$VERSION_FILE"
            printf '0.4.0\n' > "$BREW_LINKED_VERSION_FILE"
            printf 'car-go-clean\n' > "$BREW_LINKED_FORMULA"
            exit 74
        fi
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
                if [ "${BREW_PARTIAL_REPLACE_FAIL-0}" = 1 ]; then
                    printf '0.4.0\n' > "$VERSION_FILE"
                    printf '0.4.0\n' > "$BREW_LINKED_VERSION_FILE"
                    printf 'car-go-clean\n' > "$BREW_LINKED_FORMULA"
                    exit 74
                fi
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
install_dir=
requested_version=
while [ "$#" -gt 0 ]; do
    case "$1" in
        --version)
            requested_version=$2
            shift 2
            ;;
        --install-dir)
            install_dir=$2
            shift 2
            ;;
        *)
            shift
            ;;
    esac
done
test -n "$requested_version"
if [ "${SHELL_PARTIAL_REPLACE_FAIL-0}" = 1 ]; then
    printf '0.4.0\n' > "$VERSION_FILE"
    exit 75
fi
test "${SHELL_REPLACE_FAIL-0}" != 1
if [ "${WRONG_NEW_VERSION-0}" = 1 ] && [ "$requested_version" = 0.4.0 ]; then
    printf '0.4.1\n' > "$VERSION_FILE"
else
    printf '%s\n' "$requested_version" > "$VERSION_FILE"
fi
if [ "${SHELL_RESOLVE_FAIL_AFTER_SUCCESS-0}" = 1 ]; then
    test -n "$install_dir"
    chmod -x "$install_dir/car-go-clean"
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
case "$file" in
    */car-go-clean-installer.sh)
        printf 'fixture-sha256  %s\n' "$file"
        ;;
    *)
        if [ "${DEFINITION_AUTH_RACE-0}" = 1 ] &&
            [ "$file" = "$SERVICE_DEFINITION" ] &&
            [ ! -e "$DEFINITION_AUTH_RACE_MARKER" ]; then
            checksum=$(/usr/bin/shasum -a 256 "$file")
            : > "$DEFINITION_AUTH_RACE_MARKER"
            case "$TEST_PLATFORM" in
                Darwin)
                    {
                        printf '%s\n' \
                            '<?xml version="1.0" encoding="UTF-8"?>' \
                            '<plist version="1.0">' \
                            '<dict>' \
                            '<key>ProgramArguments</key>' \
                            '<array>' \
                            "<string>$VISIBLE_CGC_PATH</string>" \
                            '<string>daemon</string>' \
                            '</array>' \
                            '<key>EnvironmentVariables</key>' \
                            '<dict>' \
                            '<key>DYLD_INSERT_LIBRARIES</key>' \
                            '<string>/tmp/attacker.dylib</string>' \
                            '</dict>' \
                            '</dict>' \
                            '</plist>'
                    } > "$file"
                    ;;
                Linux)
                    {
                        printf '%s\n' \
                            '[Service]' \
                            "ExecStart=\"$VISIBLE_CGC_PATH\" daemon" \
                            'Environment="LD_PRELOAD=/tmp/attacker.so"'
                    } > "$file"
                    ;;
            esac
            printf '%s\n' "$checksum"
            exit 0
        fi
        exec /usr/bin/shasum -a 256 "$file"
        ;;
esac
EOF

cat > "$fake_bin/sha256sum" <<'EOF'
#!/bin/sh
set -eu
for argument do
    file=$argument
done
case "$file" in
    */car-go-clean-installer.sh)
        printf 'fixture-sha256  %s\n' "$file"
        ;;
    *)
        exec "$SHASUM_FIXTURE" -a 256 "$file"
        ;;
esac
EOF

cat > "$fake_bin/stat" <<'EOF'
#!/bin/sh
set -eu

metadata_for() {
    metadata_path=$1
    metadata=$(/usr/bin/stat -f '%u:%Lp:%d:%i:%z:%m' "$metadata_path" 2>/dev/null || :)
    case "$metadata" in
        [0-9]*:[0-7]*:[0-9]*:[0-9]*:[0-9]*:[0-9]*) ;;
        *)
            metadata=$(/usr/bin/stat -c '%u:%a:%d:%i:%s:%Y' "$metadata_path")
            ;;
    esac
    if [ -n "${STAT_OWNER_OVERRIDE_PATH-}" ] &&
        [ "$metadata_path" = "$STAT_OWNER_OVERRIDE_PATH" ]; then
        metadata=${STAT_OWNER_OVERRIDE-999}:${metadata#*:}
    fi
    printf '%s\n' "$metadata"
}

mode_for() {
    mode_path=$1
    mode=$(/usr/bin/stat -f '%Lp' "$mode_path" 2>/dev/null || :)
    case "$mode" in
        [0-7][0-7][0-7]) ;;
        *) mode=$(/usr/bin/stat -c '%a' "$mode_path") ;;
    esac
    printf '%s\n' "$mode"
}

for argument do
    stat_path=$argument
done

if [ "${GNU_STAT_FIXTURE-0}" = 1 ]; then
    case "$1" in
        -f)
            printf 'GNU filesystem status output\n'
            ;;
        -c)
            case "$2" in
                %a) mode_for "$stat_path" ;;
                '%u:%a:%d:%i:%s:%Y') metadata_for "$stat_path" ;;
                *) exit 64 ;;
            esac
            ;;
        *)
            exit 64
            ;;
    esac
elif [ "${2-}" = '%u:%Lp:%d:%i:%z:%m' ] ||
    [ "${2-}" = '%u:%a:%d:%i:%s:%Y' ]; then
    metadata_for "$stat_path"
else
    exec /usr/bin/stat "$@"
fi
EOF

cat > "$fake_bin/cp" <<'EOF'
#!/bin/sh
set -eu
/bin/cp "$@"
if [ "${BACKUP_RACE-0}" = 1 ] &&
    [ "${1-}" = "${SERVICE_DEFINITION_BACKUP-}" ]; then
    mv "$1" "$1.raced"
    printf '%s\n' '# attacker-replaced-service-definition' > "$1"
    chmod 600 "$1"
fi
EOF

chmod +x "$fake_bin"/* "$car_go_clean_fixture"

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
    service_enabled="$current_case/service-enabled"
    case "$platform" in
        Darwin)
            service_definition="$home/Library/LaunchAgents/com.dcchuck.car-go-clean.plist"
            ;;
        Linux)
            service_definition="$home/.config/systemd/user/car-go-clean.service"
            ;;
    esac
    service_definition_backup="$state_dir/upgrade-service-definition"
    executed_review="$current_case/executed-review"
    execute_marker="$current_case/execute-marker"
    restore_signal_marker="$current_case/restore-signal-marker"
    definition_auth_race_marker="$current_case/definition-auth-race-marker"
    definition_preflight_swap_marker="$current_case/definition-preflight-swap-marker"
    definition_enable_swap_marker="$current_case/definition-enable-swap-marker"
    brew_taps_file="$current_case/brew-taps"
    brew_linked_formula="$current_case/brew-linked-formula"
    brew_linked_version_file="$current_case/brew-linked-version"
    brew_extracted_version_file="$current_case/brew-extracted-version"
    brew_installed_formula_file="$current_case/brew-installed-formula"
    output_file="$current_case/output"
    binary_path_log="$current_case/binary-paths"
    brew_prefix="$current_case/brew-prefix"
    brew_rollback_prefix="$current_case/brew-rollback-prefix"
    new_formula_binary="$current_case/new-cellar/0.4.0/bin/car-go-clean"
    mkdir -p "$home" "$state_dir" "$brew_prefix/bin" \
        "$brew_rollback_prefix/bin" "$(dirname "$new_formula_binary")" \
        "$(dirname "$service_definition")"
    : > "$call_log"
    : > "$binary_path_log"
    printf '%s\n' "$old_version" > "$version_file"
    printf '%s\n' "$old_state" > "$service_state"
    rm -f "$service_enabled" "$service_definition"
    if [ "$old_state" != "not installed" ]; then
        : > "$service_enabled"
        case "$platform" in
            Darwin)
                cat > "$service_definition" <<EOF
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN"
  "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>Label</key>
  <string>com.dcchuck.car-go-clean</string>
  <key>ProgramArguments</key>
  <array>
    <string>$fake_bin/car-go-clean</string>
    <string>daemon</string>
  </array>
  <key>RunAtLoad</key>
  <true/>
  <key>KeepAlive</key>
  <true/>
  <key>StandardOutPath</key>
  <string>$home/Library/Logs/car-go-clean/car-go-clean.launchd.out.log</string>
  <key>StandardErrorPath</key>
  <string>$home/Library/Logs/car-go-clean/car-go-clean.launchd.err.log</string>
</dict>
</plist>
EOF
                ;;
            Linux)
                cat > "$service_definition" <<EOF
[Unit]
Description=Run car-go-clean daemon
Documentation=https://github.com/dcchuck/car-go-clean
After=network.target

[Service]
Type=simple
ExecStart="$fake_bin/car-go-clean" daemon
Restart=on-failure
RestartSec=30s

[Install]
WantedBy=default.target
EOF
                ;;
        esac
    fi
    printf 'dcchuck/tap\n' > "$brew_taps_file"
    printf 'car-go-clean\n' > "$brew_linked_formula"
    printf '%s\n' "$old_version" > "$brew_linked_version_file"
    : > "$brew_extracted_version_file"
    : > "$brew_installed_formula_file"
    rm -f "$executed_review"
    cp "$car_go_clean_fixture" "$brew_prefix/bin/car-go-clean"
    chmod +x "$brew_prefix/bin/car-go-clean"
    cp "$car_go_clean_fixture" "$brew_rollback_prefix/bin/car-go-clean"
    chmod +x "$brew_rollback_prefix/bin/car-go-clean"
    cat > "$new_formula_binary" <<'EOF'
#!/bin/sh
set -eu
case "$*" in
    version) printf '0.4.0\n' ;;
    *) exit 64 ;;
esac
EOF
    chmod +x "$new_formula_binary"
    rm -f "$fake_bin/car-go-clean"
    case "$method" in
        homebrew)
            ln -s "$brew_prefix/bin/car-go-clean" "$fake_bin/car-go-clean"
            ;;
        shell)
            cp "$car_go_clean_fixture" "$fake_bin/car-go-clean"
            chmod +x "$fake_bin/car-go-clean"
            ;;
    esac

    USER=cgc-fixture
    TEST_PLATFORM=$platform
    VERSION_FILE=$version_file
    SERVICE_STATE=$service_state
    SERVICE_ENABLED=$service_enabled
    SERVICE_DEFINITION=$service_definition
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
    EXECUTE_REJECTION=
    EXECUTE_TARGET_EVENT=0
    EXECUTE_MUTATE_BINARY=0
    EXECUTE_MUTATE_DEFINITION=0
    BREW_ROLLBACK_FIXTURE=0
    ROLLBACK_SHADOW_DIR=
    BREW_TAPS_FILE=$brew_taps_file
    BREW_LINKED_FORMULA=$brew_linked_formula
    BREW_LINKED_VERSION_FILE=$brew_linked_version_file
    BREW_EXTRACTED_VERSION_FILE=$brew_extracted_version_file
    BREW_INSTALLED_FORMULA_FILE=$brew_installed_formula_file
    BREW_PREFIX=$brew_prefix
    BREW_ROLLBACK_PREFIX=$brew_rollback_prefix
    VISIBLE_CGC_PATH=$fake_bin/car-go-clean
    BINARY_PATH_LOG=$binary_path_log
    BREW_LINK_FAIL=0
    BREW_LINK_WRONG_VERSION=0
    ROLLBACK_EXPECTED_VERSION=$old_version
    LEGACY_EXCLUDES=0
    BREW_INSTALLED=1
    BREW_UPDATE_FAIL=0
    BREW_REPLACE_FAIL=0
    BREW_PARTIAL_REPLACE_FAIL=0
    BREW_RECOVERY_DEFINITION_FAILURE=
    BREW_RESOLVE_FAIL_AFTER_SUCCESS=0
    SHELL_DOWNLOAD_FAIL=0
    SHELL_REPLACE_FAIL=0
    SHELL_PARTIAL_REPLACE_FAIL=0
    SHELL_RESOLVE_FAIL_AFTER_SUCCESS=0
    WRONG_NEW_VERSION=0
    GNU_STAT_FIXTURE=0
    STAT_OWNER_OVERRIDE_PATH=
    STAT_OWNER_OVERRIDE=
    RESTORE_FAIL=0
    RESTORE_SIGNAL=0
    RESTORE_SIGNAL_MARKER=$restore_signal_marker
    MANAGER_ACTIVITY_QUERY_ERROR=0
    MANAGER_ACTIVITY_OUTPUT=
    MANAGER_ACTIVITY_STATUS=
    DEFINITION_AUTH_RACE=0
    DEFINITION_AUTH_RACE_MARKER=$definition_auth_race_marker
    DEFINITION_PREFLIGHT_SWAP=0
    DEFINITION_PREFLIGHT_SWAP_MARKER=$definition_preflight_swap_marker
    DEFINITION_ENABLE_SWAP=0
    DEFINITION_ENABLE_SWAP_MARKER=$definition_enable_swap_marker
    BACKUP_RACE=0
    SERVICE_DEFINITION_BACKUP=$service_definition_backup
    NEW_FORMULA_BINARY=$new_formula_binary
    SHASUM_FIXTURE=$fake_bin/shasum
    CARGO_HOME=$current_case/manager-roots/cargo
    COLIMA_HOME=$current_case/manager-roots/colima
    mkdir -p "$CARGO_HOME" "$COLIMA_HOME"
    export USER TEST_PLATFORM VERSION_FILE SERVICE_STATE CALL_LOG EXECUTED_REVIEW
    export SERVICE_ENABLED SERVICE_DEFINITION CARGO_HOME COLIMA_HOME
    export CAR_GO_CLEAN_UPGRADE_STATE_DIR REVIEW_ID PREVIEW_EXIT PREVIEW_TEXT
    export CONFIG_EXIT EXECUTE_EXIT LEGACY_EXCLUDES BREW_INSTALLED
    export EXECUTE_ERROR RESTORE_FAIL
    export EXECUTE_FIFO EXECUTE_MARKER EXECUTE_SIGNAL
    export EXECUTE_REJECTION EXECUTE_TARGET_EVENT
    export EXECUTE_MUTATE_BINARY EXECUTE_MUTATE_DEFINITION
    export BREW_ROLLBACK_FIXTURE BREW_TAPS_FILE BREW_LINKED_FORMULA
    export ROLLBACK_SHADOW_DIR
    export BREW_LINKED_VERSION_FILE BREW_EXTRACTED_VERSION_FILE
    export BREW_INSTALLED_FORMULA_FILE BREW_LINK_FAIL BREW_LINK_WRONG_VERSION
    export BREW_PREFIX BREW_ROLLBACK_PREFIX VISIBLE_CGC_PATH BINARY_PATH_LOG
    export ROLLBACK_EXPECTED_VERSION
    export RESTORE_SIGNAL RESTORE_SIGNAL_MARKER
    export MANAGER_ACTIVITY_QUERY_ERROR
    export MANAGER_ACTIVITY_OUTPUT MANAGER_ACTIVITY_STATUS
    export DEFINITION_AUTH_RACE DEFINITION_AUTH_RACE_MARKER
    export DEFINITION_PREFLIGHT_SWAP DEFINITION_PREFLIGHT_SWAP_MARKER
    export DEFINITION_ENABLE_SWAP DEFINITION_ENABLE_SWAP_MARKER
    export BACKUP_RACE SERVICE_DEFINITION_BACKUP
    export BREW_UPDATE_FAIL BREW_REPLACE_FAIL SHELL_DOWNLOAD_FAIL
    export BREW_PARTIAL_REPLACE_FAIL BREW_RECOVERY_DEFINITION_FAILURE
    export BREW_RESOLVE_FAIL_AFTER_SUCCESS NEW_FORMULA_BINARY
    export SHASUM_FIXTURE
    export SHELL_REPLACE_FAIL SHELL_PARTIAL_REPLACE_FAIL
    export SHELL_RESOLVE_FAIL_AFTER_SUCCESS WRONG_NEW_VERSION
    export GNU_STAT_FIXTURE
    export STAT_OWNER_OVERRIDE_PATH STAT_OWNER_OVERRIDE
}

session_value() {
    field=$1
    awk -F= -v field="$field" '$1 == field { print substr($0, length(field) + 2) }' \
        "$state_dir/upgrade-session"
}

canonical_fixture_path() {
    fixture_path=$1
    fixture_parent=$(CDPATH='' cd -P "$(dirname "$fixture_path")" && pwd -P)
    printf '%s/%s\n' "$fixture_parent" "$(basename "$fixture_path")"
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
    grep -F -- "$1" "$output_file" >/dev/null || {
        echo "missing output: $1" >&2
        cat "$output_file" >&2
        exit 1
    }
}

assert_calls_have() {
    grep -F -- "$1" "$call_log" >/dev/null || {
        echo "missing call: $1" >&2
        cat "$call_log" >&2
        exit 1
    }
}

assert_calls_lack() {
    if grep -F -- "$1" "$call_log" >/dev/null; then
        echo "unexpected call: $1" >&2
        cat "$call_log" >&2
        exit 1
    fi
}

assert_historical_definition() {
    definition=$1
    case "$TEST_PLATFORM" in
        Darwin)
            grep -F '<key>ProgramArguments</key>' "$definition" >/dev/null
            grep -F "<string>$fake_bin/car-go-clean</string>" "$definition" >/dev/null
            ;;
        Linux)
            grep -F "ExecStart=\"$fake_bin/car-go-clean\" daemon" \
                "$definition" >/dev/null
            ;;
    esac
}

assert_review_call_count() {
    expected=$1
    actual=$(grep -c '^car-go-clean run --review 42 --json$' "$call_log" || :)
    test "$actual" -eq "$expected" || {
        echo "expected $expected reviewed executions, got $actual" >&2
        cat "$call_log" >&2
        exit 1
    }
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

simulate_manager_recreation() {
    if [ -e "$service_enabled" ] &&
        [ "$(cat "$service_state")" != "not installed" ]; then
        printf 'running\n' > "$service_state"
    fi
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
    rollback_path=$fake_bin:/usr/bin:/bin
    if [ -n "${ROLLBACK_SHADOW_DIR-}" ]; then
        rollback_path=$ROLLBACK_SHADOW_DIR:$rollback_path
    fi
    if PATH="$rollback_path" HOME="$home" USER="$USER" \
        sh "$rollback_script" > "$rollback_output" 2>&1; then
        rollback_status=0
    else
        rollback_status=$?
    fi
}

capture_shell_rollback() {
    rollback_script="$current_case/shell-rollback.sh"
    sed -n \
        '/^# BEGIN car-go-clean exact shell rollback$/,/^# END car-go-clean exact shell rollback$/p' \
        "$output_file" > "$rollback_script"
    test "$(grep -c '^# BEGIN car-go-clean exact shell rollback$' "$rollback_script")" -eq 1
    test "$(grep -c '^# END car-go-clean exact shell rollback$' "$rollback_script")" -eq 1
}

run_captured_shell_rollback() {
    rollback_output="$current_case/shell-rollback.out"
    if (
        CDPATH='' cd "$current_case"
        PATH="$fake_bin:/usr/bin:/bin" HOME="$home" USER="$USER" \
            sh "$rollback_script"
    ) > "$rollback_output" 2>&1; then
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
            ;;
        "not installed")
            assert_output_has "No service was installed or started"
            assert_calls_lack "launchctl bootout"
            assert_calls_lack "systemctl --user disable --now car-go-clean.service"
            ;;
    esac
    if [ "$old_state" != "not installed" ]; then
        test ! -e "$service_enabled"
        test -f "$service_definition"
        test -f "$service_definition_backup"
        case "$TEST_PLATFORM" in
            Darwin)
                grep -F '<key>ProgramArguments</key>' \
                    "$service_definition_backup" >/dev/null
                ;;
            Linux)
                grep -F "ExecStart=\"$fake_bin/car-go-clean\" daemon" \
                    "$service_definition_backup" >/dev/null
                ;;
        esac
        grep -F '# car-go-clean-service-environment-v1' "$service_definition" >/dev/null
        grep -F "CARGO_HOME=$CARGO_HOME" "$service_definition" >/dev/null
        simulate_manager_recreation
        test "$(cat "$service_state")" = stopped
    fi
    : > "$call_log"
    run_upgrade --version 0.4.0 --method "$method" --execute-review 42
    assert_status 0
    test "$(cat "$executed_review")" = 42
    test ! -e "$state_dir/upgrade-session"
    test ! -e "$service_definition_backup"
}

assert_matrix_service_stopped() {
    case "$old_state" in
        running|stopped)
            test "$(cat "$service_state")" = stopped
            if [ "$TEST_PLATFORM" = Darwin ]; then
                assert_calls_have "launchctl disable"
                if [ "$old_state" = running ]; then
                    assert_calls_have "launchctl bootout"
                else
                    assert_calls_lack "launchctl bootout"
                fi
            else
                assert_calls_have "systemctl --user disable --now car-go-clean.service"
            fi
            ;;
        "not installed")
            test "$(cat "$service_state")" = "$old_state"
            assert_calls_lack "launchctl disable"
            assert_calls_lack "systemctl --user disable --now car-go-clean.service"
            ;;
    esac
}

assert_matrix_service_retained() {
    case "$old_state" in
        running)
            test "$(cat "$service_state")" = stopped
            assert_calls_lack "launchctl bootstrap"
            assert_calls_lack "systemctl --user enable --now car-go-clean.service"
            ;;
        stopped|"not installed")
            test "$(cat "$service_state")" = "$old_state"
            assert_calls_lack "launchctl bootstrap"
            assert_calls_lack "systemctl --user enable --now car-go-clean.service"
            ;;
    esac
}

assert_matrix_service_restored() {
    case "$old_state" in
        running)
            test "$(cat "$service_state")" = running
            if [ "$TEST_PLATFORM" = Darwin ]; then
                assert_calls_have "launchctl bootstrap"
            else
                assert_calls_have "systemctl --user enable --now car-go-clean.service"
            fi
            ;;
        stopped|"not installed")
            test "$(cat "$service_state")" = "$old_state"
            assert_calls_lack "launchctl bootstrap"
            assert_calls_lack "systemctl --user enable --now car-go-clean.service"
            ;;
    esac
}

run_upgrade_outcome_matrix_cell() {
    platform=$1
    old_version=$2
    old_state=$3
    preview_outcome=$4
    execute_outcome=$5
    case "$platform" in
        Darwin) method=homebrew ;;
        Linux) method=shell ;;
    esac
    new_case "matrix-${platform}-${old_version}-${old_state}-${preview_outcome}-${execute_outcome}" \
        "$platform" "$old_version" "$old_state" "$method"
    PREVIEW_EXIT=$preview_outcome
    EXECUTE_EXIT=$execute_outcome
    export PREVIEW_EXIT EXECUTE_EXIT

    run_upgrade --version 0.4.0 --method "$method"
    case "$preview_outcome" in
        0|2)
            assert_status 0
            assert_session_mode_600
            test "$(session_value phase)" = review_pending
            test "$(session_value review_id)" = 42
            assert_matrix_service_stopped
            if [ "$old_state" != "not installed" ]; then
                test ! -e "$service_enabled"
                test -f "$service_definition"
                simulate_manager_recreation
                test "$(cat "$service_state")" = stopped
            fi
            : > "$call_log"
            run_upgrade --version 0.4.0 --method "$method" --execute-review 42
            assert_review_call_count 1
            case "$execute_outcome" in
                0|2)
                    assert_status 0
                    test ! -e "$state_dir/upgrade-session"
                    assert_matrix_service_restored
                    ;;
                1)
                    test "$run_status" -ne 0
                    test "$(session_value phase)" = executing
                    test "$(session_value review_id)" = 42
                    assert_matrix_service_retained
                    assert_calls_have "car-go-clean run --review 42"
                    ;;
            esac
            ;;
        1)
            test "$run_status" -ne 0
            test "$(session_value phase)" = preview_pending
            test "$(session_value review_id)" = none
            assert_matrix_service_stopped
            assert_calls_lack "car-go-clean run --review"
            assert_review_call_count 0
            ;;
    esac
}

# The upgrade state boundary rejects symlink traversal and directories that are
# not exclusively controlled by the current user before touching the service.
new_case state-symlink-ancestor Darwin 0.2.0 running homebrew
mkdir -p "$current_case/real-state-parent"
ln -s "$current_case/real-state-parent" "$current_case/linked-state-parent"
state_dir=$current_case/linked-state-parent/state
service_definition_backup=$state_dir/upgrade-service-definition
CAR_GO_CLEAN_UPGRADE_STATE_DIR=$state_dir
export CAR_GO_CLEAN_UPGRADE_STATE_DIR
run_upgrade --version 0.4.0 --method homebrew
test "$run_status" -ne 0
assert_output_has "symlink"
test "$(cat "$service_state")" = running
test -e "$service_enabled"
assert_calls_lack "launchctl bootout"

new_case state-final-symlink Linux 0.3.0 running shell
rmdir "$state_dir"
mkdir -p "$current_case/real-state"
ln -s "$current_case/real-state" "$state_dir"
run_upgrade --version 0.4.0 --method shell
test "$run_status" -ne 0
assert_output_has "symlink"
test "$(cat "$service_state")" = running
test -e "$service_enabled"
assert_calls_lack "systemctl --user disable --now car-go-clean.service"

new_case state-group-writable Darwin 0.2.0 running homebrew
chmod 0770 "$state_dir"
run_upgrade --version 0.4.0 --method homebrew
test "$run_status" -ne 0
assert_output_has "group/world-writable"
test "$(cat "$service_state")" = running
test -e "$service_enabled"
assert_calls_lack "launchctl bootout"

new_case state-wrong-owner Linux 0.3.0 running shell
STAT_OWNER_OVERRIDE_PATH=$state_dir
STAT_OWNER_OVERRIDE=777
export STAT_OWNER_OVERRIDE_PATH STAT_OWNER_OVERRIDE
run_upgrade --version 0.4.0 --method shell
test "$run_status" -ne 0
assert_output_has "owned by the current user"
test "$(cat "$service_state")" = running
test -e "$service_enabled"
assert_calls_lack "systemctl --user disable --now car-go-clean.service"

# Every supported platform/version/original-service-state combination exercises
# successful/no-work previews against every execute result. A failed preview has
# one behaviorally distinct cell because reviewed execution is never attempted.
matrix_cells=0
for matrix_platform in Darwin Linux
do
    for matrix_version in 0.2.0 0.3.0
    do
        for matrix_state in running stopped 'not installed'
        do
            for matrix_preview in 0 2 1
            do
                case "$matrix_preview" in
                    0|2) matrix_execute_outcomes='0 2 1' ;;
                    1) matrix_execute_outcomes=0 ;;
                esac
                for matrix_execute in $matrix_execute_outcomes
                do
                    run_upgrade_outcome_matrix_cell "$matrix_platform" "$matrix_version" \
                        "$matrix_state" "$matrix_preview" "$matrix_execute"
                    matrix_cells=$((matrix_cells + 1))
                done
            done
        done
    done
done

# Coverage gate: 2 platforms × 2 versions × 3 service states ×
# ((2 successful preview outcomes × 3 execute outcomes) + 1 failed preview).
test "$matrix_cells" -eq 84

# Upgrade method follows the owner of the visible command and rejects ambiguity
# before stopping a service or replacing a binary.
new_case shell-shadows-brew Darwin 0.2.0 running shell
run_upgrade --version 0.4.0 --method homebrew
test "$run_status" -ne 0
assert_output_has "visible car-go-clean"
assert_output_has "--method shell"
assert_calls_lack "launchctl bootout"
assert_calls_lack "brew update"
assert_calls_lack "installer "

new_case brew-visible-with-shell-request Darwin 0.3.0 running homebrew
run_upgrade --version 0.4.0 --method shell
test "$run_status" -ne 0
assert_output_has "Homebrew"
assert_output_has "--method homebrew"
assert_calls_lack "launchctl bootout"
assert_calls_lack "brew update"
assert_calls_lack "installer "

new_case homebrew-method-without-formula Linux 0.2.0 running shell
BREW_INSTALLED=0
export BREW_INSTALLED
run_upgrade --version 0.4.0 --method homebrew
test "$run_status" -ne 0
assert_output_has "not owned by Homebrew"
assert_calls_lack "systemctl --user stop"
assert_calls_lack "brew update"

new_case ambiguous-shell-symlink Linux 0.3.0 running shell
mv "$fake_bin/car-go-clean" "$current_case/real-shell-binary"
ln -s "$current_case/real-shell-binary" "$fake_bin/car-go-clean"
run_upgrade --version 0.4.0 --method shell
test "$run_status" -ne 0
assert_output_has "symlink"
assert_calls_lack "systemctl --user stop"
assert_calls_lack "installer "

# Phase two uses the exact binary path persisted by phase one even when PATH
# later resolves a malicious binary.
new_case phase-two-malicious-path Darwin 0.2.0 stopped homebrew
run_upgrade --version 0.4.0 --method homebrew
assert_status 0
persisted_binary=$(session_value binary_path)
expected_brew_binary=$(CDPATH='' cd -P "$brew_prefix/bin" && pwd -P)/car-go-clean
test "$persisted_binary" = "$expected_brew_binary"
rm -f "$fake_bin/car-go-clean"
cat > "$fake_bin/car-go-clean" <<'EOF'
#!/bin/sh
echo "malicious PATH binary invoked" >&2
exit 88
EOF
chmod +x "$fake_bin/car-go-clean"
: > "$binary_path_log"
: > "$call_log"
run_upgrade --version 0.4.0 --method homebrew --execute-review 42
assert_status 0
assert_review_call_count 1
test "$(sort -u "$binary_path_log")" = "$persisted_binary"

new_case phase-two-stale-shell-path Linux 0.3.0 stopped shell
run_upgrade --version 0.4.0 --method shell
assert_status 0
persisted_binary=$(session_value binary_path)
expected_shell_binary=$(CDPATH='' cd -P "$fake_bin" && pwd -P)/car-go-clean
test "$persisted_binary" = "$expected_shell_binary"
mkdir -p "$current_case/malicious-bin"
cat > "$current_case/malicious-bin/car-go-clean" <<'EOF'
#!/bin/sh
echo "stale shell/PATH binary invoked" >&2
exit 89
EOF
chmod +x "$current_case/malicious-bin/car-go-clean"
: > "$binary_path_log"
: > "$call_log"
if PATH="$current_case/malicious-bin:$fake_bin:/usr/bin:/bin" HOME="$home" \
    "$upgrade" --version 0.4.0 --method shell --execute-review 42 \
    > "$output_file" 2>&1; then
    run_status=0
else
    run_status=$?
fi
assert_status 0
assert_review_call_count 1
test "$(sort -u "$binary_path_log")" = "$persisted_binary"

# A replacement changed before reviewed execution may coincide with external
# manager recreation. Validation failure must best-effort disable and stop it
# without executing the review, while retaining both recovery artifacts.
for fixture in \
    changed-before-execution-macos:Darwin \
    changed-before-execution-linux:Linux
do
    old_ifs=$IFS
    IFS=:
    # shellcheck disable=SC2086 # Intentional splitting of colon-delimited fixture fields.
    set -- $fixture
    IFS=$old_ifs
    new_case "$1" "$2" 0.3.0 running homebrew
    run_upgrade --version 0.4.0 --method homebrew
    assert_status 0
    persisted_binary=$(session_value binary_path)
    printf '%s\n' '# changed before reviewed execution' >> "$persisted_binary"
    : > "$service_enabled"
    printf 'running\n' > "$service_state"
    : > "$call_log"

    run_upgrade --version 0.4.0 --method homebrew --execute-review 42

    test "$run_status" -ne 0
    assert_review_call_count 0
    test ! -e "$service_enabled"
    test "$(cat "$service_state")" = stopped
    test -f "$state_dir/upgrade-session"
    test -f "$service_definition_backup"
done

# The binary and refreshed definition authenticated by phase one remain part of
# the reviewed session. A change during reviewed execution must be detected
# immediately before manager convergence for both complete outcomes.
for fixture in \
    final-auth-binary-macos-zero:Darwin:0 \
    final-auth-binary-macos-two:Darwin:2 \
    final-auth-binary-linux-zero:Linux:0 \
    final-auth-binary-linux-two:Linux:2 \
    final-auth-definition-macos-zero:Darwin:0 \
    final-auth-definition-macos-two:Darwin:2 \
    final-auth-definition-linux-two:Linux:2 \
    final-auth-definition-linux-zero:Linux:0
do
    old_ifs=$IFS
    IFS=:
    # shellcheck disable=SC2086 # Intentional splitting of colon-delimited fixture fields.
    set -- $fixture
    IFS=$old_ifs
    new_case "$1" "$2" 0.3.0 running homebrew
    run_upgrade --version 0.4.0 --method homebrew
    assert_status 0
    test -n "$(session_value binary_sha256)"
    test -n "$(session_value refreshed_definition_sha256)"
    case "$1" in
        *binary*) EXECUTE_MUTATE_BINARY=1 ;;
        *definition*) EXECUTE_MUTATE_DEFINITION=1 ;;
    esac
    EXECUTE_EXIT=$3
    export EXECUTE_EXIT EXECUTE_MUTATE_BINARY EXECUTE_MUTATE_DEFINITION
    : > "$call_log"

    run_upgrade --version 0.4.0 --method homebrew --execute-review 42

    test "$run_status" -ne 0
    test "$(session_value phase)" = executed
    test -f "$state_dir/upgrade-session"
    test -f "$service_definition_backup"
    test ! -e "$service_enabled"
    test "$(cat "$service_state")" = stopped
    case "$2" in
        Darwin)
            assert_calls_lack "launchctl enable"
            assert_calls_lack "launchctl bootstrap"
            assert_calls_lack "launchctl kickstart"
            ;;
        Linux)
            assert_calls_lack "systemctl --user enable --now car-go-clean.service"
            assert_calls_lack "systemctl --user enable car-go-clean.service"
            assert_calls_lack "systemctl --user start car-go-clean.service"
            ;;
    esac
done

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
    # shellcheck disable=SC2086 # Intentional splitting of colon-delimited fixture fields.
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
                assert_calls_have "systemctl --user enable --now car-go-clean.service"
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
            assert_calls_have "systemctl --user enable --now car-go-clean.service"
            ;;
        stopped|"not installed")
            test "$(cat "$service_state")" = "$state"
            assert_calls_lack "systemctl --user enable --now car-go-clean.service"
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

# A native activity-query failure is known before exact-old recovery starts.
# Recovery must not transiently enable or start through either manager.
for fixture in \
    recovery-query-error-macos:Darwin:0.2.0 \
    recovery-query-error-linux:Linux:0.3.0
do
    old_ifs=$IFS
    IFS=:
    # shellcheck disable=SC2086 # Intentional splitting of colon-delimited fixture fields.
    set -- $fixture
    IFS=$old_ifs
    new_case "$1" "$2" "$3" running homebrew
    BREW_REPLACE_FAIL=1
    MANAGER_ACTIVITY_QUERY_ERROR=1
    export BREW_REPLACE_FAIL MANAGER_ACTIVITY_QUERY_ERROR
    run_upgrade --version 0.4.0 --method homebrew
    test "$run_status" -ne 0
    test "$(cat "$service_state")" = stopped
    test ! -e "$service_enabled"
    test -f "$state_dir/upgrade-session"
    test -f "$service_definition_backup"
    case "$2" in
        Darwin)
            assert_calls_lack "launchctl enable"
            assert_calls_lack "launchctl bootstrap"
            assert_calls_lack "launchctl kickstart"
            ;;
        Linux)
            assert_calls_lack "systemctl --user enable --now car-go-clean.service"
            assert_calls_lack "systemctl --user start car-go-clean.service"
            ;;
    esac
done

# launchctl's documented not-found diagnostics are inactive only with exit 113.
# Unknown diagnostics and a not-found message paired with another exit are errors.
for fixture in \
    recovery-launchd-unknown:75 \
    recovery-launchd-not-found-wrong-exit:71
do
    old_ifs=$IFS
    IFS=:
    # shellcheck disable=SC2086 # Intentional splitting of colon-delimited fixture fields.
    set -- $fixture
    IFS=$old_ifs
    new_case "$1" Darwin 0.3.0 running homebrew
    BREW_REPLACE_FAIL=1
    MANAGER_ACTIVITY_STATUS=$2
    case "$1" in
        recovery-launchd-unknown)
            MANAGER_ACTIVITY_OUTPUT='launchctl activity state unknown'
            ;;
        recovery-launchd-not-found-wrong-exit)
            MANAGER_ACTIVITY_OUTPUT='Could not find specified service'
            ;;
    esac
    export BREW_REPLACE_FAIL MANAGER_ACTIVITY_OUTPUT MANAGER_ACTIVITY_STATUS
    run_upgrade --version 0.4.0 --method homebrew
    test "$run_status" -ne 0
    test "$(cat "$service_state")" = stopped
    test ! -e "$service_enabled"
    test -f "$state_dir/upgrade-session"
    test -f "$service_definition_backup"
    assert_calls_lack "launchctl enable"
    assert_calls_lack "launchctl bootstrap"
    assert_calls_lack "launchctl kickstart"
done

# systemctl reports several nonterminal or indeterminate states with statuses
# that are easy to confuse with the terminal active/inactive pair. None may
# authorize recovery, and exit/output mismatches must fail closed as well.
nonterminal_activity_fixtures='activating:activating:3
deactivating:deactivating:3
reloading:reloading:0
refreshing:refreshing:0
maintenance:maintenance:3
failed:failed:3
unknown:unknown:4
active-wrong-exit:active:3
inactive-wrong-exit:inactive:0'

for fixture in $nonterminal_activity_fixtures
do
    old_ifs=$IFS
    IFS=:
    # shellcheck disable=SC2086 # Intentional splitting of colon-delimited fixture fields.
    set -- $fixture
    IFS=$old_ifs
    new_case "recovery-activity-$1" Linux 0.3.0 running homebrew
    BREW_REPLACE_FAIL=1
    MANAGER_ACTIVITY_OUTPUT=$2
    MANAGER_ACTIVITY_STATUS=$3
    export BREW_REPLACE_FAIL MANAGER_ACTIVITY_OUTPUT MANAGER_ACTIVITY_STATUS
    run_upgrade --version 0.4.0 --method homebrew
    test "$run_status" -ne 0
    test "$(cat "$service_state")" = stopped
    test ! -e "$service_enabled"
    test -f "$state_dir/upgrade-session"
    test -f "$service_definition_backup"
    assert_calls_lack "systemctl --user enable --now car-go-clean.service"
    assert_calls_lack "systemctl --user enable car-go-clean.service"
    assert_calls_lack "systemctl --user start car-go-clean.service"
done

# The manager preflight is an attacker-controlled scheduling boundary. If the
# definition's visible executable is swapped after an earlier authentication,
# exact-old recovery must authenticate again immediately before any start.
for fixture in \
    recovery-post-auth-swap-macos:Darwin:0.2.0 \
    recovery-post-auth-swap-linux:Linux:0.3.0
do
    old_ifs=$IFS
    IFS=:
    # shellcheck disable=SC2086 # Intentional splitting of colon-delimited fixture fields.
    set -- $fixture
    IFS=$old_ifs
    new_case "$1" "$2" "$3" running homebrew
    BREW_REPLACE_FAIL=1
    DEFINITION_PREFLIGHT_SWAP=1
    export BREW_REPLACE_FAIL DEFINITION_PREFLIGHT_SWAP
    run_upgrade --version 0.4.0 --method homebrew
    test "$run_status" -ne 0
    test -e "$definition_preflight_swap_marker"
    test "$(readlink "$fake_bin/car-go-clean")" = "$new_formula_binary"
    test "$(cat "$service_state")" = stopped
    test ! -e "$service_enabled"
    test -f "$state_dir/upgrade-session"
    test -f "$service_definition_backup"
    case "$2" in
        Darwin)
            assert_calls_lack "launchctl bootstrap"
            assert_calls_lack "launchctl kickstart"
            ;;
        Linux)
            assert_calls_lack "systemctl --user enable --now car-go-clean.service"
            assert_calls_lack "systemctl --user start car-go-clean.service"
            ;;
    esac
done

# Enabling is another manager-controlled boundary. Recovery must authenticate
# the definition once more after enable and before bootstrap/start.
for fixture in \
    recovery-enable-swap-macos:Darwin:0.3.0 \
    recovery-enable-swap-linux:Linux:0.2.0
do
    old_ifs=$IFS
    IFS=:
    # shellcheck disable=SC2086 # Intentional splitting of colon-delimited fixture fields.
    set -- $fixture
    IFS=$old_ifs
    new_case "$1" "$2" "$3" running homebrew
    BREW_REPLACE_FAIL=1
    DEFINITION_ENABLE_SWAP=1
    export BREW_REPLACE_FAIL DEFINITION_ENABLE_SWAP
    run_upgrade --version 0.4.0 --method homebrew
    test "$run_status" -ne 0
    test -e "$definition_enable_swap_marker"
    test "$(readlink "$fake_bin/car-go-clean")" = "$new_formula_binary"
    test "$(cat "$service_state")" = stopped
    test ! -e "$service_enabled"
    test -f "$state_dir/upgrade-session"
    test -f "$service_definition_backup"
    case "$2" in
        Darwin)
            assert_calls_have "launchctl enable"
            assert_calls_lack "launchctl bootstrap"
            assert_calls_lack "launchctl kickstart"
            ;;
        Linux)
            assert_calls_have "systemctl --user enable car-go-clean.service"
            assert_calls_lack "systemctl --user enable --now car-go-clean.service"
            assert_calls_lack "systemctl --user start car-go-clean.service"
            ;;
    esac
done

# Hashing and parsing must authenticate one stable definition. The checksum
# double swaps in a definition with the same executable but hostile directives
# after returning the trusted digest; recovery must detect it before start.
for fixture in \
    definition-auth-race-macos:Darwin:0.3.0 \
    definition-auth-race-linux:Linux:0.2.0
do
    old_ifs=$IFS
    IFS=:
    # shellcheck disable=SC2086 # Intentional splitting of colon-delimited fixture fields.
    set -- $fixture
    IFS=$old_ifs
    new_case "$1" "$2" "$3" running homebrew
    BREW_REPLACE_FAIL=1
    DEFINITION_AUTH_RACE=1
    export BREW_REPLACE_FAIL DEFINITION_AUTH_RACE
    run_upgrade --version 0.4.0 --method homebrew
    test "$run_status" -ne 0
    test -e "$definition_auth_race_marker"
    test "$(cat "$service_state")" = stopped
    test ! -e "$service_enabled"
    test -f "$state_dir/upgrade-session"
    test -f "$service_definition_backup"
    grep -F 'attacker' "$service_definition" >/dev/null
    if grep -F 'attacker' "$service_definition_backup" >/dev/null; then
        echo "trusted service-definition backup was unexpectedly changed" >&2
        exit 1
    fi
    case "$2" in
        Darwin)
            assert_calls_lack "launchctl enable"
            assert_calls_lack "launchctl bootstrap"
            ;;
        Linux)
            assert_calls_lack "systemctl --user enable --now car-go-clean.service"
            ;;
    esac
done

# Once replacement can have mutated the installation, failures must retain
# recovery state and must never restart an unvalidated binary.
new_case brew-partial-replacement Darwin 0.2.0 running homebrew
BREW_PARTIAL_REPLACE_FAIL=1
export BREW_PARTIAL_REPLACE_FAIL
run_upgrade --version 0.4.0 --method homebrew
test "$run_status" -ne 0
test "$(cat "$service_state")" = stopped
test ! -e "$service_enabled"
test -f "$state_dir/upgrade-session"
test "$(session_value phase)" = replacement_attempt
test "$(session_value binary_path)" = unresolved
test "$(session_value old_binary_path)" = \
    "$(canonical_fixture_path "$brew_prefix/bin/car-go-clean")"
assert_output_has "The originally active service remains persistently disabled and stopped."
assert_output_has "rollback"
assert_calls_lack "launchctl enable"
assert_calls_lack "launchctl bootstrap"

# Automatic recovery authenticates both historical definition formats again
# after replacement. A moved or missing executable and a missing, relative, or
# unparseable definition all retain evidence without a manager start.
for fixture in \
    moved-definition-link-macos:Darwin:0.2.0:moved-executable \
    moved-definition-link-linux:Linux:0.3.0:moved-executable \
    missing-definition-executable-macos:Darwin:0.3.0:missing-executable \
    missing-definition-linux:Linux:0.2.0:missing-definition \
    relative-definition-macos:Darwin:0.2.0:relative-definition \
    unparseable-definition-linux:Linux:0.3.0:unparseable-definition
do
    old_ifs=$IFS
    IFS=:
    # shellcheck disable=SC2086 # Intentional splitting of colon-delimited fixture fields.
    set -- $fixture
    IFS=$old_ifs
    new_case "$1" "$2" "$3" running homebrew
    BREW_RECOVERY_DEFINITION_FAILURE=$4
    export BREW_RECOVERY_DEFINITION_FAILURE
    run_upgrade --version 0.4.0 --method homebrew
    test "$run_status" -ne 0
    test "$("$brew_prefix/bin/car-go-clean" version)" = "$3"
    if [ "$4" = moved-executable ]; then
        test "$("$new_formula_binary" version)" = 0.4.0
        test "$(readlink "$fake_bin/car-go-clean")" = "$new_formula_binary"
    fi
    test "$(cat "$service_state")" = stopped
    test ! -e "$service_enabled"
    test -f "$state_dir/upgrade-session"
    test -f "$service_definition_backup"
    test "$(session_value phase)" = replacement_attempt
    test "$(session_value definition_binary_path)" = \
        "$(canonical_fixture_path "$brew_prefix/bin/car-go-clean")"
    case "$2" in
        Darwin)
            grep -F '<key>ProgramArguments</key>' \
                "$service_definition_backup" >/dev/null
            assert_calls_lack "launchctl enable"
            assert_calls_lack "launchctl bootstrap"
            ;;
        Linux)
            grep -F "ExecStart=\"$fake_bin/car-go-clean\" daemon" \
                "$service_definition_backup" >/dev/null
            assert_calls_lack "systemctl --user enable"
            assert_calls_lack "systemctl --user start"
            ;;
    esac
done

new_case brew-post-success-resolution Linux 0.3.0 running homebrew
BREW_RESOLVE_FAIL_AFTER_SUCCESS=1
export BREW_RESOLVE_FAIL_AFTER_SUCCESS
run_upgrade --version 0.4.0 --method homebrew
test "$run_status" -ne 0
test "$(cat "$service_state")" = stopped
test ! -e "$service_enabled"
test -f "$state_dir/upgrade-session"
test "$(session_value phase)" = replacement_attempt
test "$(session_value binary_path)" = unresolved
test "$(session_value old_binary_path)" = \
    "$(canonical_fixture_path "$brew_prefix/bin/car-go-clean")"
assert_output_has "The originally active service remains persistently disabled and stopped."
assert_output_has "rollback"
assert_calls_lack "systemctl --user enable"

new_case shell-partial-replacement Darwin 0.2.0 running shell
SHELL_PARTIAL_REPLACE_FAIL=1
export SHELL_PARTIAL_REPLACE_FAIL
run_upgrade --version 0.4.0 --method shell
test "$run_status" -ne 0
test "$(cat "$service_state")" = stopped
test ! -e "$service_enabled"
test -f "$state_dir/upgrade-session"
test "$(session_value phase)" = replacement_attempt
test "$(session_value binary_path)" = unresolved
test "$(session_value old_binary_path)" = \
    "$(canonical_fixture_path "$fake_bin/car-go-clean")"
assert_output_has "The originally active service remains persistently disabled and stopped."
assert_output_has "rollback"
assert_calls_lack "launchctl enable"
assert_calls_lack "launchctl bootstrap"

new_case shell-post-success-resolution Linux 0.3.0 running shell
SHELL_RESOLVE_FAIL_AFTER_SUCCESS=1
export SHELL_RESOLVE_FAIL_AFTER_SUCCESS
run_upgrade --version 0.4.0 --method shell
test "$run_status" -ne 0
test "$(cat "$service_state")" = stopped
test ! -e "$service_enabled"
test -f "$state_dir/upgrade-session"
test "$(session_value phase)" = replacement_attempt
test "$(session_value binary_path)" = unresolved
test "$(session_value old_binary_path)" = \
    "$(canonical_fixture_path "$fake_bin/car-go-clean")"
assert_output_has "The originally active service remains persistently disabled and stopped."
assert_output_has "rollback"
assert_calls_lack "systemctl --user enable"

new_case wrong-replacement-version Linux 0.3.0 running homebrew
WRONG_NEW_VERSION=1
export WRONG_NEW_VERSION
run_upgrade --version 0.4.0 --method homebrew
test "$run_status" -ne 0
test "$(cat "$service_state")" = stopped
test ! -e "$service_enabled"
assert_output_has "expected car-go-clean 0.4.0"
assert_output_has "rollback"
assert_calls_lack "systemctl --user enable --now car-go-clean.service"
test -f "$state_dir/upgrade-session"
test "$(session_value phase)" = replacement_pending
test "$(session_value review_id)" = none

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
assert_output_has "launchctl enable"
if grep -F "car-go-clean service start" "$output_file" >/dev/null; then
    echo "v0.2 rollback guidance used an unsupported lifecycle verb" >&2
    exit 1
fi

CONFIG_EXIT=0
export CONFIG_EXIT
persisted_binary=$(session_value binary_path)
rm -f "$fake_bin/car-go-clean"
cat > "$fake_bin/car-go-clean" <<'EOF'
#!/bin/sh
echo "malicious preview-resume PATH binary invoked" >&2
exit 87
EOF
chmod +x "$fake_bin/car-go-clean"
: > "$binary_path_log"
: > "$call_log"
run_upgrade --version 0.4.0 --method homebrew
assert_status 0
test "$(session_value phase)" = review_pending
test "$(session_value review_id)" = 42
assert_calls_lack "brew "
assert_calls_lack "service status"
test "$(sort -u "$binary_path_log")" = "$persisted_binary"
test "$(cat "$service_state")" = stopped

new_case stopped-preview-failure Linux 0.3.0 stopped shell
PREVIEW_EXIT=1
export PREVIEW_EXIT
run_upgrade --version 0.4.0 --method shell
test "$run_status" -ne 0
test "$(session_value phase)" = preview_pending
assert_output_has "releases/download/v0.3.0/car-go-clean-installer.sh"
assert_output_has "$service_definition_backup"
assert_output_has "rollback_version="
assert_output_has "$fake_bin/car-go-clean"
assert_output_has "systemctl --user daemon-reload"
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
    # shellcheck disable=SC2086 # Intentional splitting of colon-delimited fixture fields.
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
    if [ "$3" != "not installed" ]; then
        assert_historical_definition "$service_definition"
    fi
    test "$(cat "$brew_linked_version_file")" = "$2"
    test "$(cat "$brew_linked_formula")" = \
        "$USER/car-go-clean-rollback/car-go-clean@$2"
    resolved_version=$(PATH="$fake_bin:/usr/bin:/bin" car-go-clean version)
    test "$resolved_version" = "$2"
    assert_calls_have "brew extract --force --version=$2 dcchuck/tap/car-go-clean $USER/car-go-clean-rollback"
    assert_calls_have "brew unlink car-go-clean"
    assert_calls_have "brew install $USER/car-go-clean-rollback/car-go-clean@$2"
    assert_calls_have "brew link --force --overwrite $USER/car-go-clean-rollback/car-go-clean@$2"
    assert_calls_have "brew --prefix $USER/car-go-clean-rollback/car-go-clean@$2"
    grep -Fqx \
        "$(canonical_fixture_path "$brew_rollback_prefix/bin/car-go-clean")" \
        "$binary_path_log"
    case "$tap_mode" in
        create) assert_calls_have "brew tap-new $USER/car-go-clean-rollback" ;;
        reuse) assert_calls_lack "brew tap-new $USER/car-go-clean-rollback" ;;
    esac
    case "$3" in
        running)
            test "$(cat "$service_state")" = running
            awk '
                /^car-go-clean version$/ { validated = NR }
                /^launchctl enable / {
                    if (!validated || validated >= NR) exit 1
                    started = 1
                }
                END { if (!started) exit 1 }
            ' "$call_log"
            assert_calls_lack "car-go-clean service start"
            ;;
        stopped|"not installed")
            test "$(cat "$service_state")" = "$3"
            assert_calls_have "car-go-clean version"
            assert_calls_lack "car-go-clean service start"
            assert_calls_lack "launchctl enable"
            ;;
    esac
done

# A PATH shadow that reports the requested old version is still not the exact
# binary installed and linked from the rollback formula.
new_case rollback-path-shadow Darwin 0.3.0 running homebrew
CONFIG_EXIT=1
export CONFIG_EXIT
run_upgrade --version 0.4.0 --method homebrew
test "$run_status" -ne 0
capture_homebrew_rollback
ROLLBACK_SHADOW_DIR=$current_case/shadow-bin
mkdir -p "$ROLLBACK_SHADOW_DIR"
cp "$car_go_clean_fixture" "$ROLLBACK_SHADOW_DIR/car-go-clean"
chmod +x "$ROLLBACK_SHADOW_DIR/car-go-clean"
export ROLLBACK_SHADOW_DIR
: > "$call_log"
run_captured_homebrew_rollback
test "$rollback_status" -ne 0
test "$(cat "$service_state")" = stopped
assert_calls_have "brew --prefix $USER/car-go-clean-rollback/car-go-clean@0.3.0"
assert_calls_lack "launchctl enable"

# Homebrew rollback is also executable through the complementary Linux manager.
new_case rollback-homebrew-linux-active Linux 0.3.0 running homebrew
CONFIG_EXIT=1
export CONFIG_EXIT
run_upgrade --version 0.4.0 --method homebrew
test "$run_status" -ne 0
capture_homebrew_rollback
: > "$call_log"
run_captured_homebrew_rollback
test "$rollback_status" -eq 0
test "$(cat "$service_state")" = running
test "$(cat "$brew_linked_version_file")" = 0.3.0
assert_historical_definition "$service_definition"
assert_calls_have "systemctl --user daemon-reload"
assert_calls_have "systemctl --user enable --now car-go-clean.service"
assert_calls_lack "car-go-clean service start"

# Printed shell rollback blocks execute for every prior service state on Linux,
# plus an active Darwin service, without relying on old lifecycle verbs.
for fixture in \
    rollback-shell-v02-linux-active:Linux:0.2.0:running \
    rollback-shell-v03-linux-stopped:Linux:0.3.0:stopped \
    rollback-shell-v03-linux-absent:Linux:0.3.0:'not installed' \
    rollback-shell-v02-darwin-active:Darwin:0.2.0:running
do
    old_ifs=$IFS
    IFS=:
    # shellcheck disable=SC2086 # Intentional splitting of colon-delimited fixture fields.
    set -- $fixture
    IFS=$old_ifs
    new_case "$1" "$2" "$3" "$4" shell
    CONFIG_EXIT=1
    export CONFIG_EXIT
    run_upgrade --version 0.4.0 --method shell
    test "$run_status" -ne 0
    capture_shell_rollback

    : > "$call_log"
    : > "$binary_path_log"
    run_captured_shell_rollback
    test "$rollback_status" -eq 0
    test "$(cat "$version_file")" = "$3"
    grep -Fqx "$(canonical_fixture_path "$fake_bin/car-go-clean")" \
        "$binary_path_log"
    assert_calls_have "installer --version $3 --install-dir $(dirname "$(canonical_fixture_path "$fake_bin/car-go-clean")")"
    assert_calls_lack "car-go-clean service start"
    assert_calls_lack "car-go-clean service stop"
    if [ "$4" != "not installed" ]; then
        assert_historical_definition "$service_definition"
    fi
    case "$4" in
        running)
            test "$(cat "$service_state")" = running
            case "$2" in
                Darwin) assert_calls_have "launchctl bootstrap" ;;
                Linux) assert_calls_have "systemctl --user enable --now car-go-clean.service" ;;
            esac
            ;;
        stopped|"not installed")
            test "$(cat "$service_state")" = "$4"
            assert_calls_lack "launchctl enable"
            assert_calls_lack "systemctl --user enable --now car-go-clean.service"
            ;;
    esac
done

# Rollback restoration treats the saved definition as untrusted input. Symlinks,
# broad permissions, replacement, and a copy-time race must all fail stopped.
for fixture in \
    rollback-backup-symlink:Darwin:0.2.0:homebrew:symlink \
    rollback-backup-mode:Linux:0.3.0:shell:mode \
    rollback-backup-replacement:Linux:0.2.0:homebrew:replacement \
    rollback-backup-race:Darwin:0.3.0:shell:race
do
    old_ifs=$IFS
    IFS=:
    # shellcheck disable=SC2086 # Intentional splitting of colon-delimited fixture fields.
    set -- $fixture
    IFS=$old_ifs
    new_case "$1" "$2" "$3" running "$4"
    CONFIG_EXIT=1
    export CONFIG_EXIT
    run_upgrade --version 0.4.0 --method "$4"
    test "$run_status" -ne 0
    case "$4" in
        homebrew) capture_homebrew_rollback ;;
        shell) capture_shell_rollback ;;
    esac

    case "$5" in
        symlink)
            mv "$service_definition_backup" "$service_definition_backup.safe"
            ln -s "$service_definition_backup.safe" "$service_definition_backup"
            ;;
        mode)
            chmod 0644 "$service_definition_backup"
            ;;
        replacement)
            printf '%s\n' '# attacker-replaced-service-definition' \
                > "$service_definition_backup"
            chmod 0600 "$service_definition_backup"
            ;;
        race)
            BACKUP_RACE=1
            export BACKUP_RACE
            ;;
    esac

    : > "$call_log"
    case "$4" in
        homebrew) run_captured_homebrew_rollback ;;
        shell) run_captured_shell_rollback ;;
    esac
    test "$rollback_status" -ne 0
    test "$(cat "$service_state")" = stopped
    grep -F "Rollback or service restoration failed" "$rollback_output" >/dev/null
    assert_calls_lack "launchctl enable"
    assert_calls_lack "systemctl --user enable --now car-go-clean.service"
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
    assert_calls_lack "launchctl enable"
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
assert_output_has "legacy \`excludes\`"
assert_output_has "car-go-clean config migrate"

# Exact machine envelopes prove that these review rejections happened before
# any target event. They return to a retryable preview state without claiming
# cleanup, while retaining service recovery evidence.
for fixture in \
    pre-execution-missing:Darwin:missing \
    pre-execution-expired:Linux:expired \
    pre-execution-policy:Darwin:policy \
    pre-execution-generation:Linux:generation
do
    old_ifs=$IFS
    IFS=:
    # shellcheck disable=SC2086 # Intentional splitting of colon-delimited fixture fields.
    set -- $fixture
    IFS=$old_ifs
    new_case "$1" "$2" 0.3.0 running homebrew
    run_upgrade --version 0.4.0 --method homebrew
    assert_status 0
    EXECUTE_REJECTION=$3
    export EXECUTE_REJECTION
    : > "$call_log"

    run_upgrade --version 0.4.0 --method homebrew --execute-review 42

    test "$run_status" -ne 0
    assert_review_call_count 1
    test "$(session_value phase)" = preview_pending
    test "$(session_value review_id)" = none
    test "$(cat "$service_state")" = stopped
    test ! -e "$service_enabled"
    test -f "$state_dir/upgrade-session"
    test -f "$service_definition_backup"
    assert_output_has "new preview"
    case "$2" in
        Darwin)
            assert_calls_lack "launchctl enable"
            assert_calls_lack "launchctl bootstrap"
            ;;
        Linux)
            assert_calls_lack "systemctl --user enable"
            assert_calls_lack "systemctl --user start"
            ;;
    esac

    EXECUTE_REJECTION=
    export EXECUTE_REJECTION
    : > "$call_log"
    run_upgrade --version 0.4.0 --method homebrew
    assert_status 0
    test "$(session_value phase)" = review_pending
    test "$(session_value review_id)" = 42
    assert_calls_lack "run --review"
done

# A target event, malformed or absent terminal envelope, and an unknown
# structured failure can all follow execution; each remains ambiguous.
for fixture in \
    ambiguous-target:expired:1 \
    ambiguous-malformed:malformed:0 \
    ambiguous-missing-terminal:missing-terminal:0 \
    ambiguous-unknown:unknown:0
do
    old_ifs=$IFS
    IFS=:
    # shellcheck disable=SC2086 # Intentional splitting of colon-delimited fixture fields.
    set -- $fixture
    IFS=$old_ifs
    new_case "$1" Linux 0.3.0 running homebrew
    run_upgrade --version 0.4.0 --method homebrew
    assert_status 0
    EXECUTE_REJECTION=$2
    EXECUTE_TARGET_EVENT=$3
    export EXECUTE_REJECTION EXECUTE_TARGET_EVENT
    : > "$call_log"

    run_upgrade --version 0.4.0 --method homebrew --execute-review 42

    test "$run_status" -ne 0
    assert_review_call_count 1
    test "$(session_value phase)" = executing
    test "$(session_value review_id)" = 42
    test "$(cat "$service_state")" = stopped
    test ! -e "$service_enabled"
    test -f "$state_dir/upgrade-session"
    test -f "$service_definition_backup"
    assert_output_has "will not run review 42 again"
    assert_calls_lack "systemctl --user enable"
    assert_calls_lack "systemctl --user start"
done

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

# Finalization converges persistent enablement and current activity
# independently before deleting recovery state.
new_case finalize-active-enabled-inactive Darwin 0.2.0 running homebrew
run_upgrade --version 0.4.0 --method homebrew
assert_status 0
: > "$service_enabled"
printf 'stopped\n' > "$service_state"
: > "$call_log"
run_upgrade --version 0.4.0 --method homebrew --execute-review 42
assert_status 0
test -e "$service_enabled"
test "$(cat "$service_state")" = running
assert_calls_lack "launchctl enable"
assert_calls_have "launchctl bootstrap"
test ! -e "$state_dir/upgrade-session"
test ! -e "$service_definition_backup"

new_case finalize-active-disabled-active Linux 0.3.0 running homebrew
run_upgrade --version 0.4.0 --method homebrew
assert_status 0
rm -f "$service_enabled"
printf 'running\n' > "$service_state"
: > "$call_log"
run_upgrade --version 0.4.0 --method homebrew --execute-review 42
assert_status 0
test -e "$service_enabled"
test "$(cat "$service_state")" = running
assert_calls_have "systemctl --user enable car-go-clean.service"
assert_calls_lack "systemctl --user start car-go-clean.service"
test ! -e "$state_dir/upgrade-session"
test ! -e "$service_definition_backup"

new_case finalize-stopped-enabled-inactive Darwin 0.2.0 stopped homebrew
run_upgrade --version 0.4.0 --method homebrew
assert_status 0
: > "$service_enabled"
printf 'stopped\n' > "$service_state"
: > "$call_log"
run_upgrade --version 0.4.0 --method homebrew --execute-review 42
assert_status 0
test ! -e "$service_enabled"
test "$(cat "$service_state")" = stopped
assert_calls_have "launchctl disable"
assert_calls_lack "launchctl bootout"
test ! -e "$state_dir/upgrade-session"
test ! -e "$service_definition_backup"

new_case finalize-stopped-disabled-active Linux 0.3.0 stopped homebrew
run_upgrade --version 0.4.0 --method homebrew
assert_status 0
rm -f "$service_enabled"
printf 'running\n' > "$service_state"
: > "$call_log"
run_upgrade --version 0.4.0 --method homebrew --execute-review 42
assert_status 0
test ! -e "$service_enabled"
test "$(cat "$service_state")" = stopped
assert_calls_have "systemctl --user stop car-go-clean.service"
assert_calls_lack "systemctl --user disable car-go-clean.service"
test ! -e "$state_dir/upgrade-session"
test ! -e "$service_definition_backup"

# Activity-query failures are not inactivity. Active/stopped origins fail closed,
# retain both recovery artifacts, and never start a service. Absent origins skip
# manager state probes entirely and can finalize normally.
for fixture in \
    query-error-macos-active:Darwin:0.2.0:running \
    query-error-macos-stopped:Darwin:0.3.0:stopped \
    query-error-macos-absent:Darwin:0.2.0:'not installed' \
    query-error-linux-active:Linux:0.3.0:running \
    query-error-linux-stopped:Linux:0.2.0:stopped \
    query-error-linux-absent:Linux:0.3.0:'not installed'
do
    old_ifs=$IFS
    IFS=:
    # shellcheck disable=SC2086 # Intentional splitting of colon-delimited fixture fields.
    set -- $fixture
    IFS=$old_ifs
    new_case "$1" "$2" "$3" "$4" homebrew
    run_upgrade --version 0.4.0 --method homebrew
    assert_status 0
    MANAGER_ACTIVITY_QUERY_ERROR=1
    export MANAGER_ACTIVITY_QUERY_ERROR
    : > "$call_log"
    run_upgrade --version 0.4.0 --method homebrew --execute-review 42
    case "$4" in
        running|stopped)
            test "$run_status" -ne 0
            test "$(cat "$service_state")" = stopped
            test ! -e "$service_enabled"
            test -f "$state_dir/upgrade-session"
            test -f "$service_definition_backup"
            test "$(session_value phase)" = executed
            assert_output_has "service remains stopped"
            case "$2" in
                Darwin)
                    assert_calls_lack "launchctl bootstrap"
                    assert_calls_lack "launchctl kickstart"
                    ;;
                Linux)
                    assert_calls_lack "systemctl --user enable --now car-go-clean.service"
                    assert_calls_lack "systemctl --user start car-go-clean.service"
                    ;;
            esac
            ;;
        "not installed")
            assert_status 0
            test "$(cat "$service_state")" = "not installed"
            test ! -e "$state_dir/upgrade-session"
            test ! -e "$service_definition_backup"
            case "$2" in
                Darwin)
                    assert_calls_lack "launchctl print "
                    assert_calls_lack "launchctl print-disabled"
                    assert_calls_lack "launchctl bootstrap"
                    ;;
                Linux)
                    assert_calls_lack "systemctl --user is-active"
                    assert_calls_lack "systemctl --user is-enabled"
                    assert_calls_lack "systemctl --user start"
                    ;;
            esac
            ;;
    esac
done

# An unrecognized launchctl diagnostic is likewise neither active nor inactive
# during finalization, regardless of the originally desired service state.
for original_fixture_state in active stopped
do
    case "$original_fixture_state" in
        active) fixture_service_state=running ;;
        stopped) fixture_service_state=stopped ;;
    esac
    new_case "finalize-launchd-unknown-$original_fixture_state" \
        Darwin 0.3.0 "$fixture_service_state" homebrew
    run_upgrade --version 0.4.0 --method homebrew
    assert_status 0
    MANAGER_ACTIVITY_OUTPUT='launchctl activity state unknown'
    MANAGER_ACTIVITY_STATUS=75
    export MANAGER_ACTIVITY_OUTPUT MANAGER_ACTIVITY_STATUS
    : > "$call_log"
    run_upgrade --version 0.4.0 --method homebrew --execute-review 42
    test "$run_status" -ne 0
    test "$(cat "$service_state")" = stopped
    test ! -e "$service_enabled"
    test -f "$state_dir/upgrade-session"
    test -f "$service_definition_backup"
    test "$(session_value phase)" = executed
    assert_output_has "service remains stopped"
    assert_calls_lack "launchctl enable"
    assert_calls_lack "launchctl bootstrap"
    assert_calls_lack "launchctl kickstart"
done

# Finalization also requires a terminal manager state. A transient, failed,
# unknown, or status/output-mismatched report must preserve evidence and leave
# either original state disabled and stopped without attempting a start.
for original_fixture_state in active stopped
do
    case "$original_fixture_state" in
        active) fixture_service_state=running ;;
        stopped) fixture_service_state=stopped ;;
    esac
    for fixture in $nonterminal_activity_fixtures
    do
        old_ifs=$IFS
        IFS=:
        # shellcheck disable=SC2086 # Intentional splitting of colon-delimited fixture fields.
        set -- $fixture
        IFS=$old_ifs
        new_case "finalize-$original_fixture_state-activity-$1" \
            Linux 0.3.0 "$fixture_service_state" homebrew
        run_upgrade --version 0.4.0 --method homebrew
        assert_status 0
        MANAGER_ACTIVITY_OUTPUT=$2
        MANAGER_ACTIVITY_STATUS=$3
        export MANAGER_ACTIVITY_OUTPUT MANAGER_ACTIVITY_STATUS
        : > "$call_log"
        run_upgrade --version 0.4.0 --method homebrew --execute-review 42
        test "$run_status" -ne 0
        test "$(cat "$service_state")" = stopped
        test ! -e "$service_enabled"
        test -f "$state_dir/upgrade-session"
        test -f "$service_definition_backup"
        test "$(session_value phase)" = executed
        assert_output_has "service remains stopped"
        assert_calls_lack "systemctl --user enable --now car-go-clean.service"
        assert_calls_lack "systemctl --user enable car-go-clean.service"
        assert_calls_lack "systemctl --user start car-go-clean.service"

        # Once a later query proves terminal inactivity, retry may converge and
        # only then remove the retained recovery evidence.
        if [ "$1" = maintenance ]; then
            MANAGER_ACTIVITY_OUTPUT=
            MANAGER_ACTIVITY_STATUS=
            export MANAGER_ACTIVITY_OUTPUT MANAGER_ACTIVITY_STATUS
            : > "$call_log"
            run_upgrade --version 0.4.0 --method homebrew --execute-review 42
            assert_status 0
            case "$original_fixture_state" in
                active)
                    test "$(cat "$service_state")" = running
                    test -e "$service_enabled"
                    ;;
                stopped)
                    test "$(cat "$service_state")" = stopped
                    test ! -e "$service_enabled"
                    ;;
            esac
            test ! -e "$state_dir/upgrade-session"
            test ! -e "$service_definition_backup"
        fi
    done
done

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
test "$(grep -c '^car-go-clean run --review 42 --json$' "$call_log")" -eq 1
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
test "$(grep -c '^car-go-clean run --review 42 --json$' "$call_log")" -eq 1
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
test "$(grep -c '^car-go-clean run --review 42 --json$' "$call_log")" -eq 1
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

manual_binary=$(CDPATH='' cd -P "$brew_prefix/bin" && pwd -P)/car-go-clean
manual_digest=0000000000000000000000000000000000000000000000000000000000000000
printf 'format=6\nversion=0.4.0\nmethod=homebrew\nold_version=0.3.0\nservice_state=stopped\nphase=review_pending\nreview_id=42\nbinary_path=%s\nold_binary_path=%s\ndefinition_backup_sha256=%s\ndefinition_binary_path=%s\n' \
    "$manual_binary" "$manual_binary" "$manual_digest" "$manual_binary" \
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

printf 'format=6\nversion=0.4.0\nmethod=homebrew\nold_version=0.3.0\nservice_state=stopped\nphase=review_pending\nreview_id=42\nbinary_path=relative/car-go-clean\nold_binary_path=%s\ndefinition_backup_sha256=%s\ndefinition_binary_path=%s\n' \
    "$manual_binary" "$manual_digest" "$manual_binary" \
    > "$state_dir/upgrade-session"
chmod 600 "$state_dir/upgrade-session"
run_upgrade --version 0.4.0 --method homebrew --execute-review 42
test "$run_status" -ne 0
assert_output_has "malformed"
assert_calls_lack "run --review"

printf 'format=6\nversion=0.4.0\nmethod=homebrew\nold_version=0.3.0\nservice_state=stopped\nphase=review_pending\nreview_id=42\nbinary_path=%s\nbinary_path=%s\nold_binary_path=%s\ndefinition_backup_sha256=%s\ndefinition_binary_path=%s\n' \
    "$manual_binary" "$manual_binary" "$manual_binary" "$manual_digest" "$manual_binary" \
    > "$state_dir/upgrade-session"
run_upgrade --version 0.4.0 --method homebrew --execute-review 42
test "$run_status" -ne 0
assert_output_has "malformed"
assert_calls_lack "run --review"

printf 'format=6\nversion=0.4.0\nmethod=homebrew\nold_version=0.3.0\nservice_state=stopped\nphase=review_pending\nreview_id=42\nbinary_path=%s\nold_binary_path=%s\ndefinition_backup_sha256=%s\ndefinition_binary_path=%s\n' \
    "$fake_bin/car-go-clean" "$manual_binary" "$manual_digest" "$manual_binary" \
    > "$state_dir/upgrade-session"
run_upgrade --version 0.4.0 --method homebrew --execute-review 42
test "$run_status" -ne 0
assert_output_has "no longer exact"
assert_calls_lack "run --review"

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
    # shellcheck disable=SC2086 # Intentional splitting of whitespace-delimited argv fixture.
    set -- $invocation
    IFS=$old_ifs
    run_upgrade "$@"
    test "$run_status" -ne 0
done
TEST_PLATFORM=FreeBSD
export TEST_PLATFORM
run_upgrade --version 0.4.0 --method homebrew
test "$run_status" -ne 0
