#!/bin/sh
set -eu

root=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
installer="$root/packaging/release/car-go-clean-installer.sh"
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

mkdir -p "$fake_bin" "$fixture_dir" "$work_dir/home"
printf '%s' 'new binary' > "$fixture_dir/car-go-clean"
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
test "$(cat "$install_dir/car-go-clean")" = "new binary"
test "$(cat "$curl_log")" = "latest-meta https://github.com/dcchuck/car-go-clean/releases/download/v0.2.0/car-go-clean-aarch64-apple-darwin.tar.xz https://github.com/dcchuck/car-go-clean/releases/download/v0.2.0/car-go-clean-aarch64-apple-darwin.tar.xz.sha256"

: > "$curl_log"
versioned_dir="$work_dir/versioned-install"
run_installer --version 0.2.0 --install-dir "$versioned_dir"
test "$(cat "$versioned_dir/car-go-clean")" = "new binary"
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
CHECKSUM_MODE= TEST_UNAME_S=Linux TEST_UNAME_M=x86_64 run_installer --install-dir "$linux_dir"
test "$(cat "$linux_dir/car-go-clean")" = "new binary"
test "$(cat "$curl_log")" = "latest-meta https://github.com/dcchuck/car-go-clean/releases/download/v0.2.0/car-go-clean-x86_64-unknown-linux-musl.tar.xz https://github.com/dcchuck/car-go-clean/releases/download/v0.2.0/car-go-clean-x86_64-unknown-linux-musl.tar.xz.sha256"

: > "$curl_log"
if TEST_UNAME_S=FreeBSD TEST_UNAME_M=amd64 run_installer --install-dir "$work_dir/unsupported-install"; then
    exit 1
fi
test ! -s "$curl_log"
