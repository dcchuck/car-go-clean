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
work_dir=
session_temp=
lock_held=false
rollback_armed=false
original_state=
cgc_binary=
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
    if [ -n "$work_dir" ] && [ -d "$work_dir" ]; then
        rm -rf "$work_dir"
    fi
    if [ "$lock_held" = true ]; then
        rmdir "$session_lock" 2>/dev/null || :
        lock_held=false
    fi
}

launchd_target() {
    uid=$(id -u) || return 1
    printf 'gui/%s/com.dcchuck.car-go-clean\n' "$uid"
}

stop_active_service() {
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

restore_active_service() {
    case "$platform" in
        Darwin)
            target=$(launchd_target) || return 1
            launchctl bootstrap "gui/$(id -u)" \
                "$HOME/Library/LaunchAgents/com.dcchuck.car-go-clean.plist" &&
                launchctl kickstart -k "$target"
            ;;
        Linux)
            systemctl --user start car-go-clean.service
            ;;
    esac
}

service_is_active() {
    case "$platform" in
        Darwin)
            target=$(launchd_target) || return 1
            launchctl print "$target" >/dev/null 2>&1
            ;;
        Linux)
            systemctl --user is-active --quiet car-go-clean.service >/dev/null 2>&1
            ;;
    esac
}

on_exit() {
    status=$?
    trap - 0 HUP INT TERM
    set +e
    if [ "$status" -ne 0 ] &&
        [ "$rollback_armed" = true ] &&
        [ "$original_state" = active ]; then
        echo "Upgrade failed before exact v0.4.0 replacement; restoring the previously active service." >&2
        if ! restore_active_service; then
            echo "Automatic service rollback failed; start the existing service with its native manager." >&2
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
    if [ -L "$state_dir" ]; then
        die "upgrade state directory must not be a symlink: $state_dir"
    fi
    umask 077
    mkdir -p "$state_dir"
    [ -d "$state_dir" ] || die "upgrade state path is not a directory: $state_dir"
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
    seen_format=false
    seen_version=false
    seen_method=false
    seen_old_version=false
    seen_state=false
    seen_phase=false
    seen_review=false
    seen_binary_path=false
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
        [ "$seen_binary_path" = true ] ||
        die "upgrade session is malformed"
    [ "$session_format" = 3 ] || die "upgrade session is malformed"
    [ "$session_version" = 0.4.0 ] || die "upgrade session is malformed"
    case "$session_method" in homebrew|shell) ;; *) die "upgrade session is malformed" ;; esac
    case "$session_old_version" in
        0.2.0|0.3.0|absent) ;;
        *) die "upgrade session is malformed" ;;
    esac
    case "$session_state" in active|stopped|absent) ;; *) die "upgrade session is malformed" ;; esac
    validate_absolute_path_value "$session_binary_path" ||
        die "upgrade session is malformed"
    resolved_session_binary=$(canonical_existing_binary "$session_binary_path") ||
        die "upgrade session binary path is unavailable or unsafe"
    [ "$resolved_session_binary" = "$session_binary_path" ] ||
        die "upgrade session binary path is no longer exact"
    case "$session_phase" in
        preview_pending)
            [ "$session_review" = none ] || die "upgrade session is malformed"
            ;;
        review_pending|executing|executed)
            case "$session_review" in
                ''|*[!0-9]*) die "upgrade session is malformed" ;;
                *) [ "$session_review" -gt 0 ] || die "upgrade session is malformed" ;;
            esac
            ;;
        *)
            die "upgrade session is malformed"
            ;;
    esac
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
        printf 'format=3\n'
        printf 'version=%s\n' "$session_version"
        printf 'method=%s\n' "$session_method"
        printf 'old_version=%s\n' "$session_old_version"
        printf 'service_state=%s\n' "$session_state"
        printf 'phase=%s\n' "$next_phase"
        printf 'review_id=%s\n' "$next_review"
        printf 'binary_path=%s\n' "$session_binary_path"
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
    resumed_version=$("$cgc_binary" version 2>&1) ||
        die "could not validate the replacement car-go-clean binary"
    [ "$resumed_version" = 0.4.0 ] ||
        die "expected car-go-clean 0.4.0 while resuming, found $resumed_version"
}

print_homebrew_rollback_block() {
    echo "Copy and run this entire rollback block; it stops at the first failing command:" >&2
    echo "# BEGIN car-go-clean exact Homebrew rollback" >&2
    echo "if (" >&2
    echo "    [ -n \"\${USER-}\" ] &&" >&2
    echo "    rollback_tap=\"\$USER/car-go-clean-rollback\" &&" >&2
    echo "    rollback_formula=\"\$rollback_tap/car-go-clean@$session_old_version\" &&" >&2
    echo "    { brew tap | grep -Fqx -- \"\$rollback_tap\" || brew tap-new \"\$rollback_tap\"; } &&" >&2
    echo "    brew extract --force --version=$session_old_version dcchuck/tap/car-go-clean \"\$rollback_tap\" &&" >&2
    echo "    brew unlink car-go-clean &&" >&2
    echo "    brew install \"\$rollback_formula\" &&" >&2
    echo "    brew link --force --overwrite \"\$rollback_formula\" &&" >&2
    echo "    rollback_version=\$(car-go-clean version) &&" >&2
    if [ "$session_state" = active ]; then
        echo "    [ \"\$rollback_version\" = $session_old_version ] &&" >&2
        echo "    car-go-clean service start" >&2
    else
        echo "    [ \"\$rollback_version\" = $session_old_version ]" >&2
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
                rollback_installer=car-go-clean-installer-v$session_old_version.sh
                echo "To roll the binary back with the exact old release installer:" >&2
                printf '  curl --proto '\''=https'\'' --tlsv1.2 -fsSL -o %s https://github.com/dcchuck/car-go-clean/releases/download/v%s/car-go-clean-installer.sh\n' \
                    "$rollback_installer" "$session_old_version" >&2
                rollback_install_dir=$(dirname "$session_binary_path")
                printf '  sh %s --version %s --install-dir "%s"\n' \
                    "$rollback_installer" "$session_old_version" "$rollback_install_dir" >&2
                if [ "$session_state" = active ]; then
                    echo "Only after a successful rollback, restore the prior state with:" >&2
                    echo "  car-go-clean service start" >&2
                fi
                ;;
        esac
    fi
    if [ "$session_state" = active ] && [ "$session_old_version" = absent ]; then
        echo "Only after a successful preview/cleanup, restore the prior state with:" >&2
        echo "  car-go-clean service start" >&2
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

finalize_executed_session() {
    if [ "$session_state" = active ] && ! service_is_active; then
        if ! restore_active_service; then
            restoration_recovery_guidance
            return 1
        fi
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
            "$cgc_binary" run --review "$session_review"
            review_status=$?
            set -e
            case "$review_status" in
                0|2)
                    write_session executed "$session_review"
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
    validate_resumed_binary
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

if [ "$original_state" = active ]; then
    rollback_armed=true
    stop_active_service
fi

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

new_version=$("$new_binary" version 2>&1) ||
    die "could not validate the replacement car-go-clean binary"
[ "$new_version" = 0.4.0 ] ||
    die "expected car-go-clean 0.4.0 after replacement, found $new_version"
rollback_armed=false
cgc_binary=$new_binary

session_version=$version
session_method=$method
session_old_version=$old_version
session_state=$original_state
session_binary_path=$new_binary
write_session preview_pending none
if ! run_preview_phase; then
    exit 1
fi
