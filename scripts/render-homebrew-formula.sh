#!/bin/sh
set -eu

usage() {
    echo "usage: $0 vX.Y.Z ARTIFACT_DIR OUTPUT" >&2
    exit 2
}

test "$#" -eq 3 || usage
tag=$1
artifact_dir=$2
output=$3
repo_root=$(CDPATH='' cd -- "$(dirname -- "$0")/.." && pwd)
template="$repo_root/packaging/release/homebrew/car-go-clean.rb.in"
verifier="$repo_root/scripts/verify-release-assets.sh"

inventory=$("$verifier" "$tag" "$artifact_dir")

hash_for() {
    archive=$1
    printf '%s\n' "$inventory" | awk -F '	' -v archive="$archive" '
        $1 == archive {
            print $2
            found++
        }
        END {
            if (found != 1) {
                exit 1
            }
        }
    '
}

aarch64_apple_sha256=$(hash_for car-go-clean-aarch64-apple-darwin.tar.xz)
x86_64_apple_sha256=$(hash_for car-go-clean-x86_64-apple-darwin.tar.xz)
aarch64_linux_sha256=$(hash_for car-go-clean-aarch64-unknown-linux-musl.tar.xz)
x86_64_linux_sha256=$(hash_for car-go-clean-x86_64-unknown-linux-musl.tar.xz)

tag_placeholder_count=$(grep -o __TAG__ "$template" | awk 'END { print NR + 0 }')
if test "$tag_placeholder_count" -ne 4
then
    echo "formula template must contain one __TAG__ for each release URL" >&2
    exit 1
fi

for placeholder in \
    __AARCH64_APPLE_SHA256__ \
    __X86_64_APPLE_SHA256__ \
    __AARCH64_LINUX_SHA256__ \
    __X86_64_LINUX_SHA256__
do
    count=$(grep -o "$placeholder" "$template" | awk 'END { print NR + 0 }')
    if test "$count" -ne 1
    then
        echo "formula template must contain $placeholder exactly once" >&2
        exit 1
    fi
done

output_dir=$(dirname "$output")
test -d "$output_dir" || {
    echo "formula output directory does not exist: $output_dir" >&2
    exit 1
}
temporary=$(mktemp "$output.tmp.XXXXXX")
trap 'rm -f "$temporary"' EXIT HUP INT TERM

sed \
    -e "s/__TAG__/$tag/" \
    -e "s/__AARCH64_APPLE_SHA256__/$aarch64_apple_sha256/" \
    -e "s/__X86_64_APPLE_SHA256__/$x86_64_apple_sha256/" \
    -e "s/__AARCH64_LINUX_SHA256__/$aarch64_linux_sha256/" \
    -e "s/__X86_64_LINUX_SHA256__/$x86_64_linux_sha256/" \
    "$template" > "$temporary"

if grep -Eq '__[A-Z0-9_]+__' "$temporary"
then
    echo "rendered formula contains unresolved placeholders" >&2
    exit 1
fi

mv "$temporary" "$output"
trap - EXIT HUP INT TERM
