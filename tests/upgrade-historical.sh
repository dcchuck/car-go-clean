#!/bin/sh
set -eu

root=$(CDPATH='' cd -- "$(dirname -- "$0")/.." && pwd)
work_dir=$(mktemp -d)

cleanup() {
    rm -rf "$work_dir"
}
trap cleanup EXIT HUP INT TERM

assert_historical_release() {
    tag=$1
    version=$2
    commit=$3
    source_dir=$work_dir/$tag
    target_dir=$work_dir/target-$tag

    resolved_commit=$(git -C "$root" rev-parse "$tag^{commit}")
    test "$resolved_commit" = "$commit" || {
        echo "$tag resolved to $resolved_commit, expected $commit" >&2
        exit 1
    }
    test "$(git -C "$root" rev-parse "$commit^{commit}")" = "$commit"

    mkdir -p "$source_dir"
    git -C "$root" archive "$commit" | tar -x -C "$source_dir"
    source_version=$(awk -F'"' '
        /^\[package\]$/ { package = 1; next }
        package && /^version = "/ { print $2; exit }
    ' "$source_dir/Cargo.toml")
    test "$source_version" = "$version"

    CARGO_TARGET_DIR=$target_dir \
        mise exec rust@1.95.0 -- \
        cargo build --locked --manifest-path "$source_dir/Cargo.toml"
    historical_binary=$target_dir/debug/car-go-clean
    test -x "$historical_binary"
    test "$("$historical_binary" version)" = "$version"

    service_help=$("$historical_binary" service --help)
    printf '%s\n' "$service_help" | grep -F 'restart' >/dev/null
    for unsupported_verb in start stop
    do
        unsupported_output=$work_dir/$tag-$unsupported_verb.out
        if "$historical_binary" service "$unsupported_verb" \
            > "$unsupported_output" 2>&1; then
            echo "$tag unexpectedly accepted service $unsupported_verb" >&2
            exit 1
        else
            unsupported_status=$?
        fi
        test "$unsupported_status" -eq 2
        grep -F "$unsupported_verb" "$unsupported_output" >/dev/null
    done
}

assert_historical_release \
    v0.2.0 \
    0.2.0 \
    8bb54d6de929af6bb139acd5bd36ef7c12229afc
assert_historical_release \
    v0.3.0 \
    0.3.0 \
    75529c45c6e1b11dc2de0b41023c3baff23ec28b
