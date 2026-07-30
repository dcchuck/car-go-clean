#!/bin/sh
set -eu

root=$(CDPATH='' cd -- "$(dirname -- "$0")/.." && pwd)
installer="$root/packaging/release/car-go-clean-installer.sh"
upgrade="$root/packaging/release/car-go-clean-upgrade.sh"
work_dir=$(mktemp -d)
fake_bin="$work_dir/bin"
fixture_dir="$work_dir/fixture"
fixture_archive="$work_dir/car-go-clean.tar.xz"
curl_log="$work_dir/curl.log"
expected_hash=fixture-sha256

cleanup() {
    rm -rf "$work_dir"
}
trap cleanup EXIT HUP INT TERM

sh -n "$installer"
sh -n "$upgrade"

mkdir -p "$fake_bin" "$fixture_dir" "$work_dir/home"
cat > "$fixture_dir/car-go-clean" <<'EOF'
#!/bin/sh
case "${1-}" in
    version) printf '%s\n' 0.2.0 ;;
    *) exit 64 ;;
esac
EOF
chmod +x "$fixture_dir/car-go-clean"
tar -C "$fixture_dir" -cJf "$fixture_archive" car-go-clean

cat > "$fake_bin/uname" <<'EOF'
#!/bin/sh
case "$1" in
    -s) printf '%s\n' "$TEST_UNAME_S" ;;
    -m) printf '%s\n' "$TEST_UNAME_M" ;;
    *) exit 1 ;;
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

case "$url" in
    */releases/latest)
        printf '%s' latest-meta > "$CURL_LOG"
        printf '%s' https://github.com/dcchuck/car-go-clean/releases/tag/v0.2.0
        ;;
    */car-go-clean-*.tar.xz)
        if [ -s "$CURL_LOG" ]; then
            printf ' ' >> "$CURL_LOG"
        fi
        printf '%s' "$url" >> "$CURL_LOG"
        cp "$FIXTURE_ARCHIVE" "$output"
        ;;
    */car-go-clean-*.tar.xz.sha256)
        checksum_name=${url##*/}
        archive_name=${checksum_name%.sha256}
        if [ -s "$CURL_LOG" ]; then
            printf ' ' >> "$CURL_LOG"
        fi
        printf '%s' "$url" >> "$CURL_LOG"
        printf '%s  %s\n' "$EXPECTED_HASH" "$archive_name" > "$output"
        ;;
    *)
        echo "unexpected curl URL: $url" >&2
        exit 1
        ;;
esac
EOF

cat > "$fake_bin/shasum" <<'EOF'
#!/bin/sh
set -eu

for argument do
    file=$argument
done

case "${CHECKSUM_MODE-}" in
    wrong) printf '%s  %s\n' wrong-sha256 "$file" ;;
    *) printf '%s  %s\n' "$EXPECTED_HASH" "$file" ;;
esac
EOF
cp "$fake_bin/shasum" "$fake_bin/sha256sum"
chmod +x "$fake_bin/uname" "$fake_bin/curl" "$fake_bin/shasum" "$fake_bin/sha256sum"

run_installer() {
    PATH="$fake_bin:$PATH" \
    HOME="$work_dir/home" \
    CURL_LOG="$curl_log" \
    FIXTURE_ARCHIVE="$fixture_archive" \
    EXPECTED_HASH="$expected_hash" \
    TEST_UNAME_S="${TEST_UNAME_S-Darwin}" \
    TEST_UNAME_M="${TEST_UNAME_M-arm64}" \
    "$installer" "$@"
}

: > "$curl_log"
if run_installer \
    --download-base-url https://artifacts.example.invalid/releases/v0.2.0 \
    --install-dir "$work_dir/missing-version-install"
then
    echo "download base URL accepted without an explicit version" >&2
    exit 1
fi
test ! -s "$curl_log"

: > "$curl_log"
if run_installer \
    --version 0.2.0 \
    --download-base-url http://127.0.0.1:8000 \
    --install-dir "$work_dir/insecure-without-opt-in"
then
    echo "insecure download base URL accepted without the test-only opt-in" >&2
    exit 1
fi
test ! -s "$curl_log"

: > "$curl_log"
override_dir="$work_dir/override-install"
run_installer \
    --version 0.2.0 \
    --download-base-url https://artifacts.example.invalid/releases/v0.2.0 \
    --install-dir "$override_dir"
cmp "$fixture_dir/car-go-clean" "$override_dir/car-go-clean"
test "$(cat "$curl_log")" = "https://artifacts.example.invalid/releases/v0.2.0/car-go-clean-aarch64-apple-darwin.tar.xz https://artifacts.example.invalid/releases/v0.2.0/car-go-clean-aarch64-apple-darwin.tar.xz.sha256"

: > "$curl_log"
loopback_dir="$work_dir/loopback-install"
CAR_GO_CLEAN_ALLOW_INSECURE_TEST_URL=1 run_installer \
    --version 0.2.0 \
    --download-base-url http://127.0.0.1:8000 \
    --install-dir "$loopback_dir"
cmp "$fixture_dir/car-go-clean" "$loopback_dir/car-go-clean"
test "$(cat "$curl_log")" = "http://127.0.0.1:8000/car-go-clean-aarch64-apple-darwin.tar.xz http://127.0.0.1:8000/car-go-clean-aarch64-apple-darwin.tar.xz.sha256"

: > "$curl_log"
localhost_dir="$work_dir/localhost-install"
CAR_GO_CLEAN_ALLOW_INSECURE_TEST_URL=1 run_installer \
    --version 0.2.0 \
    --download-base-url http://localhost/rehearsal/assets \
    --install-dir "$localhost_dir"
cmp "$fixture_dir/car-go-clean" "$localhost_dir/car-go-clean"
test "$(cat "$curl_log")" = "http://localhost/rehearsal/assets/car-go-clean-aarch64-apple-darwin.tar.xz http://localhost/rehearsal/assets/car-go-clean-aarch64-apple-darwin.tar.xz.sha256"

: > "$curl_log"
ipv6_loopback_dir="$work_dir/ipv6-loopback-install"
CAR_GO_CLEAN_ALLOW_INSECURE_TEST_URL=1 run_installer \
    --version 0.2.0 \
    --download-base-url 'http://[::1]:8001/rehearsal/assets' \
    --install-dir "$ipv6_loopback_dir"
cmp "$fixture_dir/car-go-clean" "$ipv6_loopback_dir/car-go-clean"
test "$(cat "$curl_log")" = "http://[::1]:8001/rehearsal/assets/car-go-clean-aarch64-apple-darwin.tar.xz http://[::1]:8001/rehearsal/assets/car-go-clean-aarch64-apple-darwin.tar.xz.sha256"

for userinfo_url in \
    http://127.0.0.1:80@attacker.example.invalid/assets \
    http://localhost:80@attacker.example.invalid/assets
do
    : > "$curl_log"
    if CAR_GO_CLEAN_ALLOW_INSECURE_TEST_URL=1 run_installer \
        --version 0.2.0 \
        --download-base-url "$userinfo_url" \
        --install-dir "$work_dir/userinfo-install"
    then
        echo "userinfo download URL accepted as loopback: $userinfo_url" >&2
        exit 1
    fi
    test ! -s "$curl_log"
done

for malformed_loopback_url in \
    http://127.0.0.1:not-a-port/assets \
    http://localhost:80:90/assets \
    'http://[::1]:not-a-port/assets'
do
    : > "$curl_log"
    if CAR_GO_CLEAN_ALLOW_INSECURE_TEST_URL=1 run_installer \
        --version 0.2.0 \
        --download-base-url "$malformed_loopback_url" \
        --install-dir "$work_dir/malformed-loopback-install"
    then
        echo "malformed loopback URL accepted: $malformed_loopback_url" >&2
        exit 1
    fi
    test ! -s "$curl_log"
done

: > "$curl_log"
if CAR_GO_CLEAN_ALLOW_INSECURE_TEST_URL=1 run_installer \
    --version 0.2.0 \
    --download-base-url http://artifacts.example.invalid/releases/v0.2.0 \
    --install-dir "$work_dir/non-loopback-install"
then
    echo "non-loopback insecure URL accepted with the test-only opt-in" >&2
    exit 1
fi
test ! -s "$curl_log"

file_artifacts="$work_dir/file-artifacts"
file_bin="$work_dir/file-bin"
file_install="$work_dir/file-install"
file_archive=car-go-clean-aarch64-apple-darwin.tar.xz
mkdir -p "$file_artifacts" "$file_bin"
cp "$fixture_archive" "$file_artifacts/$file_archive"
file_hash=$(shasum -a 256 "$file_artifacts/$file_archive" | awk '{ print $1 }')
printf '%s  %s\n' "$file_hash" "$file_archive" > "$file_artifacts/$file_archive.sha256"
cp "$fake_bin/uname" "$file_bin/uname"
chmod +x "$file_bin/uname"
if ! CAR_GO_CLEAN_ALLOW_INSECURE_TEST_URL=1 \
    PATH="$file_bin:/usr/bin:/bin" \
    HOME="$work_dir/home" \
    TEST_UNAME_S=Darwin \
    TEST_UNAME_M=arm64 \
    "$installer" \
        --version 0.2.0 \
        --download-base-url "file://$file_artifacts" \
        --install-dir "$file_install"
then
    echo "absolute file-backed rehearsal URL was not usable" >&2
    exit 1
fi
cmp "$fixture_dir/car-go-clean" "$file_install/car-go-clean"

for malformed_version in \
    1 \
    1.2 \
    1.2.3.4 \
    1.2.3-rc1 \
    1.2.3/../../escape \
    '1.2.3 extra' \
    latest \
    .2.3 \
    1..3 \
    1.2.
do
    : > "$curl_log"
    if run_installer --version "$malformed_version" --install-dir "$work_dir/malformed-install"; then
        echo "accepted malformed version: $malformed_version" >&2
        exit 1
    fi
    test ! -s "$curl_log"
done

install_dir="$work_dir/default-install"
run_installer --install-dir "$install_dir"
cmp "$fixture_dir/car-go-clean" "$install_dir/car-go-clean"
test "$(cat "$curl_log")" = "latest-meta https://github.com/dcchuck/car-go-clean/releases/download/v0.2.0/car-go-clean-aarch64-apple-darwin.tar.xz https://github.com/dcchuck/car-go-clean/releases/download/v0.2.0/car-go-clean-aarch64-apple-darwin.tar.xz.sha256"

: > "$curl_log"
run_installer --version 0.2.0
(
    PATH="$work_dir/home/.local/bin:$PATH"
    export PATH
    hash -r
    test "$(command -v car-go-clean)" = "$work_dir/home/.local/bin/car-go-clean"
    test "$(car-go-clean version)" = 0.2.0
)

: > "$curl_log"
versioned_dir="$work_dir/versioned-install"
run_installer --version 0.2.0 --install-dir "$versioned_dir"
cmp "$fixture_dir/car-go-clean" "$versioned_dir/car-go-clean"
grep -qx 'https://github.com/dcchuck/car-go-clean/releases/download/v0.2.0/car-go-clean-aarch64-apple-darwin.tar.xz https://github.com/dcchuck/car-go-clean/releases/download/v0.2.0/car-go-clean-aarch64-apple-darwin.tar.xz.sha256' "$curl_log"

failed_dir="$work_dir/failed-install"
mkdir -p "$failed_dir"
printf '%s' 'old binary' > "$failed_dir/car-go-clean"
if CHECKSUM_MODE=wrong run_installer --install-dir "$failed_dir"; then
    exit 1
fi
test "$(cat "$failed_dir/car-go-clean")" = "old binary"

: > "$curl_log"
linux_dir="$work_dir/linux-install"
CHECKSUM_MODE='' TEST_UNAME_S=Linux TEST_UNAME_M=x86_64 run_installer --install-dir "$linux_dir"
cmp "$fixture_dir/car-go-clean" "$linux_dir/car-go-clean"
test "$(cat "$curl_log")" = "latest-meta https://github.com/dcchuck/car-go-clean/releases/download/v0.2.0/car-go-clean-x86_64-unknown-linux-musl.tar.xz https://github.com/dcchuck/car-go-clean/releases/download/v0.2.0/car-go-clean-x86_64-unknown-linux-musl.tar.xz.sha256"

: > "$curl_log"
if TEST_UNAME_S=FreeBSD TEST_UNAME_M=amd64 run_installer --install-dir "$work_dir/unsupported-install"; then
    exit 1
fi
test ! -s "$curl_log"
