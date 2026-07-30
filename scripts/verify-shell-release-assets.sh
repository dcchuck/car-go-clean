#!/bin/sh
set -eu

if test "$#" -ne 1
then
    echo "usage: $0 ASSET_DIR" >&2
    exit 64
fi

asset_dir=$1
test -d "$asset_dir" || {
    echo "shell asset directory does not exist: $asset_dir" >&2
    exit 66
}

manifest=$asset_dir/car-go-clean-shell-assets.sha256
for asset in \
    "$asset_dir/car-go-clean-installer.sh" \
    "$asset_dir/car-go-clean-upgrade.sh" \
    "$manifest"
do
    test -f "$asset" && test ! -L "$asset" || {
        echo "shell release asset must be a regular, non-symlink file: $asset" >&2
        exit 66
    }
done

work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT HUP INT TERM
parsed=$work/parsed
if ! awk '
    {
        count++
        hash = substr($0, 1, 64)
        separator = substr($0, 65, 1)
        marker = substr($0, 66, 1)
        name = substr($0, 67)
        if (length(hash) != 64 ||
            hash !~ /^[0-9a-f]+$/ ||
            separator != " " ||
            (marker != " " && marker != "*") ||
            (name != "car-go-clean-installer.sh" &&
             name != "car-go-clean-upgrade.sh")) {
            invalid = 1
            exit 1
        }
        seen[name]++
        print name "\t" hash
    }
    END {
        if (invalid || count != 2 ||
            seen["car-go-clean-installer.sh"] != 1 ||
            seen["car-go-clean-upgrade.sh"] != 1) {
            exit 1
        }
    }
' "$manifest" > "$work/parsed-unsorted"
then
    echo "shell checksum manifest must contain exactly the two expected hashes" >&2
    exit 1
fi
LC_ALL=C sort "$work/parsed-unsorted" > "$parsed"

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

while IFS='	' read -r name expected_hash
do
    asset=$asset_dir/$name
    actual_hash=$(hash_file "$asset")
    test "$actual_hash" = "$expected_hash" || {
        echo "shell release asset checksum failed: $name" >&2
        exit 1
    }
    printf '%s\t%s\t%s\n' "$name" "$actual_hash" "$asset"
done < "$parsed"
