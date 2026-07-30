#!/bin/sh
set -eu

repo_root=$(CDPATH='' cd -- "$(dirname -- "$0")/.." && pwd)

manifest_msrv=$(awk -F '"' '
    /^[[:space:]]*rust-version[[:space:]]*=/ {
        print $2
        found++
    }
    END {
        if (found != 1) {
            exit 1
        }
    }
' "$repo_root/Cargo.toml")

toolchain_version=$(awk -F '"' '
    /^[[:space:]]*channel[[:space:]]*=/ {
        print $2
        found++
    }
    END {
        if (found != 1) {
            exit 1
        }
    }
' "$repo_root/rust-toolchain.toml")

case "$manifest_msrv" in
    *.*.*) normalized_msrv=$manifest_msrv ;;
    *.*) normalized_msrv=$manifest_msrv.0 ;;
    *)
        echo "Cargo.toml rust-version is not a stable Rust version: $manifest_msrv" >&2
        exit 1
        ;;
esac

if test "$normalized_msrv" != "$toolchain_version"
then
    echo "Cargo.toml rust-version ($manifest_msrv) does not match rust-toolchain.toml ($toolchain_version)" >&2
    exit 1
fi
