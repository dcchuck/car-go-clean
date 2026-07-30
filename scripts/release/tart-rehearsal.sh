#!/bin/sh
set -eu

usage() {
    echo "usage: $0 /absolute/path/to/rehearsal-artifacts /absolute/path/to/evidence" >&2
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

test "$#" -eq 2 || {
    usage
    exit 2
}
artifact_dir=$1
evidence_dir=$2
case "$artifact_dir:$evidence_dir" in
    /*:/*) ;;
    *) die "artifact and evidence paths must be absolute" ;;
esac
test -d "$artifact_dir" || die "artifact directory does not exist: $artifact_dir"
test -f "$artifact_dir/SHA256SUMS" ||
    die "artifact directory must contain the exact rehearsal SHA256SUMS"

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
command -v sshpass >/dev/null 2>&1 || die "sshpass is required for the documented admin/admin Tart images"
command -v ssh >/dev/null 2>&1 || die "ssh is not available"
command -v scp >/dev/null 2>&1 || die "scp is not available"
command -v python3 >/dev/null 2>&1 || die "python3 is required"

script_dir=$(CDPATH='' cd -- "$(dirname -- "$0")" && pwd)
acceptance_script=$script_dir/acceptance.sh
test -f "$acceptance_script" || die "guest acceptance script is missing"

python3 - "$artifact_dir/SHA256SUMS" <<'PY'
import pathlib
import re
import sys

manifest = pathlib.Path(sys.argv[1])
seen = set()
for line_number, line in enumerate(manifest.read_text().splitlines(), 1):
    match = re.fullmatch(r"([0-9a-f]{64}) [ *]([^\r\n]+)", line)
    if not match:
        raise SystemExit(f"malformed SHA256SUMS line {line_number}")
    name = match.group(2)
    path = pathlib.PurePosixPath(name)
    if path.is_absolute() or ".." in path.parts or name in seen:
        raise SystemExit(f"unsafe or duplicate SHA256SUMS path on line {line_number}")
    seen.add(name)
if not seen:
    raise SystemExit("SHA256SUMS is empty")
PY

if command -v sha256sum >/dev/null 2>&1; then
    (CDPATH='' cd "$artifact_dir" && sha256sum -c SHA256SUMS)
else
    (CDPATH='' cd "$artifact_dir" && shasum -a 256 -c SHA256SUMS)
fi

mkdir -p "$evidence_dir"
chmod 700 "$evidence_dir"
source_map=$evidence_dir/source-map.tsv
: > "$source_map"
chmod 600 "$source_map"

ssh_password=${CAR_GO_CLEAN_TART_SSH_PASSWORD:-admin}
ssh_user=${CAR_GO_CLEAN_TART_SSH_USER:-admin}
ssh_options="-o StrictHostKeyChecking=no -o UserKnownHostsFile=/dev/null -o LogLevel=ERROR -o ConnectTimeout=5"

ssh_guest() {
    guest_host=$1
    shift
    # shellcheck disable=SC2086 # The fixed option string must become separate ssh arguments.
    sshpass -p "$ssh_password" ssh $ssh_options "$ssh_user@$guest_host" "$@"
}

scp_guest() {
    # shellcheck disable=SC2086 # The fixed option string must become separate scp arguments.
    sshpass -p "$ssh_password" scp $ssh_options "$@"
}

wait_for_ssh() {
    vm_name=$1
    attempts=0
    while test "$attempts" -lt 60; do
        guest_ip=$(tart ip "$vm_name" --wait 5 2>/dev/null || :)
        if test -n "$guest_ip" &&
            ssh_guest "$guest_ip" "printf ready" >/dev/null 2>&1; then
            printf '%s\n' "$guest_ip"
            return 0
        fi
        attempts=$((attempts + 1))
        sleep 2
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

manifest_hash() {
    if command -v sha256sum >/dev/null 2>&1; then
        sha256sum "$artifact_dir/SHA256SUMS" | awk '{ print $1 }'
    else
        shasum -a 256 "$artifact_dir/SHA256SUMS" | awk '{ print $1 }'
    fi
}

run_vm() {
    platform=$1
    image=$2
    digest=$3
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
        printf 'vm_name=%s\n' "$vm_name"
    } > "$platform_evidence/host-metadata.txt"

    tart pull "$image"
    tart clone "$image" "$vm_name"
    printf '%s\t%s\t%s\n' "$vm_name" "$image" "$digest" >> "$source_map"
    tart run "$vm_name" --no-graphics \
        > "$platform_evidence/tart-run.log" 2>&1 &

    guest_ip=$(wait_for_ssh "$vm_name") || {
        echo "VM $vm_name did not become reachable" >&2
        return 1
    }
    # shellcheck disable=SC2016 # HOME must expand in the guest, not on the host.
    guest_home=$(ssh_guest "$guest_ip" 'printf %s "$HOME"') || {
        echo "could not resolve guest HOME for $vm_name" >&2
        return 1
    }
    case "$guest_home" in
        /*) ;;
        *) echo "guest HOME is not absolute for $vm_name" >&2; return 1 ;;
    esac
    remote_root=$guest_home/car-go-clean-v040-acceptance
    ssh_guest "$guest_ip" "mkdir -p '$remote_root/evidence'"
    scp_guest -r "$artifact_dir" "$ssh_user@$guest_ip:$remote_root/artifacts"
    scp_guest "$acceptance_script" "$ssh_user@$guest_ip:$remote_root/acceptance.sh"

    verify_command="cd '$remote_root/artifacts' && if command -v sha256sum >/dev/null 2>&1; then sha256sum -c SHA256SUMS; else shasum -a 256 -c SHA256SUMS; fi"
    phase_status=0
    if ! ssh_guest "$guest_ip" "$verify_command" \
        > "$platform_evidence/guest-hash-verification.log" 2>&1; then
        phase_status=1
    fi

    if test "$phase_status" -eq 0; then
        pre_command="CAR_GO_CLEAN_ACCEPTANCE_VERSION='$version' CAR_GO_CLEAN_ACCEPTANCE_SHA='$exact_sha' sh '$remote_root/acceptance.sh' '$remote_root/artifacts' '$remote_root/evidence' pre-reboot"
        if ! ssh_guest "$guest_ip" "$pre_command" \
            > "$platform_evidence/pre-reboot-ssh.log" 2>&1; then
            phase_status=1
        fi
    fi

    if test "$phase_status" -eq 0; then
        set +e
        ssh_guest "$guest_ip" "sudo reboot" \
            > "$platform_evidence/reboot-ssh.log" 2>&1
        reboot_status=$?
        set -e
        case "$reboot_status" in
            0|255) ;;
            *) phase_status=1 ;;
        esac
        if test "$phase_status" -eq 0; then
            new_guest_ip=$(wait_for_reboot "$vm_name" "$guest_ip") || phase_status=1
        fi
    fi

    if test "$phase_status" -eq 0; then
        post_command="CAR_GO_CLEAN_ACCEPTANCE_VERSION='$version' CAR_GO_CLEAN_ACCEPTANCE_SHA='$exact_sha' sh '$remote_root/acceptance.sh' '$remote_root/artifacts' '$remote_root/evidence' post-reboot"
        if ! ssh_guest "$new_guest_ip" "$post_command" \
            > "$platform_evidence/post-reboot-ssh.log" 2>&1; then
            phase_status=1
        fi
        guest_ip=$new_guest_ip
    fi

    # Evidence extraction is unconditional once the guest is reachable. It
    # happens before this function returns either success or failure.
    if ! scp_guest -r "$ssh_user@$guest_ip:$remote_root/evidence/." "$platform_evidence"; then
        echo "could not copy acceptance evidence from $vm_name" >&2
        phase_status=1
    fi
    return "$phase_status"
}

overall_status=0
if ! run_vm macos "$macos_image" "$macos_digest"; then
    overall_status=1
fi
if ! run_vm linux "$linux_image" "$linux_digest"; then
    overall_status=1
fi

if test "$overall_status" -ne 0; then
    die "one or more guest acceptance runs failed; sanitized evidence was copied before return"
fi
echo "Fresh Tart acceptance passed for macOS and Linux."
echo "VMs were intentionally preserved. Inventory and review them before explicit cleanup."
