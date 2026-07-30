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
work_dir=
session_temp=
rollback_armed=false
original_state=

cleanup_temporary_files() {
    if [ -n "$session_temp" ] && [ -e "$session_temp" ]; then
        rm -f "$session_temp"
    fi
    if [ -n "$work_dir" ] && [ -d "$work_dir" ]; then
        rm -rf "$work_dir"
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
    session_state=
    session_review=
    seen_format=false
    seen_version=false
    seen_method=false
    seen_state=false
    seen_review=false
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
            service_state=*)
                [ "$seen_state" = false ] || malformed=true
                seen_state=true
                session_state=${line#service_state=}
                ;;
            review_id=*)
                [ "$seen_review" = false ] || malformed=true
                seen_review=true
                session_review=${line#review_id=}
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
        [ "$seen_state" = true ] &&
        [ "$seen_review" = true ] ||
        die "upgrade session is malformed"
    [ "$session_format" = 1 ] || die "upgrade session is malformed"
    [ "$session_version" = 0.4.0 ] || die "upgrade session is malformed"
    case "$session_method" in homebrew|shell) ;; *) die "upgrade session is malformed" ;; esac
    case "$session_state" in active|stopped|absent) ;; *) die "upgrade session is malformed" ;; esac
    case "$session_review" in
        ''|*[!0-9]*) die "upgrade session is malformed" ;;
        *) [ "$session_review" -gt 0 ] || die "upgrade session is malformed" ;;
    esac
}

write_session() {
    review_id=$1
    prepare_state_dir
    session_temp=$(mktemp "$state_dir/.upgrade-session.XXXXXX") ||
        die "could not create upgrade session"
    chmod 600 "$session_temp"
    {
        printf 'format=1\n'
        printf 'version=%s\n' "$version"
        printf 'method=%s\n' "$method"
        printf 'service_state=%s\n' "$original_state"
        printf 'review_id=%s\n' "$review_id"
    } > "$session_temp"
    mv -f "$session_temp" "$session_file"
    session_temp=
}

post_replacement_guidance() {
    echo "The v0.4.0 binary is installed, but the upgrade did not complete; the service remains stopped." >&2
    echo "Resolve the error, then rerun the helper or perform a binary rollback before starting the service." >&2
}

pending_review_guidance() {
    echo "Reviewed cleanup did not complete; the service remains stopped and the session was retained." >&2
    echo "For a transient failure, resolve it and rerun with --execute-review $session_review." >&2
    echo "For a stale or invalid review, create and approve a fresh preview manually; the original service state was $session_state." >&2
    echo "After that reviewed run succeeds, restore only an originally active service and remove $session_file." >&2
}

prepare_state_dir

if [ -n "$execute_review" ]; then
    load_session
    [ "$session_version" = "$version" ] ||
        die "session version does not match requested version"
    [ "$session_method" = "$method" ] ||
        die "session method does not match requested method"
    [ "$session_review" = "$execute_review" ] ||
        die "review ID $execute_review does not match live session review $session_review"
    resumed_version=$(car-go-clean version 2>&1) ||
        die "could not validate the replacement car-go-clean binary"
    [ "$resumed_version" = 0.4.0 ] ||
        die "expected car-go-clean 0.4.0 before reviewed execution, found $resumed_version"

    if ! car-go-clean run --review "$session_review"; then
        pending_review_guidance
        exit 1
    fi
    if [ "$session_state" = active ]; then
        if ! restore_active_service; then
            pending_review_guidance
            exit 1
        fi
    fi
    rm -f "$session_file"
    echo "Upgrade to car-go-clean 0.4.0 completed."
    exit 0
fi

if [ -e "$session_file" ] || [ -L "$session_file" ]; then
    load_session
    die "a live upgrade session already exists for review $session_review; resume it with --execute-review $session_review"
fi

old_binary=
if command -v car-go-clean >/dev/null 2>&1; then
    old_binary=$(command -v car-go-clean)
    old_version=$(car-go-clean version 2>&1) ||
        die "could not determine the installed car-go-clean version"
    case "$old_version" in
        0.2.0|0.3.0) ;;
        *) die "this helper upgrades only car-go-clean 0.2.0 or 0.3.0 (found $old_version)" ;;
    esac
    service_output=$(car-go-clean service status 2>&1) ||
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
else
    original_state=absent
fi

if [ "$original_state" = active ]; then
    rollback_armed=true
    stop_active_service
fi

case "$method" in
    homebrew)
        command -v brew >/dev/null 2>&1 || die "Homebrew is not available"
        brew update
        if brew list --versions car-go-clean >/dev/null 2>&1; then
            brew upgrade dcchuck/tap/car-go-clean
        else
            brew install dcchuck/tap/car-go-clean
        fi
        new_binary=car-go-clean
        ;;
    shell)
        command -v curl >/dev/null 2>&1 || die "curl is not available"
        if [ -n "$old_binary" ]; then
            install_dir=$(dirname "$old_binary")
        else
            install_dir=$HOME/.local/bin
        fi
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
        new_binary=$install_dir/car-go-clean
        ;;
esac

new_version=$("$new_binary" version 2>&1) ||
    die "could not validate the replacement car-go-clean binary"
[ "$new_version" = 0.4.0 ] ||
    die "expected car-go-clean 0.4.0 after replacement, found $new_version"
rollback_armed=false

set +e
config_output=$("$new_binary" config 2>&1)
config_status=$?
set -e
printf '%s\n' "$config_output"
if [ "$config_status" -ne 0 ]; then
    post_replacement_guidance
    exit 1
fi
case "$config_output" in
    *"\`excludes\` is deprecated"*)
        echo "Detected legacy \`excludes\` configuration."
        echo 'Review and apply the migration with: car-go-clean config migrate'
        ;;
esac

set +e
preview_output=$("$new_binary" run --dry-run --all 2>&1)
preview_status=$?
set -e
printf '%s\n' "$preview_output"
case "$preview_status" in
    0|2) ;;
    *)
        echo "car-go-clean preview failed with exit $preview_status." >&2
        post_replacement_guidance
        exit 1
        ;;
esac

review_ids=$(printf '%s\n' "$preview_output" |
    sed -n 's/^Review ID: \([0-9][0-9]*\)$/\1/p')
review_count=$(printf '%s\n' "$review_ids" |
    awk 'NF { count++ } END { print count + 0 }')
[ "$review_count" -eq 1 ] || {
    echo "preview did not produce exactly one usable numeric review ID" >&2
    post_replacement_guidance
    exit 1
}
case "$review_ids" in
    ''|*[!0-9]*) die "preview produced an unusable review ID" ;;
    *) [ "$review_ids" -gt 0 ] || die "preview produced an unusable review ID" ;;
esac

write_session "$review_ids"
echo "Upgrade preview saved as review $review_ids; the service remains stopped."
echo "After approval, execute exactly this review with:"
printf '  %s --version 0.4.0 --method %s --execute-review %s\n' \
    "$0" "$method" "$review_ids"
