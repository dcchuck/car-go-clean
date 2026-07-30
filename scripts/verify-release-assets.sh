#!/bin/sh
set -eu

usage() {
    echo "usage: $0 vX.Y.Z ARTIFACT_DIR" >&2
    exit 2
}

test "$#" -eq 2 || usage
tag=$1
artifact_dir=$2
test -d "$artifact_dir" || {
    echo "artifact directory does not exist: $artifact_dir" >&2
    exit 1
}

case "$tag" in
    v[0-9]*.[0-9]*.[0-9]*) ;;
    *) usage ;;
esac
version=${tag#v}
case "$version" in
    ''|*[!0-9.]*) usage ;;
esac
major=${version%%.*}
remainder=${version#*.}
minor=${remainder%%.*}
patch=${remainder#*.}
if test "$remainder" = "$version" ||
   test "$patch" = "$remainder" ||
   test -z "$major" ||
   test -z "$minor" ||
   test -z "$patch"
then
    usage
fi
case "$patch" in
    *.*) usage ;;
esac

hash_file() {
    if command -v shasum >/dev/null 2>&1
    then
        shasum -a 256 "$1" | awk '{ print $1 }'
    elif command -v sha256sum >/dev/null 2>&1
    then
        sha256sum "$1" | awk '{ print $1 }'
    else
        echo "neither shasum nor sha256sum is available" >&2
        exit 1
    fi
}

checksum_for() {
    archive=$1
    checksum=$2
    expected=$(awk -v archive="$archive" '
        {
            count++
            hash = substr($0, 1, 64)
            separator = substr($0, 65, 1)
            marker = substr($0, 66, 1)
            filename = substr($0, 67)
            if (length($0) != 66 + length(archive) ||
                length(hash) != 64 || hash !~ /^[0-9a-f]+$/ ||
                separator != " " || (marker != " " && marker != "*") ||
                filename != archive) {
                exit 1
            }
        }
        END {
            if (count != 1) {
                exit 1
            }
            print hash
        }
    ' "$checksum") || {
        echo "expected exactly one valid checksum for $archive" >&2
        exit 1
    }
    test -n "$expected" || {
        echo "expected exactly one valid checksum for $archive" >&2
        exit 1
    }
    printf '%s\n' "$expected"
}

archive_count=$(find "$artifact_dir" -type f -name 'car-go-clean-*.tar.xz' |
    awk 'END { print NR + 0 }')
checksum_count=$(find "$artifact_dir" -type f -name 'car-go-clean-*.tar.xz.sha256' |
    awk 'END { print NR + 0 }')
if test "$archive_count" -ne 4 || test "$checksum_count" -ne 4
then
    echo "expected exactly four release archives and four checksum files" >&2
    exit 1
fi

archives='
car-go-clean-aarch64-apple-darwin.tar.xz
car-go-clean-x86_64-apple-darwin.tar.xz
car-go-clean-aarch64-unknown-linux-musl.tar.xz
car-go-clean-x86_64-unknown-linux-musl.tar.xz
'
for archive in $archives
do
    archive_paths=$(find "$artifact_dir" -type f -name "$archive")
    checksum_paths=$(find "$artifact_dir" -type f -name "$archive.sha256")
    archive_matches=$(printf '%s\n' "$archive_paths" | awk 'NF { count++ } END { print count + 0 }')
    checksum_matches=$(printf '%s\n' "$checksum_paths" | awk 'NF { count++ } END { print count + 0 }')
    if test "$archive_matches" -ne 1 || test "$checksum_matches" -ne 1
    then
        echo "expected one archive and checksum named $archive" >&2
        exit 1
    fi

    expected_hash=$(checksum_for "$archive" "$checksum_paths")
    actual_hash=$(hash_file "$archive_paths")
    if test "$actual_hash" != "$expected_hash"
    then
        echo "checksum verification failed for $archive" >&2
        exit 1
    fi
    printf '%s\t%s\t%s\n' "$archive" "$expected_hash" "$archive_paths"
done
