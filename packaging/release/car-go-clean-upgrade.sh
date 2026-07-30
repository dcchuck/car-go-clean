#!/bin/sh
set -eu

usage() {
    echo "usage: $0 --version 0.4.0 --method homebrew|shell [--execute-review ID]" >&2
}

die() {
    echo "car-go-clean upgrade: $*" >&2
    exit 1
}

version=
method=
execute_review=
version_seen=false
method_seen=false
execute_seen=false
while [ "$#" -gt 0 ]; do
    case "$1" in
        --version)
            [ "$#" -ge 2 ] || { usage; exit 2; }
            [ "$version_seen" = false ] || die "--version may be supplied only once"
            version_seen=true
            version=$2
            shift 2
            ;;
        --method)
            [ "$#" -ge 2 ] || { usage; exit 2; }
            [ "$method_seen" = false ] || die "--method may be supplied only once"
            method_seen=true
            method=$2
            shift 2
            ;;
        --execute-review)
            [ "$#" -ge 2 ] || { usage; exit 2; }
            [ "$execute_seen" = false ] ||
                die "--execute-review may be supplied only once"
            execute_seen=true
            execute_review=$2
            shift 2
            ;;
        *)
            usage
            exit 2
            ;;
    esac
done

[ "$version" = 0.4.0 ] || die "this helper requires exact --version 0.4.0"
case "$method" in
    homebrew|shell) ;;
    '') usage; exit 2 ;;
    *) die "unsupported upgrade method: $method" ;;
esac
case "$execute_review" in
    ''|*[!0-9]*)
        if [ -n "$execute_review" ]; then
            die "--execute-review must be a positive numeric review ID"
        fi
        ;;
    *)
        [ "$execute_review" -gt 0 ] ||
            die "--execute-review must be a positive numeric review ID"
        ;;
esac

platform=$(uname -s)
case "$platform" in
    Darwin|Linux) ;;
    *) die "unsupported platform: $platform" ;;
esac

if [ -n "${CAR_GO_CLEAN_UPGRADE_STATE_DIR-}" ]; then
    state_dir=$CAR_GO_CLEAN_UPGRADE_STATE_DIR
elif [ -n "${XDG_STATE_HOME-}" ]; then
    state_dir=$XDG_STATE_HOME/car-go-clean
else
    [ -n "${HOME-}" ] || die "HOME is required"
    state_dir=$HOME/.local/state/car-go-clean
fi
case "$state_dir" in
    /*) ;;
    *) die "upgrade state directory must be absolute: $state_dir" ;;
esac

session_file=$state_dir/upgrade-session
session_lock=$state_dir/upgrade-session.lock
service_definition_backup=$state_dir/upgrade-service-definition
work_dir=
session_temp=
definition_temp=
lock_held=false
rollback_armed=false
replacement_recovery_armed=false
original_state=
cgc_binary=
session_version=
session_method=
session_old_version=
session_state=
session_phase=
session_review=
session_binary_path=
session_binary_sha256=
session_old_binary_path=
session_definition_backup_sha256=
session_definition_binary_path=
session_refreshed_definition_sha256=
session_refreshed_definition_binary_path=
carriage_return=$(printf '\r')

validate_line_value() {
    case "$1" in
        *'
'*) return 1 ;;
    esac
    case "$1" in
        *"$carriage_return"*) return 1 ;;
    esac
}

validate_absolute_path_value() {
    validate_line_value "$1" || return 1
    case "$1" in
        /*) ;;
        *) return 1 ;;
    esac
}

path_has_no_symlink_components() {
    candidate_path=$1
    validate_absolute_path_value "$candidate_path" || return 1
    remaining_path=${candidate_path#/}
    checked_path=
    while [ -n "$remaining_path" ]; do
        case "$remaining_path" in
            */*)
                path_component=${remaining_path%%/*}
                remaining_path=${remaining_path#*/}
                ;;
            *)
                path_component=$remaining_path
                remaining_path=
                ;;
        esac
        case "$path_component" in
            ''|.|..) return 1 ;;
        esac
        checked_path=$checked_path/$path_component
        [ ! -L "$checked_path" ] || return 1
    done
}

portable_file_metadata() {
    metadata_path=$1
    metadata=$(stat -f '%u:%Lp:%d:%i:%z:%m' -- "$metadata_path" 2>/dev/null || :)
    case "$metadata" in
        [0-9]*:[0-7]*:[0-9]*:[0-9]*:[0-9]*:[0-9]*)
            printf '%s\n' "$metadata"
            return 0
            ;;
    esac
    metadata=$(stat -c '%u:%a:%d:%i:%s:%Y' -- "$metadata_path" 2>/dev/null || :)
    case "$metadata" in
        [0-9]*:[0-7]*:[0-9]*:[0-9]*:[0-9]*:[0-9]*)
            printf '%s\n' "$metadata"
            return 0
            ;;
    esac
    return 1
}

sha256_file() {
    checksum_path=$1
    case "$platform" in
        Darwin)
            checksum_output=$(shasum -a 256 "$checksum_path" 2>/dev/null) ||
                return 1
            ;;
        Linux)
            checksum_output=$(sha256sum "$checksum_path" 2>/dev/null) ||
                return 1
            ;;
    esac
    checksum=$(printf '%s\n' "$checksum_output" |
        awk 'NR == 1 { value = $1 } END { if (NR != 1) exit 1; print value }') ||
        return 1
    case "$checksum" in
        *[!0-9a-f]*|'') return 1 ;;
    esac
    [ "${#checksum}" -eq 64 ] || return 1
    printf '%s\n' "$checksum"
}

validate_secure_state_dir() {
    [ ! -L "$state_dir" ] && [ -d "$state_dir" ] ||
        die "upgrade state path is not a secure directory: $state_dir"
    path_has_no_symlink_components "$state_dir" ||
        die "upgrade state directory must not contain a symlink component: $state_dir"
    state_metadata=$(portable_file_metadata "$state_dir") ||
        die "could not inspect upgrade state directory ownership and permissions"
    state_owner=${state_metadata%%:*}
    state_metadata_rest=${state_metadata#*:}
    state_mode=${state_metadata_rest%%:*}
    state_metadata_rest=${state_metadata_rest#*:}
    state_device=${state_metadata_rest%%:*}
    state_metadata_rest=${state_metadata_rest#*:}
    state_inode=${state_metadata_rest%%:*}
    state_metadata_rest=${state_metadata_rest#*:}
    state_size=${state_metadata_rest%%:*}
    state_mtime=${state_metadata_rest#*:}
    [ "$state_owner:$state_mode:$state_device:$state_inode:$state_size:$state_mtime" = "$state_metadata" ] ||
        die "could not inspect upgrade state directory ownership and permissions"
    for metadata_field in "$state_owner" "$state_mode" "$state_device" \
        "$state_inode" "$state_size" "$state_mtime"; do
        case "$metadata_field" in
            ''|*[!0-9]*)
                die "could not inspect upgrade state directory ownership and permissions"
                ;;
            esac
    done
    current_uid=$(id -u) ||
        die "could not determine the current user for upgrade state validation"
    [ "$state_owner" = "$current_uid" ] ||
        die "upgrade state directory must be owned by the current user: $state_dir"
    case "$state_mode" in
        0[0-7][0-7][0-7]) state_mode=${state_mode#0} ;;
        [0-7][0-7][0-7]) ;;
        *) die "upgrade state directory has unsupported permissions: $state_mode" ;;
    esac
    case "$state_mode" in
        ?[2367]?|??[2367])
            die "upgrade state directory must not be group/world-writable: $state_dir"
            ;;
    esac
}

quote_shell_word() {
    printf "'"
    printf '%s' "$1" | sed "s/'/'\\\\''/g"
    printf "'"
}

canonical_existing_binary() (
    candidate=$1
    validate_absolute_path_value "$candidate" || exit 1
    links=0
    while :; do
        parent=$(CDPATH='' cd -P "$(dirname "$candidate")" 2>/dev/null && pwd -P) ||
            exit 1
        candidate=$parent/$(basename "$candidate")
        if [ ! -L "$candidate" ]; then
            break
        fi
        links=$((links + 1))
        [ "$links" -le 40 ] || exit 1
        target=$(readlink "$candidate") || exit 1
        validate_line_value "$target" || exit 1
        case "$target" in
            /*) ;;
            *) target=$(dirname "$candidate")/$target ;;
        esac
        candidate=$target
    done
    [ -f "$candidate" ] && [ -x "$candidate" ] || exit 1
    printf '%s\n' "$candidate"
)

installed_homebrew_binary() {
    command -v brew >/dev/null 2>&1 || return 1
    brew list --versions car-go-clean >/dev/null 2>&1 || return 1
    formula_prefix=$(brew --prefix car-go-clean 2>/dev/null) || return 1
    validate_absolute_path_value "$formula_prefix" || return 1
    canonical_existing_binary "$formula_prefix/bin/car-go-clean"
}

cleanup_temporary_files() {
    if [ -n "$session_temp" ] && [ -e "$session_temp" ]; then
        rm -f "$session_temp"
    fi
    if [ -n "$definition_temp" ] && [ -e "$definition_temp" ]; then
        rm -f "$definition_temp"
    fi
    if [ -n "$work_dir" ] && [ -d "$work_dir" ]; then
        rm -rf "$work_dir"
    fi
    if [ "$lock_held" = true ]; then
        rmdir "$session_lock" 2>/dev/null || :
        lock_held=false
    fi
}

installed_service_definition() {
    case "$platform" in
        Darwin)
            printf '%s\n' "$HOME/Library/LaunchAgents/com.dcchuck.car-go-clean.plist"
            ;;
        Linux)
            printf '%s\n' "$HOME/.config/systemd/user/car-go-clean.service"
            ;;
    esac
}

backup_installed_service_definition() {
    definition_path=$(installed_service_definition) || return 1
    [ ! -L "$definition_path" ] && [ -f "$definition_path" ] || {
        echo "car-go-clean upgrade: installed service definition is unavailable or unsafe: $definition_path" >&2
        return 1
    }
    definition_temp=$(mktemp "$state_dir/.upgrade-service-definition.XXXXXX") || {
        echo "car-go-clean upgrade: could not create service-definition backup" >&2
        return 1
    }
    chmod 600 "$definition_temp" || return 1
    cp "$definition_path" "$definition_temp" || return 1
    chmod 600 "$definition_temp" || return 1
    mv -f "$definition_temp" "$service_definition_backup" || return 1
    definition_temp=
}

extract_launchd_definition_binary() {
    awk '
        function decode_xml(value, output, character) {
            output = ""
            while (length(value) > 0) {
                character = substr(value, 1, 1)
                if (character != "&") {
                    output = output character
                    value = substr(value, 2)
                } else if (substr(value, 1, 5) == "&amp;") {
                    output = output "&"
                    value = substr(value, 6)
                } else if (substr(value, 1, 4) == "&lt;") {
                    output = output "<"
                    value = substr(value, 5)
                } else if (substr(value, 1, 4) == "&gt;") {
                    output = output ">"
                    value = substr(value, 5)
                } else if (substr(value, 1, 6) == "&quot;") {
                    output = output "\""
                    value = substr(value, 7)
                } else if (substr(value, 1, 6) == "&apos;") {
                    output = output "\047"
                    value = substr(value, 7)
                } else {
                    parse_error = 1
                    return ""
                }
            }
            return output
        }

        {
            line = $0
            sub(/^[ \t]*/, "", line)
            sub(/[ \t]*$/, "", line)
            if (line == "<key>ProgramArguments</key>") {
                key_count++
                if (key_count != 1 || waiting_for_array || in_array) {
                    parse_error = 1
                }
                waiting_for_array = 1
                next
            }
            if (waiting_for_array) {
                if (line == "") {
                    next
                }
                if (line != "<array>") {
                    parse_error = 1
                } else {
                    in_array = 1
                }
                waiting_for_array = 0
                next
            }
            if (in_array && !found_binary) {
                if (line == "") {
                    next
                }
                if (line !~ /^<string>.*<\/string>$/) {
                    parse_error = 1
                    next
                }
                sub(/^<string>/, "", line)
                sub(/<\/string>$/, "", line)
                binary = decode_xml(line)
                found_binary = 1
                next
            }
            if (in_array && found_binary && line == "</array>") {
                in_array = 0
                closed_array = 1
            }
        }

        END {
            if (!parse_error && key_count == 1 && found_binary && closed_array) {
                print binary
            } else {
                exit 1
            }
        }
    ' "$1"
}

extract_systemd_definition_binary() {
    awk '
        function decode_exec_start(value, output, position, character, escaped, rest) {
            if (substr(value, 1, 1) != "\"") {
                parse_error = 1
                return ""
            }
            output = ""
            position = 2
            while (position <= length(value)) {
                character = substr(value, position, 1)
                if (character == "\"") {
                    rest = substr(value, position + 1)
                    if (rest !~ /^[ \t]+daemon[ \t]*$/) {
                        parse_error = 1
                    }
                    return output
                }
                if (character == "\\") {
                    escaped = substr(value, position + 1, 1)
                    if (escaped != "\\" && escaped != "\"") {
                        parse_error = 1
                        return ""
                    }
                    output = output escaped
                    position += 2
                } else if (character == "%") {
                    if (substr(value, position + 1, 1) != "%") {
                        parse_error = 1
                        return ""
                    }
                    output = output "%"
                    position += 2
                } else {
                    output = output character
                    position++
                }
            }
            parse_error = 1
            return ""
        }

        /^[ \t]*ExecStart=/ {
            exec_start_count++
            value = $0
            sub(/^[ \t]*ExecStart=/, "", value)
            binary = decode_exec_start(value)
        }

        END {
            if (!parse_error && exec_start_count == 1) {
                print binary
            } else {
                exit 1
            }
        }
    ' "$1"
}

extract_service_definition_binary() {
    case "$platform" in
        Darwin) extract_launchd_definition_binary "$1" ;;
        Linux) extract_systemd_definition_binary "$1" ;;
    esac
}

authenticate_service_definition() {
    definition_path=$1
    expected_digest=$2
    [ ! -L "$definition_path" ] && [ -f "$definition_path" ] || return 1
    definition_metadata_before=$(portable_file_metadata "$definition_path") ||
        return 1
    definition_digest_before=$(sha256_file "$definition_path") || return 1
    [ "$definition_digest_before" = "$expected_digest" ] || return 1
    definition_binary=$(extract_service_definition_binary "$definition_path") ||
        return 1
    validate_absolute_path_value "$definition_binary" || return 1
    resolved_definition_binary=$(canonical_existing_binary "$definition_binary") ||
        return 1
    definition_digest_after=$(sha256_file "$definition_path") || return 1
    definition_metadata_after=$(portable_file_metadata "$definition_path") ||
        return 1
    [ ! -L "$definition_path" ] && [ -f "$definition_path" ] || return 1
    [ "$definition_digest_after" = "$expected_digest" ] || return 1
    [ "$definition_metadata_after" = "$definition_metadata_before" ] || return 1
    printf '%s\n' "$resolved_definition_binary"
}

authenticate_replacement_binary() {
    binary_path=$1
    expected_digest=$2
    binary_version=
    resolved_binary=$(canonical_existing_binary "$binary_path") || return 1
    [ "$resolved_binary" = "$binary_path" ] || return 1
    binary_metadata_before=$(portable_file_metadata "$binary_path") || return 1
    binary_digest_before=$(sha256_file "$binary_path") || return 1
    [ "$binary_digest_before" = "$expected_digest" ] || return 1
    binary_version=$("$binary_path" version 2>&1) || return 1
    [ "$binary_version" = 0.4.0 ] || return 1
    binary_digest_after=$(sha256_file "$binary_path") || return 1
    binary_metadata_after=$(portable_file_metadata "$binary_path") || return 1
    [ "$binary_digest_after" = "$expected_digest" ] || return 1
    [ "$binary_metadata_after" = "$binary_metadata_before" ] || return 1
}

validate_final_artifacts() {
    authenticate_replacement_binary \
        "$session_binary_path" "$session_binary_sha256" || {
        echo "car-go-clean upgrade: replacement binary changed before service convergence" >&2
        return 1
    }
    if [ "$session_state" != absent ]; then
        definition_path=$(installed_service_definition) || return 1
        refreshed_definition_binary=$(
            authenticate_service_definition \
                "$definition_path" "$session_refreshed_definition_sha256"
        ) || {
            echo "car-go-clean upgrade: refreshed service definition changed before service convergence" >&2
            return 1
        }
        [ "$refreshed_definition_binary" = \
            "$session_refreshed_definition_binary_path" ] &&
            [ "$refreshed_definition_binary" = "$session_binary_path" ] || {
            echo "car-go-clean upgrade: refreshed service definition no longer resolves to the authenticated replacement binary" >&2
            return 1
        }
    fi
}

launchd_target() {
    uid=$(id -u) || return 1
    printf 'gui/%s/com.dcchuck.car-go-clean\n' "$uid"
}

disable_installed_service() {
    case "$platform" in
        Darwin)
            target=$(launchd_target) || return 1
            launchctl disable "$target" &&
                if [ "$original_state" = active ]; then
                    launchctl bootout "$target"
                fi
            ;;
        Linux)
            systemctl --user disable --now car-go-clean.service
            ;;
    esac
}

restore_active_service() {
    case "$platform" in
        Darwin)
            target=$(launchd_target) || return 1
            launchctl enable "$target" &&
                launchctl bootstrap "gui/$(id -u)" \
                "$HOME/Library/LaunchAgents/com.dcchuck.car-go-clean.plist" &&
                launchctl kickstart -k "$target"
            ;;
        Linux)
            systemctl --user enable --now car-go-clean.service
            ;;
    esac
}

service_activity_state() {
    case "$platform" in
        Darwin)
            target=$(launchd_target) || {
                printf 'error\n'
                return 0
            }
            if activity_output=$(launchctl print "$target" 2>&1); then
                printf 'active\n'
                return 0
            else
                activity_status=$?
            fi
            case "$activity_status:$activity_output" in
                113:*"Could not find specified service"*|\
                113:*"Could not find service \"com.dcchuck.car-go-clean\""*|\
                113:*"Service not found"*|\
                113:*"No such process"*)
                    printf 'inactive\n'
                    ;;
                *)
                    echo "car-go-clean upgrade: launchctl activity query failed: $activity_output" >&2
                    printf 'error\n'
                    ;;
            esac
            ;;
        Linux)
            if activity_output=$(
                systemctl --user is-active car-go-clean.service 2>&1
            ); then
                activity_status=0
            else
                activity_status=$?
            fi
            case "$activity_status:$activity_output" in
                0:active)
                    printf 'active\n'
                    ;;
                3:inactive)
                    printf 'inactive\n'
                    ;;
                *)
                    echo "car-go-clean upgrade: systemctl activity query failed: $activity_output" >&2
                    printf 'error\n'
                    ;;
            esac
            ;;
    esac
}

service_enabled_state() {
    case "$platform" in
        Darwin)
            uid=$(id -u) || return 1
            disabled_services=$(launchctl print-disabled "gui/$uid" 2>/dev/null) ||
                return 1
            case "$disabled_services" in
                *'"com.dcchuck.car-go-clean" => true'*)
                    printf 'disabled\n'
                    ;;
                *)
                    printf 'enabled\n'
                    ;;
            esac
            ;;
        Linux)
            if enabled_output=$(systemctl --user is-enabled car-go-clean.service 2>/dev/null); then
                [ "$enabled_output" = enabled ] || return 1
                printf 'enabled\n'
            else
                [ "$enabled_output" = disabled ] || return 1
                printf 'disabled\n'
            fi
            ;;
    esac
}

enable_service_only() {
    case "$platform" in
        Darwin)
            target=$(launchd_target) || return 1
            launchctl enable "$target"
            ;;
        Linux)
            systemctl --user enable car-go-clean.service
            ;;
    esac
}

disable_service_only() {
    case "$platform" in
        Darwin)
            target=$(launchd_target) || return 1
            launchctl disable "$target"
            ;;
        Linux)
            systemctl --user disable car-go-clean.service
            ;;
    esac
}

start_service_only() {
    case "$platform" in
        Darwin)
            uid=$(id -u) || return 1
            target=$(launchd_target) || return 1
            launchctl bootstrap "gui/$uid" \
                "$HOME/Library/LaunchAgents/com.dcchuck.car-go-clean.plist" &&
                launchctl kickstart -k "$target"
            ;;
        Linux)
            systemctl --user start car-go-clean.service
            ;;
    esac
}

stop_service_only() {
    case "$platform" in
        Darwin)
            target=$(launchd_target) || return 1
            launchctl bootout "$target"
            ;;
        Linux)
            systemctl --user stop car-go-clean.service
            ;;
    esac
}

keep_service_disabled_and_stopped() {
    case "$platform" in
        Darwin)
            target=$(launchd_target) || return 1
            launchctl disable "$target" >/dev/null 2>&1 || :
            launchctl bootout "$target" >/dev/null 2>&1 || :
            ;;
        Linux)
            systemctl --user disable --now car-go-clean.service \
                >/dev/null 2>&1 || :
            ;;
    esac
}

converge_final_service_state() {
    if [ "$session_state" = absent ]; then
        validate_final_artifacts
        return
    fi

    enabled_state=$(service_enabled_state) || return 1
    activity_state=$(service_activity_state)
    [ "$activity_state" != error ] || return 1
    validate_final_artifacts || return 1

    case "$session_state" in
        active)
            if [ "$enabled_state" = disabled ] &&
                [ "$activity_state" = inactive ]; then
                restore_active_service || return 1
            else
                if [ "$enabled_state" = disabled ]; then
                    enable_service_only || return 1
                fi
                if [ "$activity_state" = inactive ]; then
                    start_service_only || return 1
                fi
            fi
            final_enabled_state=$(service_enabled_state) || return 1
            final_activity_state=$(service_activity_state)
            [ "$final_enabled_state" = enabled ] &&
                [ "$final_activity_state" = active ]
            ;;
        stopped)
            if [ "$enabled_state" = enabled ]; then
                disable_service_only || return 1
            fi
            if [ "$activity_state" = active ]; then
                stop_service_only || return 1
            fi
            final_enabled_state=$(service_enabled_state) || return 1
            final_activity_state=$(service_activity_state)
            [ "$final_enabled_state" = disabled ] &&
                [ "$final_activity_state" = inactive ]
            ;;
    esac
}

recover_exact_old_active_service() {
    [ "$session_state" = active ] || return 1
    resolved_old_binary=$(canonical_existing_binary "$session_old_binary_path") ||
        return 1
    [ "$resolved_old_binary" = "$session_old_binary_path" ] || return 1
    recovered_old_version=$("$resolved_old_binary" version 2>&1) || return 1
    [ "$recovered_old_version" = "$session_old_version" ] || return 1

    recovery_enabled_state=$(service_enabled_state) || return 1
    recovery_activity_state=$(service_activity_state)
    [ "$recovery_activity_state" != error ] || return 1

    definition_path=$(installed_service_definition) || return 1
    recovered_definition_binary=$(
        authenticate_service_definition \
            "$definition_path" "$session_definition_backup_sha256"
    ) || return 1
    [ "$recovered_definition_binary" = "$session_definition_binary_path" ] ||
        return 1
    [ "$recovered_definition_binary" = "$session_old_binary_path" ] || return 1

    if [ "$recovery_enabled_state" = disabled ]; then
        enable_service_only || return 1
        recovered_definition_binary=$(
            authenticate_service_definition \
                "$definition_path" "$session_definition_backup_sha256"
        ) || return 1
        [ "$recovered_definition_binary" = "$session_definition_binary_path" ] ||
            return 1
        [ "$recovered_definition_binary" = "$session_old_binary_path" ] ||
            return 1
    fi
    if [ "$recovery_activity_state" = inactive ]; then
        start_service_only || return 1
    fi
    [ "$(service_enabled_state)" = enabled ] || return 1
    recovered_activity_state=$(service_activity_state)
    [ "$recovered_activity_state" = active ]
}

on_exit() {
    status=$?
    trap - 0 HUP INT TERM
    set +e
    if [ "$status" -ne 0 ] &&
        [ "$replacement_recovery_armed" = true ]; then
        if recover_exact_old_active_service; then
            echo "Upgrade replacement failed; exact car-go-clean $session_old_version was validated at $session_old_binary_path and the previously active service was restored." >&2
            rm -f "$session_file" "$service_definition_backup"
        else
            keep_service_disabled_and_stopped >/dev/null 2>&1 || :
            replacement_recovery_guidance
        fi
    elif [ "$status" -ne 0 ] &&
        [ "$rollback_armed" = true ] &&
        [ "$original_state" = active ]; then
        if recover_exact_old_active_service; then
            echo "Upgrade failed before replacement; exact car-go-clean $session_old_version was validated at $session_old_binary_path and the previously active service was restored." >&2
            rm -f "$session_file" "$service_definition_backup"
        else
            keep_service_disabled_and_stopped >/dev/null 2>&1 || :
            pre_replacement_recovery_guidance
        fi
    fi
    cleanup_temporary_files
    exit "$status"
}

trap on_exit 0
trap 'exit 129' HUP
trap 'exit 130' INT
trap 'exit 143' TERM

prepare_state_dir() {
    path_has_no_symlink_components "$state_dir" ||
        die "upgrade state directory must not contain a symlink component: $state_dir"
    umask 077
    mkdir -p "$state_dir"
    validate_secure_state_dir
}

acquire_session_lock() {
    if mkdir "$session_lock" 2>/dev/null; then
        lock_held=true
        return 0
    fi
    die "another car-go-clean upgrade is already in progress; if no helper is running, inspect and remove $session_lock"
}

session_mode() {
    mode=$(stat -f '%Lp' "$session_file" 2>/dev/null || :)
    case "$mode" in
        [0-7][0-7][0-7])
            printf '%s\n' "$mode"
            return 0
            ;;
    esac
    mode=$(stat -c '%a' "$session_file" 2>/dev/null || :)
    case "$mode" in
        [0-7][0-7][0-7])
            printf '%s\n' "$mode"
            return 0
            ;;
    esac
    return 1
}

migrate_format6_session() {
    migrated_binary_sha256=$(sha256_file "$session_binary_path") || {
        echo "car-go-clean upgrade: could not fingerprint the format-6 replacement binary" >&2
        return 1
    }
    authenticate_replacement_binary \
        "$session_binary_path" "$migrated_binary_sha256" || {
        echo "car-go-clean upgrade: could not authenticate the format-6 replacement binary" >&2
        return 1
    }

    if [ "$session_state" != absent ]; then
        migrated_backup_binary=$(
            authenticate_service_definition \
                "$service_definition_backup" \
                "$session_definition_backup_sha256"
        ) || {
            echo "car-go-clean upgrade: could not authenticate the format-6 service-definition backup" >&2
            return 1
        }
        [ "$migrated_backup_binary" = "$session_definition_binary_path" ] || {
            echo "car-go-clean upgrade: format-6 service-definition backup no longer resolves to its recorded binary" >&2
            return 1
        }
    fi

    case "$session_phase:$session_state" in
        replacement_pending:*|definition_pending:*)
            migrated_definition_sha256=none
            migrated_definition_binary=none
            ;;
        preview_pending:absent|review_pending:absent|executing:absent|executed:absent)
            migrated_definition_sha256=none
            migrated_definition_binary=none
            ;;
        preview_pending:*|review_pending:*|executing:*|executed:*)
            migrated_definition_path=$(installed_service_definition) || return 1
            migrated_definition_sha256=$(
                sha256_file "$migrated_definition_path"
            ) || {
                echo "car-go-clean upgrade: could not fingerprint the format-6 refreshed service definition" >&2
                return 1
            }
            migrated_definition_binary=$(
                authenticate_service_definition \
                    "$migrated_definition_path" \
                    "$migrated_definition_sha256"
            ) || {
                echo "car-go-clean upgrade: could not authenticate the format-6 refreshed service definition" >&2
                return 1
            }
            [ "$migrated_definition_binary" = "$session_binary_path" ] || {
                echo "car-go-clean upgrade: format-6 refreshed service definition does not resolve to the replacement binary" >&2
                return 1
            }
            ;;
        *)
            return 1
            ;;
    esac

    session_binary_sha256=$migrated_binary_sha256
    session_refreshed_definition_sha256=$migrated_definition_sha256
    session_refreshed_definition_binary_path=$migrated_definition_binary
    write_session "$session_phase" "$session_review"
    session_format=7
    echo "Authenticated and migrated the resumable upgrade session from format 6 to format 7." >&2
}

load_session() {
    [ ! -L "$session_file" ] ||
        die "upgrade session must not be a symlink: $session_file"
    [ -f "$session_file" ] ||
        die "no resumable upgrade session exists; run the preview phase first"
    mode=$(session_mode) ||
        die "could not inspect upgrade session permissions"
    [ "$mode" = 600 ] ||
        die "upgrade session must have mode 0600 (found $mode)"

    session_format=
    session_version=
    session_method=
    session_old_version=
    session_state=
    session_phase=
    session_review=
    session_binary_path=
    session_binary_sha256=
    session_old_binary_path=
    session_definition_backup_sha256=
    session_definition_binary_path=
    session_refreshed_definition_sha256=
    session_refreshed_definition_binary_path=
    seen_format=false
    seen_version=false
    seen_method=false
    seen_old_version=false
    seen_state=false
    seen_phase=false
    seen_review=false
    seen_binary_path=false
    seen_binary_sha256=false
    seen_old_binary_path=false
    seen_definition_backup_sha256=false
    seen_definition_binary_path=false
    seen_refreshed_definition_sha256=false
    seen_refreshed_definition_binary_path=false
    malformed=false
    while IFS= read -r line || [ -n "$line" ]; do
        case "$line" in
            format=*)
                [ "$seen_format" = false ] || malformed=true
                seen_format=true
                session_format=${line#format=}
                ;;
            version=*)
                [ "$seen_version" = false ] || malformed=true
                seen_version=true
                session_version=${line#version=}
                ;;
            method=*)
                [ "$seen_method" = false ] || malformed=true
                seen_method=true
                session_method=${line#method=}
                ;;
            old_version=*)
                [ "$seen_old_version" = false ] || malformed=true
                seen_old_version=true
                session_old_version=${line#old_version=}
                ;;
            service_state=*)
                [ "$seen_state" = false ] || malformed=true
                seen_state=true
                session_state=${line#service_state=}
                ;;
            phase=*)
                [ "$seen_phase" = false ] || malformed=true
                seen_phase=true
                session_phase=${line#phase=}
                ;;
            review_id=*)
                [ "$seen_review" = false ] || malformed=true
                seen_review=true
                session_review=${line#review_id=}
                ;;
            binary_path=*)
                [ "$seen_binary_path" = false ] || malformed=true
                seen_binary_path=true
                session_binary_path=${line#binary_path=}
                ;;
            binary_sha256=*)
                [ "$seen_binary_sha256" = false ] || malformed=true
                seen_binary_sha256=true
                session_binary_sha256=${line#binary_sha256=}
                ;;
            old_binary_path=*)
                [ "$seen_old_binary_path" = false ] || malformed=true
                seen_old_binary_path=true
                session_old_binary_path=${line#old_binary_path=}
                ;;
            definition_backup_sha256=*)
                [ "$seen_definition_backup_sha256" = false ] || malformed=true
                session_definition_backup_sha256=${line#definition_backup_sha256=}
                seen_definition_backup_sha256=true
                ;;
            definition_binary_path=*)
                [ "$seen_definition_binary_path" = false ] || malformed=true
                session_definition_binary_path=${line#definition_binary_path=}
                seen_definition_binary_path=true
                ;;
            refreshed_definition_sha256=*)
                [ "$seen_refreshed_definition_sha256" = false ] || malformed=true
                session_refreshed_definition_sha256=${line#refreshed_definition_sha256=}
                seen_refreshed_definition_sha256=true
                ;;
            refreshed_definition_binary_path=*)
                [ "$seen_refreshed_definition_binary_path" = false ] || malformed=true
                session_refreshed_definition_binary_path=${line#refreshed_definition_binary_path=}
                seen_refreshed_definition_binary_path=true
                ;;
            *)
                malformed=true
                ;;
        esac
    done < "$session_file"

    [ "$malformed" = false ] &&
        [ "$seen_format" = true ] &&
        [ "$seen_version" = true ] &&
        [ "$seen_method" = true ] &&
        [ "$seen_old_version" = true ] &&
        [ "$seen_state" = true ] &&
        [ "$seen_phase" = true ] &&
        [ "$seen_review" = true ] &&
        [ "$seen_binary_path" = true ] &&
        [ "$seen_old_binary_path" = true ] &&
        [ "$seen_definition_backup_sha256" = true ] &&
        [ "$seen_definition_binary_path" = true ] ||
        die "upgrade session is malformed"
    case "$session_format" in
        6)
            [ "$seen_binary_sha256" = false ] &&
                [ "$seen_refreshed_definition_sha256" = false ] &&
                [ "$seen_refreshed_definition_binary_path" = false ] ||
                die "upgrade session is malformed"
            session_binary_sha256=unresolved
            session_refreshed_definition_sha256=none
            session_refreshed_definition_binary_path=none
            ;;
        7)
            [ "$seen_binary_sha256" = true ] &&
                [ "$seen_refreshed_definition_sha256" = true ] &&
                [ "$seen_refreshed_definition_binary_path" = true ] ||
                die "upgrade session is malformed"
            ;;
        *)
            die "upgrade session is malformed"
            ;;
    esac
    [ "$session_version" = 0.4.0 ] || die "upgrade session is malformed"
    case "$session_method" in homebrew|shell) ;; *) die "upgrade session is malformed" ;; esac
    case "$session_old_version" in
        0.2.0|0.3.0|absent) ;;
        *) die "upgrade session is malformed" ;;
    esac
    case "$session_state" in active|stopped|absent) ;; *) die "upgrade session is malformed" ;; esac
    case "$session_state:$session_definition_backup_sha256" in
        absent:none) ;;
        active:*|stopped:*)
            case "$session_definition_backup_sha256" in
                *[!0-9a-f]*|'') die "upgrade session is malformed" ;;
            esac
            [ "${#session_definition_backup_sha256}" -eq 64 ] ||
                die "upgrade session is malformed"
            ;;
        *)
            die "upgrade session is malformed"
            ;;
    esac
    case "$session_state:$session_definition_binary_path" in
        absent:none) ;;
        active:*|stopped:*)
            validate_absolute_path_value "$session_definition_binary_path" ||
                die "upgrade session is malformed"
            ;;
        *)
            die "upgrade session is malformed"
            ;;
    esac
    validate_absolute_path_value "$session_old_binary_path" ||
        die "upgrade session is malformed"
    case "$session_phase" in
        replacement_attempt)
            [ "$session_binary_path" = unresolved ] ||
                die "upgrade session is malformed"
            [ "$session_binary_sha256" = unresolved ] ||
                die "upgrade session is malformed"
            [ "$session_review" = none ] || die "upgrade session is malformed"
            ;;
        replacement_pending|definition_pending|preview_pending)
            validate_absolute_path_value "$session_binary_path" ||
                die "upgrade session is malformed"
            if [ "$session_format" = 7 ]; then
                case "$session_binary_sha256" in
                    *[!0-9a-f]*|'') die "upgrade session is malformed" ;;
                esac
                [ "${#session_binary_sha256}" -eq 64 ] ||
                    die "upgrade session is malformed"
            fi
            [ "$session_review" = none ] || die "upgrade session is malformed"
            ;;
        review_pending|executing|executed)
            validate_absolute_path_value "$session_binary_path" ||
                die "upgrade session is malformed"
            if [ "$session_format" = 7 ]; then
                case "$session_binary_sha256" in
                    *[!0-9a-f]*|'') die "upgrade session is malformed" ;;
                esac
                [ "${#session_binary_sha256}" -eq 64 ] ||
                    die "upgrade session is malformed"
            fi
            case "$session_review" in
                ''|*[!0-9]*) die "upgrade session is malformed" ;;
                *) [ "$session_review" -gt 0 ] || die "upgrade session is malformed" ;;
            esac
            ;;
        *)
            die "upgrade session is malformed"
            ;;
    esac
    if [ "$session_phase" != replacement_attempt ]; then
        resolved_session_binary=$(canonical_existing_binary "$session_binary_path") ||
            die "upgrade session binary path is unavailable or unsafe"
        [ "$resolved_session_binary" = "$session_binary_path" ] ||
            die "upgrade session binary path is no longer exact"
    fi
    if [ "$session_format" = 6 ] &&
        [ "$session_phase" != replacement_attempt ]; then
        if ! migrate_format6_session; then
            if [ "$session_state" != absent ]; then
                keep_service_disabled_and_stopped >/dev/null 2>&1 || :
            fi
            die "format-6 session artifacts could not be authenticated; the session remains unchanged for manual recovery"
        fi
    fi
    if [ "$session_format" = 7 ]; then
        case "$session_phase:$session_state" in
            replacement_pending:*|definition_pending:*)
                [ "$session_refreshed_definition_sha256" = none ] &&
                    [ "$session_refreshed_definition_binary_path" = none ] ||
                    die "upgrade session is malformed"
                ;;
            preview_pending:absent|review_pending:absent|executing:absent|executed:absent)
                [ "$session_refreshed_definition_sha256" = none ] &&
                    [ "$session_refreshed_definition_binary_path" = none ] ||
                    die "upgrade session is malformed"
                ;;
            preview_pending:*|review_pending:*|executing:*|executed:*)
                case "$session_refreshed_definition_sha256" in
                    *[!0-9a-f]*|'') die "upgrade session is malformed" ;;
                esac
                [ "${#session_refreshed_definition_sha256}" -eq 64 ] ||
                    die "upgrade session is malformed"
                validate_absolute_path_value \
                    "$session_refreshed_definition_binary_path" ||
                    die "upgrade session is malformed"
                ;;
            replacement_attempt:*)
                ;;
        esac
    fi
}

write_session() {
    next_phase=$1
    next_review=$2
    prepare_state_dir
    session_temp=$(mktemp "$state_dir/.upgrade-session.XXXXXX") ||
        die "could not create upgrade session"
    chmod 600 "$session_temp" ||
        die "could not secure upgrade session"
    if ! {
        printf 'format=7\n'
        printf 'version=%s\n' "$session_version"
        printf 'method=%s\n' "$session_method"
        printf 'old_version=%s\n' "$session_old_version"
        printf 'service_state=%s\n' "$session_state"
        printf 'phase=%s\n' "$next_phase"
        printf 'review_id=%s\n' "$next_review"
        printf 'binary_path=%s\n' "$session_binary_path"
        printf 'binary_sha256=%s\n' "$session_binary_sha256"
        printf 'old_binary_path=%s\n' "$session_old_binary_path"
        printf 'definition_backup_sha256=%s\n' "$session_definition_backup_sha256"
        printf 'definition_binary_path=%s\n' "$session_definition_binary_path"
        printf 'refreshed_definition_sha256=%s\n' \
            "$session_refreshed_definition_sha256"
        printf 'refreshed_definition_binary_path=%s\n' \
            "$session_refreshed_definition_binary_path"
    } > "$session_temp"; then
        die "could not write upgrade session"
    fi
    mv -f "$session_temp" "$session_file" ||
        die "could not publish upgrade session"
    session_temp=
    session_phase=$next_phase
    session_review=$next_review
}

validate_resumed_binary() {
    cgc_binary=$session_binary_path
    authenticate_replacement_binary \
        "$session_binary_path" "$session_binary_sha256" || {
        if [ -n "$binary_version" ] && [ "$binary_version" != 0.4.0 ]; then
            echo "car-go-clean upgrade: expected car-go-clean 0.4.0, found $binary_version" >&2
        else
            echo "car-go-clean upgrade: could not validate the replacement car-go-clean binary" >&2
        fi
        return 1
    }
}

# shellcheck disable=SC2016 # These helpers are emitted for the operator's rollback shell.
print_secure_definition_restore_helpers() {
    echo 'secure_restore_no_symlink_components() (' >&2
    echo '    secure_candidate=$1' >&2
    echo '    case "$secure_candidate" in /*) ;; *) exit 1 ;; esac' >&2
    echo '    secure_remaining=${secure_candidate#/}' >&2
    echo '    secure_checked=' >&2
    echo '    while [ -n "$secure_remaining" ]; do' >&2
    echo '        case "$secure_remaining" in' >&2
    echo '            */*) secure_component=${secure_remaining%%/*}; secure_remaining=${secure_remaining#*/} ;;' >&2
    echo '            *) secure_component=$secure_remaining; secure_remaining= ;;' >&2
    echo '        esac' >&2
    echo '        case "$secure_component" in ""|"."|"..") exit 1 ;; esac' >&2
    echo '        secure_checked=$secure_checked/$secure_component' >&2
    echo '        [ ! -L "$secure_checked" ] || exit 1' >&2
    echo '    done' >&2
    echo ')' >&2
    echo 'secure_restore_metadata() (' >&2
    echo '    secure_path=$1' >&2
    echo '    secure_restore_no_symlink_components "$secure_path" || exit 1' >&2
    echo '    [ ! -L "$secure_path" ] && [ -f "$secure_path" ] || exit 1' >&2
    echo '    secure_metadata=$(stat -f "%u:%Lp:%d:%i:%z:%m" -- "$secure_path" 2>/dev/null || :)' >&2
    echo '    case "$secure_metadata" in' >&2
    echo '        [0-9]*:[0-7]*:[0-9]*:[0-9]*:[0-9]*:[0-9]*) ;;' >&2
    echo '        *) secure_metadata=$(stat -c "%u:%a:%d:%i:%s:%Y" -- "$secure_path" 2>/dev/null) || exit 1 ;;' >&2
    echo '    esac' >&2
    echo '    secure_old_ifs=$IFS' >&2
    echo '    IFS=:' >&2
    echo '    set -f' >&2
    echo '    set -- $secure_metadata' >&2
    echo '    IFS=$secure_old_ifs' >&2
    echo '    [ "$#" -eq 6 ] || exit 1' >&2
    echo '    for secure_field do' >&2
    echo '        case "$secure_field" in ""|*[!0-9]*) exit 1 ;; esac' >&2
    echo '    done' >&2
    echo '    [ "$1" = "$(id -u)" ] || exit 1' >&2
    echo '    case "$2" in 600|0600) ;; *) exit 1 ;; esac' >&2
    printf '%s\n' '    printf "%s\n" "$secure_metadata"' >&2
    echo ')' >&2
    echo 'secure_restore_sha256() (' >&2
    echo '    secure_path=$1' >&2
    echo '    if command -v shasum >/dev/null 2>&1; then' >&2
    echo '        secure_checksum_output=$(shasum -a 256 "$secure_path") || exit 1' >&2
    echo '    elif command -v sha256sum >/dev/null 2>&1; then' >&2
    echo '        secure_checksum_output=$(sha256sum "$secure_path") || exit 1' >&2
    echo '    else' >&2
    echo '        exit 1' >&2
    echo '    fi' >&2
    echo '    set -f' >&2
    echo '    set -- $secure_checksum_output' >&2
    echo '    [ "$#" -ge 2 ] || exit 1' >&2
    echo '    secure_checksum=$1' >&2
    echo '    case "$secure_checksum" in ""|*[!0-9a-f]*) exit 1 ;; esac' >&2
    echo '    [ "${#secure_checksum}" -eq 64 ] || exit 1' >&2
    printf '%s\n' '    printf "%s\n" "$secure_checksum"' >&2
    echo ')' >&2
    echo 'secure_restore_saved_definition() (' >&2
    echo '    secure_backup=$1' >&2
    echo '    secure_definition=$2' >&2
    echo '    secure_expected_checksum=$3' >&2
    echo '    secure_before_metadata=$(secure_restore_metadata "$secure_backup") || exit 1' >&2
    echo '    [ "$(secure_restore_sha256 "$secure_backup")" = "$secure_expected_checksum" ] || exit 1' >&2
    echo '    secure_definition_parent=$(CDPATH="" cd -P "$(dirname "$secure_definition")" 2>/dev/null && pwd -P) || exit 1' >&2
    echo '    secure_definition=$secure_definition_parent/$(basename "$secure_definition")' >&2
    echo '    secure_temp=$(mktemp "$secure_definition_parent/.car-go-clean-service-restore.XXXXXX") || exit 1' >&2
    echo '    if chmod 600 "$secure_temp" &&' >&2
    echo '        cp "$secure_backup" "$secure_temp" &&' >&2
    echo '        [ "$(secure_restore_metadata "$secure_backup")" = "$secure_before_metadata" ] &&' >&2
    echo '        [ "$(secure_restore_sha256 "$secure_backup")" = "$secure_expected_checksum" ] &&' >&2
    echo '        secure_restore_metadata "$secure_temp" >/dev/null &&' >&2
    echo '        cmp -s "$secure_backup" "$secure_temp" &&' >&2
    echo '        [ "$(secure_restore_metadata "$secure_backup")" = "$secure_before_metadata" ] &&' >&2
    echo '        [ "$(secure_restore_sha256 "$secure_temp")" = "$secure_expected_checksum" ] &&' >&2
    echo '        mv -f "$secure_temp" "$secure_definition"; then' >&2
    echo '        exit 0' >&2
    echo '    fi' >&2
    echo '    rm -f "$secure_temp"' >&2
    echo '    exit 1' >&2
    echo ')' >&2
}

# shellcheck disable=SC2016 # The rollback block must expand these expressions when the operator runs it.
print_homebrew_rollback_block() {
    rollback_definition=$(installed_service_definition)
    rollback_backup_word=$(quote_shell_word "$service_definition_backup")
    rollback_definition_word=$(quote_shell_word "$rollback_definition")
    echo "Copy and run this entire rollback block; it stops at the first failing command:" >&2
    echo "# BEGIN car-go-clean exact Homebrew rollback" >&2
    print_secure_definition_restore_helpers
    echo 'canonical_rollback_binary() (' >&2
    echo '    rollback_candidate=$1' >&2
    echo '    case "$rollback_candidate" in /*) ;; *) exit 1 ;; esac' >&2
    echo '    rollback_links=0' >&2
    echo '    while :; do' >&2
    echo '        rollback_parent=$(CDPATH="" cd -P "$(dirname "$rollback_candidate")" 2>/dev/null && pwd -P) || exit 1' >&2
    echo '        rollback_candidate=$rollback_parent/$(basename "$rollback_candidate")' >&2
    echo '        [ -L "$rollback_candidate" ] || break' >&2
    echo '        rollback_links=$((rollback_links + 1))' >&2
    echo '        [ "$rollback_links" -le 40 ] || exit 1' >&2
    echo '        rollback_target=$(readlink "$rollback_candidate") || exit 1' >&2
    echo '        case "$rollback_target" in /*) ;; *) rollback_target=$(dirname "$rollback_candidate")/$rollback_target ;; esac' >&2
    echo '        rollback_candidate=$rollback_target' >&2
    echo '    done' >&2
    echo '    [ -f "$rollback_candidate" ] && [ -x "$rollback_candidate" ] || exit 1' >&2
    printf '%s\n' '    printf "%s\n" "$rollback_candidate"' >&2
    echo ')' >&2
    echo "if (" >&2
    echo "    [ -n \"\${USER-}\" ] &&" >&2
    echo "    rollback_tap=\"\$USER/car-go-clean-rollback\" &&" >&2
    echo "    rollback_formula=\"\$rollback_tap/car-go-clean@$session_old_version\" &&" >&2
    echo "    { brew tap | grep -Fqx -- \"\$rollback_tap\" || brew tap-new \"\$rollback_tap\"; } &&" >&2
    echo "    brew extract --force --version=$session_old_version dcchuck/tap/car-go-clean \"\$rollback_tap\" &&" >&2
    echo "    brew unlink car-go-clean &&" >&2
    echo "    brew install \"\$rollback_formula\" &&" >&2
    echo "    brew link --force --overwrite \"\$rollback_formula\" &&" >&2
    echo "    rollback_prefix=\$(brew --prefix \"\$rollback_formula\") &&" >&2
    echo '    rollback_binary=$(canonical_rollback_binary "$rollback_prefix/bin/car-go-clean") &&' >&2
    echo '    visible_binary=$(command -v car-go-clean) &&' >&2
    echo '    visible_binary=$(canonical_rollback_binary "$visible_binary") &&' >&2
    echo '    [ "$visible_binary" = "$rollback_binary" ] &&' >&2
    echo '    rollback_version=$("$rollback_binary" version) &&' >&2
    echo "    [ \"\$rollback_version\" = $session_old_version ] &&" >&2
    if [ "$session_state" != absent ]; then
        echo "    secure_restore_saved_definition $rollback_backup_word $rollback_definition_word $session_definition_backup_sha256 &&" >&2
        if [ "$platform" = Linux ]; then
            echo "    systemctl --user daemon-reload &&" >&2
        fi
    fi
    if [ "$session_state" = active ]; then
        case "$platform" in
            Darwin)
                echo '    launchctl enable "gui/$(id -u)/com.dcchuck.car-go-clean" &&' >&2
                echo '    launchctl bootstrap "gui/$(id -u)" "$HOME/Library/LaunchAgents/com.dcchuck.car-go-clean.plist" &&' >&2
                echo '    launchctl kickstart -k "gui/$(id -u)/com.dcchuck.car-go-clean"' >&2
                ;;
            Linux)
                echo "    systemctl --user enable --now car-go-clean.service" >&2
                ;;
        esac
    else
        echo "    true" >&2
    fi
    echo "); then" >&2
    echo "    echo \"Exact car-go-clean $session_old_version rollback validated.\"" >&2
    echo "else" >&2
    case "$session_state" in
        active)
            echo "    echo \"Rollback or service restoration failed; the chain stopped at the first failing command.\" >&2" >&2
            ;;
        stopped|absent)
            echo "    echo \"Rollback failed; no service start was requested.\" >&2" >&2
            ;;
    esac
    echo "    false" >&2
    echo "fi" >&2
    echo "# END car-go-clean exact Homebrew rollback" >&2
}

# shellcheck disable=SC2016 # Guidance intentionally preserves expressions for the operator's shell.
print_native_restore_guidance() {
    case "$platform" in
        Darwin)
            echo '  launchctl enable "gui/$(id -u)/com.dcchuck.car-go-clean"' >&2
            echo '  launchctl bootstrap "gui/$(id -u)" "$HOME/Library/LaunchAgents/com.dcchuck.car-go-clean.plist"' >&2
            echo '  launchctl kickstart -k "gui/$(id -u)/com.dcchuck.car-go-clean"' >&2
            ;;
        Linux)
            echo "  systemctl --user enable --now car-go-clean.service" >&2
            ;;
    esac
}

# shellcheck disable=SC2016 # The rollback block must expand these expressions when the operator runs it.
print_shell_rollback_block() {
    rollback_installer=car-go-clean-installer-v$session_old_version.sh
    rollback_binary_path=$session_binary_path
    if [ "$rollback_binary_path" = unresolved ]; then
        rollback_binary_path=$session_old_binary_path
    fi
    rollback_install_dir=$(dirname "$rollback_binary_path")
    rollback_definition=$(installed_service_definition)
    rollback_installer_word=$(quote_shell_word "$rollback_installer")
    rollback_install_dir_word=$(quote_shell_word "$rollback_install_dir")
    rollback_binary_word=$(quote_shell_word "$rollback_binary_path")
    rollback_backup_word=$(quote_shell_word "$service_definition_backup")
    rollback_definition_word=$(quote_shell_word "$rollback_definition")
    echo "Copy and run this entire rollback block; it stops at the first failing command:" >&2
    echo "# BEGIN car-go-clean exact shell rollback" >&2
    print_secure_definition_restore_helpers
    echo "if (" >&2
    printf '    curl --proto '\''=https'\'' --tlsv1.2 -fsSL -o %s https://github.com/dcchuck/car-go-clean/releases/download/v%s/car-go-clean-installer.sh &&\n' \
        "$rollback_installer_word" "$session_old_version" >&2
    printf '    sh %s --version %s --install-dir %s &&\n' \
        "$rollback_installer_word" "$session_old_version" "$rollback_install_dir_word" >&2
    echo "    rollback_version=\$($rollback_binary_word version) &&" >&2
    echo "    [ \"\$rollback_version\" = $session_old_version ] &&" >&2
    if [ "$session_state" != absent ]; then
        echo "    secure_restore_saved_definition $rollback_backup_word $rollback_definition_word $session_definition_backup_sha256 &&" >&2
        if [ "$platform" = Linux ]; then
            echo "    systemctl --user daemon-reload &&" >&2
        fi
    fi
    if [ "$session_state" = active ]; then
        case "$platform" in
            Darwin)
                echo '    launchctl enable "gui/$(id -u)/com.dcchuck.car-go-clean" &&' >&2
                echo '    launchctl bootstrap "gui/$(id -u)" "$HOME/Library/LaunchAgents/com.dcchuck.car-go-clean.plist" &&' >&2
                echo '    launchctl kickstart -k "gui/$(id -u)/com.dcchuck.car-go-clean"' >&2
                ;;
            Linux)
                echo "    systemctl --user enable --now car-go-clean.service" >&2
                ;;
        esac
    else
        echo "    true" >&2
    fi
    echo "); then" >&2
    echo "    echo \"Exact car-go-clean $session_old_version rollback validated.\"" >&2
    echo "else" >&2
    case "$session_state" in
        active)
            echo "    echo \"Rollback or service restoration failed; the chain stopped at the first failing command.\" >&2" >&2
            ;;
        stopped|absent)
            echo "    echo \"Rollback failed; no service start was requested.\" >&2" >&2
            ;;
    esac
    echo "    false" >&2
    echo "fi" >&2
    echo "# END car-go-clean exact shell rollback" >&2
}

replacement_recovery_guidance() {
    echo "The replacement binary did not pass exact v0.4.0 validation." >&2
    case "$session_state" in
        active)
            echo "The originally active service remains persistently disabled and stopped." >&2
            ;;
        stopped)
            echo "The originally stopped service remains persistently disabled and stopped." >&2
            ;;
        absent)
            echo "No service was installed or started." >&2
            ;;
    esac
    echo "Recovery state is retained at $session_file." >&2
    if [ "$session_old_version" != absent ]; then
        case "$session_method" in
            homebrew)
                echo "To roll the binary back to exact $session_old_version with Homebrew:" >&2
                print_homebrew_rollback_block
                ;;
            shell)
                echo "To roll the binary back with the exact old release installer:" >&2
                print_shell_rollback_block
                ;;
        esac
    fi
}

pre_replacement_recovery_guidance() {
    echo "The upgrade stopped before a durable replacement attempt, and the recorded car-go-clean $session_old_version binary could not be validated for automatic restart." >&2
    echo "The originally active service remains persistently disabled and stopped." >&2
    if [ -f "$session_file" ] && [ ! -L "$session_file" ]; then
        echo "Recovery state is retained at $session_file." >&2
    else
        echo "The preserved service definition remains at $service_definition_backup." >&2
    fi
    case "$session_method" in
        homebrew)
            echo "To restore exact car-go-clean $session_old_version with Homebrew:" >&2
            print_homebrew_rollback_block
            ;;
        shell)
            echo "To restore exact car-go-clean $session_old_version with the release installer:" >&2
            print_shell_rollback_block
            ;;
    esac
}

preview_recovery_guidance() {
    echo "The exact v0.4.0 binary is installed, but preview approval is still pending." >&2
    case "$session_state" in
        active)
            echo "The originally active service remains stopped." >&2
            ;;
        stopped)
            echo "The originally stopped service remains stopped." >&2
            ;;
        absent)
            echo "No service was installed or started." >&2
            ;;
    esac
    echo "Resolve the error, then resume config and preview with exactly:" >&2
    printf '  %s --version 0.4.0 --method %s\n' "$0" "$session_method" >&2
    if [ "$session_old_version" != absent ]; then
        case "$session_method" in
            homebrew)
                echo "To roll the binary back to exact $session_old_version with Homebrew:" >&2
                print_homebrew_rollback_block
                ;;
            shell)
                echo "To roll the binary back with the exact old release installer:" >&2
                print_shell_rollback_block
                ;;
        esac
    fi
    if [ "$session_state" = active ] && [ "$session_old_version" = absent ]; then
        echo "Only after a successful preview/cleanup, restore the prior state with:" >&2
        print_native_restore_guidance
    fi
}

ambiguous_execution_guidance() {
    echo "Reviewed execution did not report a complete outcome; the session remains in executing state." >&2
    echo "The helper will not run review $session_review again because completion is ambiguous." >&2
    echo "Inspect car-go-clean status and logs before deciding whether to restore service state." >&2
    if [ "$session_state" = active ]; then
        echo "The originally active service remains stopped; restore it manually only after that inspection." >&2
    fi
    echo "Session retained at $session_file." >&2
}

pre_execution_rejection_guidance() {
    echo "Review $session_review was rejected before execution; no cleanup outcome is claimed." >&2
    echo "The session returned to preview-pending state and recovery evidence remains at $session_file." >&2
    echo "Create and inspect a new preview with exactly:" >&2
    printf '  %s --version 0.4.0 --method %s\n' \
        "$0" "$session_method" >&2
}

validate_review_output() {
    validation_input=$1
    validation_status=$2
    validation_review=$3
    validate_resumed_binary || return 1
    if validation_result=$(
        printf '%s' "$validation_input" |
            "$cgc_binary" __validate-upgrade-review-output \
                --review-id "$validation_review" \
                --exit-code "$validation_status"
    ); then
        :
    else
        return 1
    fi
    case "$validation_status:$validation_result" in
        0:completed|2:completed|1:pre_execution_rejection)
            printf '%s\n' "$validation_result"
            ;;
        *)
            return 1
            ;;
    esac
}

restoration_recovery_guidance() {
    echo "Reviewed execution completed, but service restoration did not; the service remains stopped." >&2
    printf 'Resume restoration without repeating cleanup with exactly:\n  %s --version 0.4.0 --method %s --execute-review %s\n' \
        "$0" "$session_method" "$session_review" >&2
}

run_preview_phase() {
    set +e
    config_output=$("$cgc_binary" config 2>&1)
    config_status=$?
    set -e
    printf '%s\n' "$config_output"
    if [ "$config_status" -ne 0 ]; then
        preview_recovery_guidance
        return 1
    fi
    case "$config_output" in
        *"\`excludes\` is deprecated"*)
            echo "Detected legacy \`excludes\` configuration."
            printf 'Review and apply the migration with: %s config migrate\n' "$cgc_binary"
            ;;
    esac

    set +e
    preview_output=$("$cgc_binary" run --dry-run --all 2>&1)
    preview_status=$?
    set -e
    printf '%s\n' "$preview_output"
    case "$preview_status" in
        0|2) ;;
        *)
            echo "car-go-clean preview failed with exit $preview_status." >&2
            preview_recovery_guidance
            return 1
            ;;
    esac

    review_ids=$(printf '%s\n' "$preview_output" |
        sed -n 's/^Review ID: \([0-9][0-9]*\)$/\1/p')
    review_count=$(printf '%s\n' "$review_ids" |
        awk 'NF { count++ } END { print count + 0 }')
    if [ "$review_count" -ne 1 ]; then
        echo "preview did not produce exactly one usable numeric review ID" >&2
        preview_recovery_guidance
        return 1
    fi
    case "$review_ids" in
        ''|*[!0-9]*)
            echo "preview produced an unusable review ID" >&2
            preview_recovery_guidance
            return 1
            ;;
        *)
            if [ "$review_ids" -le 0 ]; then
                echo "preview produced an unusable review ID" >&2
                preview_recovery_guidance
                return 1
            fi
            ;;
    esac

    write_session review_pending "$review_ids"
    echo "Upgrade preview saved as review $review_ids."
    case "$session_state" in
        active)
            echo "The originally active service remains stopped pending reviewed execution."
            ;;
        stopped)
            echo "The originally stopped service remains stopped."
            ;;
        absent)
            echo "No service was installed or started."
            ;;
    esac
    echo "After approval, execute exactly this review with:"
    printf '  %s --version 0.4.0 --method %s --execute-review %s\n' \
        "$0" "$session_method" "$review_ids"
}

refresh_definition_phase() {
    if [ "$session_state" != absent ]; then
        if ! "$cgc_binary" service refresh; then
            echo "car-go-clean service-definition refresh failed while the service was disabled." >&2
            preview_recovery_guidance
            return 1
        fi
        definition_path=$(installed_service_definition) || return 1
        session_refreshed_definition_sha256=$(
            sha256_file "$definition_path"
        ) || {
            echo "car-go-clean upgrade: could not fingerprint the refreshed service definition" >&2
            preview_recovery_guidance
            return 1
        }
        session_refreshed_definition_binary_path=$(
            authenticate_service_definition \
                "$definition_path" "$session_refreshed_definition_sha256"
        ) || {
            echo "car-go-clean upgrade: could not authenticate the refreshed service definition" >&2
            preview_recovery_guidance
            return 1
        }
        if [ "$session_refreshed_definition_binary_path" != \
            "$session_binary_path" ]; then
            echo "car-go-clean upgrade: refreshed service definition does not resolve to the authenticated replacement binary" >&2
            preview_recovery_guidance
            return 1
        fi
    else
        session_refreshed_definition_sha256=none
        session_refreshed_definition_binary_path=none
    fi
    write_session preview_pending none
}

finalize_executed_session() {
    if ! converge_final_service_state; then
        if [ "$session_state" != absent ]; then
            keep_service_disabled_and_stopped >/dev/null 2>&1 || :
        fi
        restoration_recovery_guidance
        return 1
    fi
    if ! rm -f "$service_definition_backup"; then
        echo "Reviewed execution completed, but the obsolete service-definition backup could not be cleared." >&2
        return 1
    fi
    if ! rm -f "$session_file"; then
        echo "Reviewed execution completed, but the completed upgrade session could not be cleared." >&2
        printf 'Retry finalization without repeating cleanup with exactly:\n  %s --version 0.4.0 --method %s --execute-review %s\n' \
            "$0" "$session_method" "$session_review" >&2
        return 1
    fi
    echo "Upgrade to car-go-clean 0.4.0 completed."
}

execute_review_session() {
    case "$session_phase" in
        review_pending)
            write_session executing "$session_review"
            set +e
            review_output=$(
                "$cgc_binary" run --review "$session_review" --json
            )
            review_status=$?
            set -e
            if [ -n "$review_output" ]; then
                printf '%s\n' "$review_output"
            fi
            case "$review_status" in
                0|1|2)
                    if review_validation=$(
                        validate_review_output \
                            "$review_output" "$review_status" "$session_review"
                    ); then
                        :
                    else
                        ambiguous_execution_guidance
                        return 1
                    fi
                    case "$review_validation" in
                        completed)
                            write_session executed "$session_review"
                            ;;
                        pre_execution_rejection)
                            rejected_review=$session_review
                            write_session preview_pending none
                            session_review=$rejected_review
                            pre_execution_rejection_guidance
                            session_review=none
                            return 1
                            ;;
                        *)
                            ambiguous_execution_guidance
                            return 1
                            ;;
                    esac
                    ;;
                *)
                    ambiguous_execution_guidance
                    return 1
                    ;;
            esac
            ;;
        executing)
            ambiguous_execution_guidance
            return 1
            ;;
        executed)
            ;;
        preview_pending)
            die "preview is still pending; resume it without --execute-review using: $0 --version 0.4.0 --method $session_method"
            ;;
    esac
    finalize_executed_session
}

prepare_state_dir
acquire_session_lock

if [ -e "$session_file" ] || [ -L "$session_file" ]; then
    load_session
    [ "$session_version" = "$version" ] ||
        die "session version does not match requested version"
    [ "$session_method" = "$method" ] ||
        die "session method does not match requested method"
    if [ "$session_phase" = replacement_attempt ]; then
        replacement_recovery_guidance
        exit 1
    fi
    if ! validate_resumed_binary; then
        if [ "$session_state" != absent ]; then
            keep_service_disabled_and_stopped >/dev/null 2>&1 || :
        fi
        replacement_recovery_guidance
        exit 1
    fi
    case "$session_phase" in
        replacement_pending)
            write_session definition_pending none
            if ! refresh_definition_phase; then
                exit 1
            fi
            ;;
        definition_pending)
            if ! refresh_definition_phase; then
                exit 1
            fi
            ;;
    esac
    if [ -n "$execute_review" ]; then
        if [ "$session_phase" != preview_pending ] &&
            [ "$session_review" != "$execute_review" ]; then
            die "review ID $execute_review does not match live session review $session_review"
        fi
        if ! execute_review_session; then
            exit 1
        fi
        exit 0
    fi
    case "$session_phase" in
        preview_pending)
            if ! run_preview_phase; then
                exit 1
            fi
            exit 0
            ;;
        review_pending)
            die "review $session_review is awaiting approval; execute it with: $0 --version 0.4.0 --method $session_method --execute-review $session_review"
            ;;
        executing)
            ambiguous_execution_guidance
            exit 1
            ;;
        executed)
            die "review $session_review already executed; finalize with: $0 --version 0.4.0 --method $session_method --execute-review $session_review"
            ;;
    esac
fi

[ -z "$execute_review" ] ||
    die "no resumable upgrade session exists; run the preview phase first"

command -v car-go-clean >/dev/null 2>&1 ||
    die "no existing car-go-clean installation was found; use a fresh-install path instead"
visible_binary=$(command -v car-go-clean)
validate_absolute_path_value "$visible_binary" ||
    die "visible car-go-clean command is not an absolute executable path: $visible_binary"
visible_resolved=$(canonical_existing_binary "$visible_binary") ||
    die "visible car-go-clean command is unavailable or unsafe: $visible_binary"

brew_inventory=false
homebrew_binary=
if command -v brew >/dev/null 2>&1 &&
    brew list --versions car-go-clean >/dev/null 2>&1; then
    brew_inventory=true
    homebrew_binary=$(installed_homebrew_binary) ||
        die "Homebrew reports car-go-clean installed but its exact formula binary could not be resolved"
fi

case "$method" in
    homebrew)
        [ "$brew_inventory" = true ] ||
            die "visible car-go-clean is not owned by Homebrew; use --method shell if it is a shell installation"
        [ "$visible_resolved" = "$homebrew_binary" ] ||
            die "visible car-go-clean ($visible_binary) is shell-owned or shadows Homebrew; use --method shell for that visible installation"
        old_binary=$homebrew_binary
        ;;
    shell)
        if [ "$brew_inventory" = true ] &&
            [ "$visible_resolved" = "$homebrew_binary" ]; then
            die "visible car-go-clean is Homebrew-managed; use --method homebrew"
        fi
        [ ! -L "$visible_binary" ] ||
            die "visible shell car-go-clean must not be a symlink: $visible_binary"
        [ -w "$visible_binary" ] && [ -w "$(dirname "$visible_binary")" ] ||
            die "visible shell car-go-clean replacement target is not writable: $visible_binary"
        old_binary=$visible_resolved
        ;;
esac

old_version=$("$old_binary" version 2>&1) ||
    die "could not determine the installed car-go-clean version"
case "$old_version" in
    0.2.0|0.3.0) ;;
    *) die "this helper upgrades only car-go-clean 0.2.0 or 0.3.0 (found $old_version)" ;;
esac
service_output=$("$old_binary" service status 2>&1) ||
    die "could not determine the existing service state: $service_output"
state_values=$(printf '%s\n' "$service_output" |
    awk '
        /^  State: / {
            count++
            value = substr($0, 10)
        }
        END {
            if (count == 1) print value
        }
    ')
case "$state_values" in
    running) original_state=active ;;
    stopped) original_state=stopped ;;
    "not installed") original_state=absent ;;
    *) die "could not parse v0.2/v0.3 service status output" ;;
esac

session_version=$version
session_method=$method
session_old_version=$old_version
session_state=$original_state
session_binary_path=unresolved
session_binary_sha256=unresolved
session_old_binary_path=$old_binary
session_refreshed_definition_sha256=none
session_refreshed_definition_binary_path=none

if [ "$original_state" != absent ]; then
    backup_installed_service_definition ||
        die "could not preserve the installed service definition before replacement"
    session_definition_backup_sha256=$(sha256_file "$service_definition_backup") ||
        die "could not fingerprint the preserved service definition"
    session_definition_binary_path=$(
        authenticate_service_definition \
            "$service_definition_backup" "$session_definition_backup_sha256"
    ) || die "could not authenticate the preserved service definition executable"
    [ "$session_definition_binary_path" = "$session_old_binary_path" ] ||
        die "installed service definition does not resolve to the authenticated old binary"
    if [ "$original_state" = active ]; then
        rollback_armed=true
    fi
    disable_installed_service
else
    rm -f "$service_definition_backup"
    session_definition_backup_sha256=none
    session_definition_binary_path=none
fi

write_session replacement_attempt none
rollback_armed=false
replacement_recovery_armed=true

case "$method" in
        homebrew)
        brew update
        brew upgrade dcchuck/tap/car-go-clean
        hash -r 2>/dev/null || :
        new_binary=$(installed_homebrew_binary) ||
            die "could not resolve the exact upgraded Homebrew binary"
        refreshed_visible=$(command -v car-go-clean) ||
            die "Homebrew upgrade left car-go-clean unavailable on PATH"
        validate_absolute_path_value "$refreshed_visible" ||
            die "Homebrew upgrade left an ambiguous car-go-clean command on PATH"
        refreshed_resolved=$(canonical_existing_binary "$refreshed_visible") ||
            die "Homebrew upgrade left an unsafe car-go-clean command on PATH"
        [ "$refreshed_resolved" = "$new_binary" ] ||
            die "Homebrew upgrade did not leave the visible car-go-clean command owned by the upgraded formula"
        ;;
    shell)
        command -v curl >/dev/null 2>&1 || die "curl is not available"
        install_dir=$(dirname "$old_binary")
        work_dir=$(mktemp -d) || die "could not create temporary upgrade directory"
        installer=$work_dir/car-go-clean-installer.sh
        checksum=$work_dir/car-go-clean-shell-assets.sha256
        base_url=https://github.com/dcchuck/car-go-clean/releases/download/v0.4.0
        curl --proto '=https' --tlsv1.2 -fsSL -o "$installer" \
            "$base_url/car-go-clean-installer.sh"
        curl --proto '=https' --tlsv1.2 -fsSL -o "$checksum" \
            "$base_url/car-go-clean-shell-assets.sha256"
        expected_hash=$(awk '
            NF {
                if (NF != 2) {
                    exit 1
                }
                if ($2 == "car-go-clean-installer.sh") {
                    installer_count++
                    installer_hash = $1
                } else if ($2 == "car-go-clean-upgrade.sh") {
                    upgrade_count++
                } else {
                    exit 1
                }
            }
            END {
                if (installer_count != 1 || upgrade_count != 1) exit 1
                print installer_hash
            }
        ' "$checksum") || die "invalid shell-installer checksum"
        case "$platform" in
            Darwin) actual_hash=$(shasum -a 256 "$installer" | awk '{ print $1 }') ;;
            Linux) actual_hash=$(sha256sum "$installer" | awk '{ print $1 }') ;;
        esac
        [ "$actual_hash" = "$expected_hash" ] ||
            die "shell-installer checksum verification failed"
        sh "$installer" --version "$version" --install-dir "$install_dir"
        new_binary=$(canonical_existing_binary "$old_binary") ||
            die "shell installer did not replace the validated car-go-clean target"
        [ "$new_binary" = "$old_binary" ] ||
            die "shell installer replacement target became ambiguous"
        ;;
esac

session_binary_path=$new_binary
session_binary_sha256=$(sha256_file "$new_binary") ||
    die "could not fingerprint the exact replacement car-go-clean binary"
write_session replacement_pending none
replacement_recovery_armed=false
if ! validate_resumed_binary; then
    if [ "$session_state" != absent ]; then
        keep_service_disabled_and_stopped >/dev/null 2>&1 || :
    fi
    replacement_recovery_guidance
    exit 1
fi
write_session definition_pending none
if ! refresh_definition_phase; then
    exit 1
fi
if ! run_preview_phase; then
    exit 1
fi
