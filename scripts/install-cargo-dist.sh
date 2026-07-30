#!/bin/sh
set -eu

installer_url=https://github.com/axodotdev/cargo-dist/releases/download/v0.32.0/cargo-dist-installer.sh
expected_sha256=b657cf8c04a8b7bc28f39d220f7e6dd11bbd2bdb072c552262bd9ccf597261b5
work_dir=$(mktemp -d)
trap 'rm -rf "$work_dir"' EXIT HUP INT TERM
installer="$work_dir/cargo-dist-installer.sh"

curl --proto '=https' --tlsv1.2 -fsSL -o "$installer" "$installer_url"

if command -v shasum >/dev/null 2>&1
then
    actual_sha256=$(shasum -a 256 "$installer" | awk '{ print $1 }')
elif command -v sha256sum >/dev/null 2>&1
then
    actual_sha256=$(sha256sum "$installer" | awk '{ print $1 }')
else
    echo "neither shasum nor sha256sum is available" >&2
    exit 1
fi

if test "$actual_sha256" != "$expected_sha256"
then
    echo "cargo-dist installer checksum verification failed" >&2
    exit 1
fi

sh "$installer"
dist --version
