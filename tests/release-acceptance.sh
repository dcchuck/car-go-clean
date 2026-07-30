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
            case "$host" in
                *@192.0.2.10) : > "$TART_STATE/rebooted-macos" ;;
                *@192.0.2.20) : > "$TART_STATE/rebooted-linux" ;;
            esac
            exit 255
        fi
        printf 'ready'
        ;;
    *'printf %s "$HOME"'*)
        printf '/home/admin'
        ;;
    *'rustup-init-'*'--default-toolchain 1.95.0'*)
        case "$host:${FAIL_GUEST_DEPENDENCY-}" in
            admin@192.0.2.10:macos|admin@192.0.2.20:linux)
                echo "required guest dependency missing" >&2
                exit 29
                ;;
        esac
        case "$host" in
            admin@192.0.2.10)
                printf 'platform=macos\npython=Python 3.12.0\nbrew=Homebrew 4.6.0\n'
                ;;
            admin@192.0.2.20)
                printf 'platform=linux\npython=Python 3.12.0\ncc=cc 15.0\n'
                ;;
        esac
        printf 'rustc=rustc 1.95.0 (fixture)\ncargo=cargo 1.95.0 (fixture)\n'
        ;;
    *'acceptance.sh'*'pre-reboot'*)
        ;;
    *'/usr/sbin/sysctl -n kern.boottime'*)
        if test "${UNCHANGED_BOOT_ID-}" = 1 ||
            test ! -e "$TART_STATE/rebooted-macos"; then
            printf '{ sec = 100, usec = 0 }\n'
        else
            printf '{ sec = 200, usec = 0 }\n'
        fi
        ;;
    *'cat /proc/sys/kernel/random/boot_id'*)
        if test "${UNCHANGED_BOOT_ID-}" = 1 ||
            test ! -e "$TART_STATE/rebooted-linux"; then
            printf 'linux-boot-before\n'
        else
            printf 'linux-boot-after\n'
        fi
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
case "$*: ${FAIL_PRE_COPY_HOST-}" in
    *admin@192.0.2.10:*evidence*pre-reboot*:*macos) exit 31 ;;
    *admin@192.0.2.20:*evidence*pre-reboot*:*linux) exit 31 ;;
esac
case "$*" in
    *admin@*:*evidence*/*)
        mkdir -p "$destination"
        case "$destination" in
            */pre-reboot)
                printf 'sanitized pre-reboot fixture evidence\n' \
                    > "$destination/pre-reboot-transcript.log"
                ;;
            */post-reboot)
                printf 'sanitized post-reboot fixture evidence\n' \
                    > "$destination/post-reboot-transcript.log"
                ;;
        esac
        ;;
esac
EOF

cat > "$fake_bin/git" <<'EOF'
#!/bin/sh
set -eu
case "$1" in
    status)
        test "${FAKE_GIT_DIRTY-}" != 1 || printf ' M local-change\n'
        exit 0
        ;;
    rev-parse)
        case "$*" in
            *refs/tags/*) exit 1 ;;
            *) printf '%s\n' "$FAKE_EXACT_SHA" ;;
        esac
        ;;
    merge-base) exit 0 ;;
    ls-remote) exit 2 ;;
    *) exit 64 ;;
esac
EOF

cat > "$fake_bin/cargo" <<'EOF'
#!/bin/sh
set -eu
test "$1" = metadata
printf '{"packages":[{"name":"car-go-clean","version":"0.4.0"}]}\n'
EOF

cat > "$fake_bin/jq" <<'EOF'
#!/bin/sh
set -eu
cat >/dev/null
printf '0.4.0\n'
EOF

cat > "$fake_bin/df" <<'EOF'
#!/bin/sh
set -eu
printf '%s\n' "${2-}" >> "${DF_LOG:-/dev/null}"
exec /bin/df "$@"
EOF

chmod +x "$fake_bin/tart" "$fake_bin/sshpass" "$fake_bin/ssh" "$fake_bin/scp" \
    "$fake_bin/git" "$fake_bin/cargo" "$fake_bin/jq" "$fake_bin/df"

export CALL_LOG="$call_log"
export TART_STATE="$tart_state"
export FAKE_EXACT_SHA=18e2b772698b5f9b67da64c4ad299beacfe219e9

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

# Tart's supported TART_HOME controls both byte accounting and df metrics.
supported_tart_home=$work_dir/supported-tart-home
mkdir -p "$supported_tart_home"
printf 'tart bytes\n' > "$supported_tart_home/blob"
supported_inventory=$work_dir/supported-inventory.tsv
df_log=$work_dir/df.log
: > "$df_log"
PATH="$fake_bin:/usr/bin:/bin" TART_HOME="$supported_tart_home" \
    DF_LOG="$df_log" \
    "$root/scripts/release/tart-inventory.sh" "$supported_inventory"
assert_not_contains "$supported_inventory" "# tart_storage_bytes	0"
assert_contains "$df_log" "$supported_tart_home"

# Cleanup is inert without the exact confirmation and touches only concrete names.
: > "$call_log"
run_capture env PATH="$fake_bin:/usr/bin:/bin" CALL_LOG="$call_log" \
    TART_STATE="$tart_state" \
    "$root/scripts/release/tart-cleanup.sh" "$inventory"
test "$run_status" -ne 0 || fail "cleanup ran without explicit confirmation"
assert_not_contains "$call_log" "tart delete"
assert_contains "$output_file" "alpha"
assert_contains "$output_file" "legacy"

mkdir -p "$work_dir/tart-home"
: > "$df_log"
run_capture env PATH="$fake_bin:/usr/bin:/bin" CALL_LOG="$call_log" \
    TART_STATE="$tart_state" TART_HOME="$work_dir/tart-home" DF_LOG="$df_log" \
    CAR_GO_CLEAN_TART_DELETE_ALL=YES \
    "$root/scripts/release/tart-cleanup.sh" "$inventory"
assert_status "$run_status" 0
assert_contains "$call_log" "tart stop alpha"
assert_contains "$call_log" "tart stop legacy"
assert_contains "$call_log" "tart delete alpha"
assert_contains "$call_log" "tart delete legacy"
assert_contains "$call_log" "tart prune --entries caches --space-budget 0"
assert_not_contains "$call_log" "tart prune --entries vms"
assert_contains "$df_log" "$work_dir/tart-home"

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
aggregate=$work_dir/aggregate
evidence=$work_dir/evidence
mkdir -p "$artifacts" "$aggregate/jobs"
cp "$root/scripts/release/acceptance.sh" "$artifacts/acceptance.sh"
cp "$root/packaging/release/car-go-clean-installer.sh" \
    "$artifacts/car-go-clean-installer.sh"
cp "$root/packaging/release/car-go-clean-upgrade.sh" \
    "$artifacts/car-go-clean-upgrade.sh"
for target in aarch64-apple-darwin aarch64-unknown-linux-musl; do
    printf 'archive %s\n' "$target" > "$artifacts/car-go-clean-$target.tar.xz"
    archive_hash=$(shasum -a 256 "$artifacts/car-go-clean-$target.tar.xz" |
        awk '{ print $1 }')
    printf '%s  car-go-clean-%s.tar.xz\n' "$archive_hash" "$target" \
        > "$artifacts/car-go-clean-$target.tar.xz.sha256"
    for old_version in 0.2.0 0.3.0; do
        printf 'old fixture %s %s\n' "$old_version" "$target" \
            > "$artifacts/car-go-clean-v$old_version-$target"
    done
done
installer_hash=$(shasum -a 256 "$artifacts/car-go-clean-installer.sh" |
    awk '{ print $1 }')
upgrade_hash=$(shasum -a 256 "$artifacts/car-go-clean-upgrade.sh" |
    awk '{ print $1 }')
printf '%s  car-go-clean-installer.sh\n%s  car-go-clean-upgrade.sh\n' \
    "$installer_hash" "$upgrade_hash" \
    > "$artifacts/car-go-clean-shell-assets.sha256"
for target in aarch64-apple-darwin aarch64-unknown-linux-gnu; do
    rustup=rustup-init-$target
    printf '#!/bin/sh\nexit 0\n' > "$artifacts/$rustup"
    rustup_hash=$(shasum -a 256 "$artifacts/$rustup" | awk '{ print $1 }')
    printf '%s  rustup-init\n' "$rustup_hash" > "$artifacts/$rustup.sha256"
done

apple_hash=$(shasum -a 256 \
    "$artifacts/car-go-clean-aarch64-apple-darwin.tar.xz" | awk '{ print $1 }')
linux_hash=$(shasum -a 256 \
    "$artifacts/car-go-clean-aarch64-unknown-linux-musl.tar.xz" |
    awk '{ print $1 }')
x86_apple_hash=cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc
x86_linux_hash=dddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddd
sed \
    -e 's/__TAG__/v0.4.0/g' \
    -e "s/__AARCH64_APPLE_SHA256__/$apple_hash/" \
    -e "s/__X86_64_APPLE_SHA256__/$x86_apple_hash/" \
    -e "s/__AARCH64_LINUX_SHA256__/$linux_hash/" \
    -e "s/__X86_64_LINUX_SHA256__/$x86_linux_hash/" \
    "$root/packaging/release/homebrew/car-go-clean.rb.in" \
    > "$artifacts/car-go-clean.rb"

: > "$artifacts/SHA256SUMS"
for artifact in "$artifacts"/*; do
    name=${artifact##*/}
    test "$name" = SHA256SUMS && continue
    hash=$(shasum -a 256 "$artifact" | awk '{ print $1 }')
    printf '%s  %s\n' "$hash" "$name" >> "$artifacts/SHA256SUMS"
done

printf 'ready\n' > "$aggregate/aggregate-status.txt"
cat > "$aggregate/aggregate-inventory.json" <<EOF
{"format_version":1,"exact_sha":"$FAKE_EXACT_SHA","version":"0.4.0","complete":true,"sanitized":true}
EOF
cat > "$aggregate/jobs/validate.json" <<EOF
{"format_version":1,"phase":"validate","exact_sha":"$FAKE_EXACT_SHA","version":"0.4.0","outcomes":{"evidence_key":"success","checkout":"success","fetch_refs":"success","validation":"success","rust_toolchain":"success","install_cargo_dist":"success","dist_plan":"success"}}
EOF
for target_hash in \
    "aarch64-apple-darwin:$apple_hash" \
    "x86_64-apple-darwin:$x86_apple_hash" \
    "aarch64-unknown-linux-musl:$linux_hash" \
    "x86_64-unknown-linux-musl:$x86_linux_hash"
do
    target=${target_hash%%:*}
    hash=${target_hash#*:}
    linux_dependencies=success
    case "$target" in *apple-darwin) linux_dependencies=skipped ;; esac
    cat > "$aggregate/jobs/build-$target.json" <<EOF
{"format_version":1,"phase":"build","exact_sha":"$FAKE_EXACT_SHA","version":"0.4.0","target":"$target","archive_sha256":"$hash","outcomes":{"checkout":"success","fetch_refs":"success","revalidation":"success","rust_toolchain":"success","linux_dependencies":"$linux_dependencies","install_cargo_dist":"success","build":"success","attestation":"success","archive_upload":"success"}}
EOF
    cat > "$aggregate/jobs/smoke-$target.json" <<EOF
{"format_version":1,"phase":"smoke","exact_sha":"$FAKE_EXACT_SHA","version":"0.4.0","target":"$target","archive_sha256":"$hash","outcomes":{"checkout":"success","artifact_download":"success","installer_and_formula":"success"}}
EOF
done
cat > "$aggregate/jobs/runner-resolution.json" <<EOF
{"format_version":1,"phase":"runner-resolution","exact_sha":"$FAKE_EXACT_SHA","version":"0.4.0","resolution":"verified"}
EOF
cat > "$aggregate/jobs/tap-capability.json" <<EOF
{"format_version":1,"phase":"tap-capability","exact_sha":"$FAKE_EXACT_SHA","version":"0.4.0","outcomes":{"checkout":"success","capability":"success","cleanup":"success"}}
EOF

run_capture env PATH="$fake_bin:/usr/bin:/bin" CALL_LOG="$call_log" \
    TART_STATE="$tart_state" CAR_GO_CLEAN_TART_MACOS_IMAGE=ghcr.io/example/macos:latest \
    CAR_GO_CLEAN_TART_LINUX_IMAGE=ghcr.io/example/linux:latest \
    CAR_GO_CLEAN_ACCEPTANCE_VERSION=0.4.0 \
    CAR_GO_CLEAN_ACCEPTANCE_SHA="$FAKE_EXACT_SHA" \
    "$root/scripts/release/tart-rehearsal.sh" "$artifacts" "$aggregate" "$evidence"
test "$run_status" -ne 0 || fail "rehearsal accepted movable image tags"
assert_contains "$output_file" "immutable ghcr.io"

linux_digest=bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb
mac_ref=ghcr.io/cirruslabs/macos-sequoia-base@sha256:$mac_digest
linux_ref=ghcr.io/cirruslabs/ubuntu@sha256:$linux_digest

refresh_closed_manifest() {
    : > "$artifacts/SHA256SUMS"
    for artifact in "$artifacts"/*; do
        name=${artifact##*/}
        test "$name" = SHA256SUMS && continue
        hash=$(shasum -a 256 "$artifact" | awk '{ print $1 }')
        printf '%s  %s\n' "$hash" "$name" >> "$artifacts/SHA256SUMS"
    done
}

assert_rehearsal_rejected_before_pull() {
    rejected_evidence=$1
    : > "$call_log"
    run_capture env PATH="$fake_bin:/usr/bin:/bin" CALL_LOG="$call_log" \
        TART_STATE="$tart_state" \
        CAR_GO_CLEAN_TART_MACOS_IMAGE="$mac_ref" \
        CAR_GO_CLEAN_TART_LINUX_IMAGE="$linux_ref" \
        CAR_GO_CLEAN_ACCEPTANCE_VERSION=0.4.0 \
        CAR_GO_CLEAN_ACCEPTANCE_SHA="$FAKE_EXACT_SHA" \
        "$root/scripts/release/tart-rehearsal.sh" \
        "$artifacts" "$aggregate" "$rejected_evidence"
    test "$run_status" -ne 0 || fail "rehearsal accepted invalid bound inputs"
    assert_not_contains "$call_log" "tart pull"
}

# Reused output directories are rejected so stale phase evidence cannot be
# mistaken for the current run.
stale_evidence=$work_dir/stale-evidence
mkdir "$stale_evidence"
printf 'old transcript\n' > "$stale_evidence/pre-reboot-transcript.log"
assert_rehearsal_rejected_before_pull "$stale_evidence"
assert_contains "$output_file" "must be new and absent"

# The artifact directory is closed: unlisted files, subdirectories, and
# symlinks are rejected before any VM source is pulled.
printf 'unlisted\n' > "$artifacts/unlisted"
assert_rehearsal_rejected_before_pull "$work_dir/reject-unlisted"
assert_contains "$output_file" "closed allowlist"
rm -f "$artifacts/unlisted"

mkdir "$artifacts/nested"
assert_rehearsal_rejected_before_pull "$work_dir/reject-directory"
assert_contains "$output_file" "not a regular non-symlink"
rmdir "$artifacts/nested"

ln -s car-go-clean-installer.sh "$artifacts/linked"
assert_rehearsal_rejected_before_pull "$work_dir/reject-symlink"
assert_contains "$output_file" "not a regular non-symlink"
rm -f "$artifacts/linked"

# Matching the outer manifest is insufficient when the exact checkout,
# aggregate provenance, or preserved official rustup proof disagrees.
cp "$artifacts/SHA256SUMS" "$work_dir/SHA256SUMS.saved"
printf '\n# altered\n' >> "$artifacts/acceptance.sh"
refresh_closed_manifest
assert_rehearsal_rejected_before_pull "$work_dir/reject-harness"
assert_contains "$output_file" "not byte-identical"
cp "$root/scripts/release/acceptance.sh" "$artifacts/acceptance.sh"
cp "$work_dir/SHA256SUMS.saved" "$artifacts/SHA256SUMS"

cp "$artifacts/SHA256SUMS" "$work_dir/SHA256SUMS.saved"
cp "$artifacts/car-go-clean.rb" "$work_dir/car-go-clean.rb.saved"
printf '\n# altered\n' >> "$artifacts/car-go-clean.rb"
refresh_closed_manifest
assert_rehearsal_rejected_before_pull "$work_dir/reject-formula"
assert_contains "$output_file" "aggregate-bound render"
cp "$work_dir/car-go-clean.rb.saved" "$artifacts/car-go-clean.rb"
cp "$work_dir/SHA256SUMS.saved" "$artifacts/SHA256SUMS"

cp "$aggregate/aggregate-inventory.json" "$work_dir/aggregate-inventory.saved"
sed "s/$FAKE_EXACT_SHA/0000000000000000000000000000000000000000/" \
    "$work_dir/aggregate-inventory.saved" \
    > "$aggregate/aggregate-inventory.json"
assert_rehearsal_rejected_before_pull "$work_dir/reject-aggregate-sha"
assert_contains "$output_file" "aggregate inventory"
cp "$work_dir/aggregate-inventory.saved" "$aggregate/aggregate-inventory.json"

cp "$artifacts/SHA256SUMS" "$work_dir/SHA256SUMS.saved"
printf '%064d  rustup-init\n' 0 \
    > "$artifacts/rustup-init-aarch64-apple-darwin.sha256"
refresh_closed_manifest
assert_rehearsal_rejected_before_pull "$work_dir/reject-rustup-proof"
assert_contains "$output_file" "official rustup checksum proof"
rustup_hash=$(shasum -a 256 \
    "$artifacts/rustup-init-aarch64-apple-darwin" | awk '{ print $1 }')
printf '%s  rustup-init\n' "$rustup_hash" \
    > "$artifacts/rustup-init-aarch64-apple-darwin.sha256"
cp "$work_dir/SHA256SUMS.saved" "$artifacts/SHA256SUMS"

# The normal release-input validator is mandatory; a dirty exact-SHA checkout
# cannot be relabeled as a clean rehearsal.
: > "$call_log"
run_capture env PATH="$fake_bin:/usr/bin:/bin" CALL_LOG="$call_log" \
    TART_STATE="$tart_state" FAKE_GIT_DIRTY=1 \
    CAR_GO_CLEAN_TART_MACOS_IMAGE="$mac_ref" \
    CAR_GO_CLEAN_TART_LINUX_IMAGE="$linux_ref" \
    CAR_GO_CLEAN_ACCEPTANCE_VERSION=0.4.0 \
    CAR_GO_CLEAN_ACCEPTANCE_SHA="$FAKE_EXACT_SHA" \
    "$root/scripts/release/tart-rehearsal.sh" \
    "$artifacts" "$aggregate" "$work_dir/reject-dirty-checkout"
test "$run_status" -ne 0 || fail "rehearsal accepted a dirty exact-SHA checkout"
assert_contains "$output_file" "release checkout is dirty"
assert_not_contains "$call_log" "tart pull"

: > "$call_log"
rm -rf "$evidence"
FAIL_ACCEPTANCE_HOST=linux
export FAIL_ACCEPTANCE_HOST
run_capture env PATH="$fake_bin:/usr/bin:/bin" CALL_LOG="$call_log" \
    TART_STATE="$tart_state" FAIL_ACCEPTANCE_HOST="$FAIL_ACCEPTANCE_HOST" \
    CAR_GO_CLEAN_TART_MACOS_IMAGE="$mac_ref" \
    CAR_GO_CLEAN_TART_LINUX_IMAGE="$linux_ref" \
    CAR_GO_CLEAN_ACCEPTANCE_VERSION=0.4.0 \
    CAR_GO_CLEAN_ACCEPTANCE_SHA="$FAKE_EXACT_SHA" \
    "$root/scripts/release/tart-rehearsal.sh" "$artifacts" "$aggregate" "$evidence"
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
test -f "$evidence/macos/pre-reboot/pre-reboot-transcript.log"
test -f "$evidence/macos/post-reboot/post-reboot-transcript.log"
test -f "$evidence/linux/pre-reboot/pre-reboot-transcript.log"
test -f "$evidence/linux/post-reboot/post-reboot-transcript.log"
test "$(cat "$evidence/macos/pre-reboot-boot-identity.txt")" != \
    "$(cat "$evidence/macos/post-reboot-boot-identity.txt")"
test "$(cat "$evidence/linux/pre-reboot-boot-identity.txt")" != \
    "$(cat "$evidence/linux/post-reboot-boot-identity.txt")"
assert_contains "$evidence/source-map.tsv" "$mac_ref"
assert_contains "$evidence/source-map.tsv" "$linux_ref"
assert_contains "$evidence/macos/tool-inventory.txt" "rustc=rustc 1.95.0"
assert_contains "$evidence/macos/tool-inventory.txt" "cargo=cargo 1.95.0"
assert_contains "$evidence/macos/tool-inventory.txt" "brew=Homebrew"
assert_contains "$evidence/linux/tool-inventory.txt" "cc=cc"
assert_not_contains "$evidence/macos/tool-inventory.txt" "/home/admin"
assert_not_contains "$evidence/linux/tool-inventory.txt" "/home/admin"
assert_contains "$call_log" "--default-toolchain 1.95.0 --profile minimal --no-modify-path"
assert_contains "$call_log" "PATH='/home/admin/.cargo/bin:"
pre_copy_line=$(grep -n 'scp .*macos/pre-reboot' "$call_log" |
    head -n 1 | cut -d : -f 1)
mac_reboot_line=$(grep -n 'ssh .*admin@192.0.2.10 sudo reboot' "$call_log" |
    head -n 1 | cut -d : -f 1)
test -n "$pre_copy_line" && test -n "$mac_reboot_line" &&
    test "$pre_copy_line" -lt "$mac_reboot_line" ||
    fail "pre-reboot evidence was not copied before reboot"

# Missing immutable-base prerequisites stop acceptance before it can produce a
# false guest PASS; no mutable package-manager bootstrap is attempted.
rm -f "$tart_state"/rebooted-* "$tart_state/rebooting"
: > "$call_log"
run_capture env PATH="$fake_bin:/usr/bin:/bin" CALL_LOG="$call_log" \
    TART_STATE="$tart_state" FAIL_ACCEPTANCE_HOST= \
    FAIL_GUEST_DEPENDENCY=linux \
    CAR_GO_CLEAN_TART_MACOS_IMAGE="$mac_ref" \
    CAR_GO_CLEAN_TART_LINUX_IMAGE="$linux_ref" \
    CAR_GO_CLEAN_ACCEPTANCE_VERSION=0.4.0 \
    CAR_GO_CLEAN_ACCEPTANCE_SHA="$FAKE_EXACT_SHA" \
    "$root/scripts/release/tart-rehearsal.sh" \
    "$artifacts" "$aggregate" "$work_dir/dependency-failure-evidence"
test "$run_status" -ne 0 || fail "missing guest dependency was hidden"
assert_contains "$output_file" "deterministic toolchain bootstrap/preflight failed"
if grep -E '^ssh .*admin@192\.0\.2\.20 .*acceptance\.sh.* pre-reboot$' \
    "$call_log" >/dev/null; then
    fail "Linux acceptance ran after dependency preflight failed"
fi
assert_not_contains "$call_log" "apt-get"
assert_not_contains "$call_log" "brew install"

# A failed pre-reboot copy blocks that VM's reboot; a transient disconnect
# without a changed boot identity is likewise insufficient.
rm -f "$tart_state"/rebooted-* "$tart_state/rebooting"
: > "$call_log"
run_capture env PATH="$fake_bin:/usr/bin:/bin" CALL_LOG="$call_log" \
    TART_STATE="$tart_state" FAIL_ACCEPTANCE_HOST= \
    FAIL_PRE_COPY_HOST=macos \
    CAR_GO_CLEAN_TART_MACOS_IMAGE="$mac_ref" \
    CAR_GO_CLEAN_TART_LINUX_IMAGE="$linux_ref" \
    CAR_GO_CLEAN_ACCEPTANCE_VERSION=0.4.0 \
    CAR_GO_CLEAN_ACCEPTANCE_SHA="$FAKE_EXACT_SHA" \
    "$root/scripts/release/tart-rehearsal.sh" \
    "$artifacts" "$aggregate" "$work_dir/pre-copy-failure-evidence"
test "$run_status" -ne 0 || fail "pre-reboot copy failure was hidden"
assert_not_contains "$call_log" "admin@192.0.2.10 sudo reboot"

rm -f "$tart_state"/rebooted-* "$tart_state/rebooting"
: > "$call_log"
run_capture env PATH="$fake_bin:/usr/bin:/bin" CALL_LOG="$call_log" \
    TART_STATE="$tart_state" FAIL_ACCEPTANCE_HOST= UNCHANGED_BOOT_ID=1 \
    CAR_GO_CLEAN_TART_MACOS_IMAGE="$mac_ref" \
    CAR_GO_CLEAN_TART_LINUX_IMAGE="$linux_ref" \
    CAR_GO_CLEAN_ACCEPTANCE_VERSION=0.4.0 \
    CAR_GO_CLEAN_ACCEPTANCE_SHA="$FAKE_EXACT_SHA" \
    "$root/scripts/release/tart-rehearsal.sh" \
    "$artifacts" "$aggregate" "$work_dir/unchanged-boot-evidence"
test "$run_status" -ne 0 || fail "unchanged boot identity was accepted"
assert_contains "$output_file" "boot identity did not change"
if grep -E '^ssh .*acceptance\.sh.* post-reboot$' "$call_log" >/dev/null; then
    fail "post-reboot acceptance ran without a changed boot identity"
fi

# Failure-only orchestration checkpoints bind at the early, middle, and late
# boundaries even though the other platform continues to preserve evidence.
for position in early middle late; do
    rm -f "$tart_state"/rebooted-* "$tart_state/rebooting"
    : > "$call_log"
    run_capture env PATH="$fake_bin:/usr/bin:/bin" CALL_LOG="$call_log" \
        TART_STATE="$tart_state" FAIL_ACCEPTANCE_HOST= \
        CAR_GO_CLEAN_TART_FAULT="macos:$position" \
        CAR_GO_CLEAN_TART_MACOS_IMAGE="$mac_ref" \
        CAR_GO_CLEAN_TART_LINUX_IMAGE="$linux_ref" \
        CAR_GO_CLEAN_ACCEPTANCE_VERSION=0.4.0 \
        CAR_GO_CLEAN_ACCEPTANCE_SHA="$FAKE_EXACT_SHA" \
        "$root/scripts/release/tart-rehearsal.sh" \
        "$artifacts" "$aggregate" "$work_dir/tart-fault-$position"
    test "$run_status" -ne 0 ||
        fail "Tart $position checkpoint was hidden"
    assert_contains "$output_file" "injected Tart rehearsal failure: macos:$position"
done

# Guest acceptance exercises real script branches against fake Cargo and
# car-go-clean binaries. A failing assertion still sanitizes and preserves the
# transcript.
fake_acceptance=$work_dir/fake-acceptance
fake_artifacts=$work_dir/fake-artifacts
fake_guest_home=$work_dir/guest-home
fake_service_state=$work_dir/fake-service-state
fake_review_path=$work_dir/fake-review-path
fake_cached_root=$work_dir/fake-cached-root
fake_error=$work_dir/fake-error
fake_current_cgc=$fake_acceptance/car-go-clean-current
mkdir -p "$fake_acceptance" "$fake_artifacts" "$fake_guest_home"
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
        no_scan=false
        review=
        json=false
        config_file=
        while test "$#" -gt 0; do
            case "$1" in
                --dry-run) dry=true; shift ;;
                --no-scan) no_scan=true; shift ;;
                --review) review=$2; shift 2 ;;
                --json) json=true; shift ;;
                --config) config_file=$2; shift 2 ;;
                --state-dir) shift 2 ;;
                *) shift ;;
            esac
        done
        if test "$dry" = true; then
            root=$(awk -F '"' '/^scan_dirs/ { print $2; exit }' "$config_file")
            if test "$no_scan" = true; then
                cached_root=$(cat "$FAKE_CACHED_ROOT" 2>/dev/null || :)
                if test -z "$cached_root" || test "$cached_root" != "$root"; then
                    printf 'Total projects: 0\nCleanable projects: 0\n'
                    printf 'No review ID was created because no valid matching discovery generation exists.\n'
                    exit 2
                fi
            else
                printf '%s\n' "$root" > "$FAKE_CACHED_ROOT"
            fi
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

cat > "$fake_acceptance/sleep" <<'EOF'
#!/bin/sh
exit 0
EOF

cat > "$fake_artifacts/car-go-clean-installer.sh" <<'EOF'
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

cat > "$fake_artifacts/car-go-clean-upgrade.sh" <<'EOF'
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
    case_dir=${CAR_GO_CLEAN_UPGRADE_STATE_DIR%/upgrade-state}
    rm -rf "$case_dir/project/sample/target"
    rm -f "$session"
    echo "Upgrade to car-go-clean 0.4.0 completed."
fi
EOF

for old_version in 0.2.0 0.3.0; do
    old_fixture=$fake_artifacts/car-go-clean-v$old_version-aarch64-unknown-linux-musl
    {
        printf '%s\n' '#!/bin/sh'
        # shellcheck disable=SC2016 # These variables belong in the generated fixture.
        printf 'if test "${1-}" = version; then echo "%s"; exit 0; fi\n' "$old_version"
        # shellcheck disable=SC2016 # These variables belong in the generated fixture.
        printf 'exec "$FAKE_CURRENT_CGC" "$@"\n'
    } > "$old_fixture"
    chmod +x "$old_fixture"
done

printf 'archive\n' > "$fake_artifacts/car-go-clean-aarch64-unknown-linux-musl.tar.xz"
printf 'fixture  car-go-clean-aarch64-unknown-linux-musl.tar.xz\n' \
    > "$fake_artifacts/car-go-clean-aarch64-unknown-linux-musl.tar.xz.sha256"
printf 'fixture checksums\n' > "$fake_artifacts/car-go-clean-shell-assets.sha256"
cat > "$fake_artifacts/car-go-clean.rb" <<'EOF'
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
    "$fake_acceptance/systemctl" "$fake_acceptance/sleep" \
    "$fake_artifacts/car-go-clean-installer.sh" \
    "$fake_artifacts/car-go-clean-upgrade.sh"

# The guest independently revalidates the exact closed payload copied by the
# host. Populate both architecture fixtures even though this fake guest is Linux.
cp "$root/scripts/release/acceptance.sh" "$fake_artifacts/acceptance.sh"
printf 'archive mac\n' > "$fake_artifacts/car-go-clean-aarch64-apple-darwin.tar.xz"
printf 'fixture  car-go-clean-aarch64-apple-darwin.tar.xz\n' \
    > "$fake_artifacts/car-go-clean-aarch64-apple-darwin.tar.xz.sha256"
for old_version in 0.2.0 0.3.0; do
    cp "$fake_artifacts/car-go-clean-v$old_version-aarch64-unknown-linux-musl" \
        "$fake_artifacts/car-go-clean-v$old_version-aarch64-apple-darwin"
done
for target in aarch64-apple-darwin aarch64-unknown-linux-gnu; do
    rustup=rustup-init-$target
    printf '#!/bin/sh\nexit 0\n' > "$fake_artifacts/$rustup"
    rustup_hash=$(shasum -a 256 "$fake_artifacts/$rustup" | awk '{ print $1 }')
    printf '%s  rustup-init\n' "$rustup_hash" \
        > "$fake_artifacts/$rustup.sha256"
done
: > "$fake_artifacts/SHA256SUMS"
for artifact in "$fake_artifacts"/*; do
    name=${artifact##*/}
    test "$name" = SHA256SUMS && continue
    hash=$(shasum -a 256 "$artifact" | awk '{ print $1 }')
    printf '%s  %s\n' "$hash" "$name" >> "$fake_artifacts/SHA256SUMS"
done

acceptance_evidence=$work_dir/acceptance-evidence
: > "$call_log"
for phase in pre-reboot post-reboot; do
    run_capture env PATH="$fake_acceptance:/usr/bin:/bin" HOME="$fake_guest_home" \
        CALL_LOG="$call_log" FAKE_CURRENT_CGC="$fake_current_cgc" \
        FAKE_SERVICE_STATE="$fake_service_state" \
        FAKE_REVIEW_PATH="$fake_review_path" FAKE_CACHED_ROOT="$fake_cached_root" \
        FAKE_ERROR="$fake_error" \
        FAKE_ACCEPTANCE_ARTIFACTS="$fake_artifacts" \
        CAR_GO_CLEAN_ACCEPTANCE_VERSION=0.4.0 \
        CAR_GO_CLEAN_ACCEPTANCE_SHA=18e2b772698b5f9b67da64c4ad299beacfe219e9 \
        "$root/scripts/release/acceptance.sh" \
        "$fake_artifacts" "$acceptance_evidence" "$phase"
    if test "$run_status" -ne 0; then
        test -f "$acceptance_evidence/$phase-transcript.log" &&
            cat "$acceptance_evidence/$phase-transcript.log" >&2
        find "$fake_guest_home/car-go-clean-v040-acceptance-work" \
            -name preview.out -o -name execute.out 2>/dev/null |
            while IFS= read -r diagnostic; do
                printf '%s\n' "--- $diagnostic" >&2
                cat "$diagnostic" >&2
            done
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
    FAKE_REVIEW_PATH="$fake_review_path" FAKE_CACHED_ROOT="$fake_cached_root" \
    FAKE_ERROR="$fake_error" \
    FAKE_ACCEPTANCE_ARTIFACTS="$fake_artifacts" \
    CAR_GO_CLEAN_ACCEPTANCE_VERSION=0.4.0 \
    CAR_GO_CLEAN_ACCEPTANCE_SHA=18e2b772698b5f9b67da64c4ad299beacfe219e9 \
    FAIL_ACCEPTANCE_STEP=cargo-failure \
    "$root/scripts/release/acceptance.sh" \
    "$fake_artifacts" "$failed_evidence" pre-reboot
test "$run_status" -ne 0 || fail "guest acceptance hid a failed assertion"
test -s "$failed_evidence/pre-reboot-transcript.log"
assert_contains "$failed_evidence/milestones.tsv" "cargo-failure	FAIL"
assert_not_contains "$failed_evidence/pre-reboot-transcript.log" "$work_dir"

# Failure-only checkpoints bind at the beginning, middle, and end of every
# composite acceptance step. Fake sleep keeps the exhaustive matrix quick.
run_acceptance_fixture() {
    fixture_phase=$1
    fixture_evidence=$2
    fixture_fault=${3-}
    run_capture env PATH="$fake_acceptance:/usr/bin:/bin" HOME="$fake_guest_home" \
        CALL_LOG="$call_log" FAKE_CURRENT_CGC="$fake_current_cgc" \
        FAKE_SERVICE_STATE="$fake_service_state" \
        FAKE_REVIEW_PATH="$fake_review_path" \
        FAKE_CACHED_ROOT="$fake_cached_root" FAKE_ERROR="$fake_error" \
        FAKE_ACCEPTANCE_ARTIFACTS="$fake_artifacts" \
        CAR_GO_CLEAN_ACCEPTANCE_VERSION=0.4.0 \
        CAR_GO_CLEAN_ACCEPTANCE_SHA="$FAKE_EXACT_SHA" \
        CAR_GO_CLEAN_ACCEPTANCE_FAULT="$fixture_fault" \
        "$root/scripts/release/acceptance.sh" \
        "$fake_artifacts" "$fixture_evidence" "$fixture_phase"
}

pre_steps="shell-install formula-install version-health disposable-build dry-run review no-scan narrowed-scope cargo-failure incomplete-scan complete-scan strict-config migration-roundtrip service-pre-reboot"
post_steps="service-post-reboot upgrade-matrix macos-library-privacy"
for position in early middle late; do
    for milestone in $pre_steps; do
        printf 'no no no\n' > "$fake_service_state"
        rm -f "$fake_cached_root" "$fake_review_path"
        fault_evidence=$work_dir/fault-$milestone-$position
        run_acceptance_fixture pre-reboot "$fault_evidence" \
            "$milestone:$position"
        test "$run_status" -ne 0 ||
            fail "$milestone:$position acceptance checkpoint was ignored"
        grep -F -- "$milestone	FAIL" "$fault_evidence/milestones.tsv" >/dev/null ||
            fail "$milestone:$position did not record FAIL"
        if grep -Fqx "$milestone	PASS" "$fault_evidence/milestones.tsv"; then
            fail "$milestone:$position also recorded PASS"
        fi
    done
    for milestone in $post_steps; do
        printf 'no no no\n' > "$fake_service_state"
        rm -f "$fake_cached_root" "$fake_review_path"
        fault_evidence=$work_dir/fault-$milestone-$position
        run_acceptance_fixture pre-reboot "$fault_evidence"
        assert_status "$run_status" 0
        run_acceptance_fixture post-reboot "$fault_evidence" \
            "$milestone:$position"
        test "$run_status" -ne 0 ||
            fail "$milestone:$position acceptance checkpoint was ignored"
        grep -F -- "$milestone	FAIL" "$fault_evidence/milestones.tsv" >/dev/null ||
            fail "$milestone:$position did not record FAIL"
        if grep -Fqx "$milestone	PASS" "$fault_evidence/milestones.tsv"; then
            fail "$milestone:$position also recorded PASS"
        fi
    done
done

# macOS fixture setup must explicitly clear launchd's persistent disabled
# record before both active and stopped old-service installs.
# shellcheck disable=SC2016 # The literal source expression is the assertion.
assert_contains "$root/scripts/release/acceptance.sh" \
    'launchctl enable "$label"'
test "$(grep -c 'native_enable_old_service_fixture' \
    "$root/scripts/release/acceptance.sh")" -eq 3 ||
    fail "launchd enable reset is not wired into both fixture install branches"

echo "release acceptance harness tests passed"
