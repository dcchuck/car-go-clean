#!/bin/sh
set -eu

version=latest
version_requested=false
install_dir="$HOME/.local/bin"
download_base_url=
while [ "$#" -gt 0 ]; do
    case "$1" in
        --version) version=${2:?missing version}; version_requested=true; shift 2 ;;
        --install-dir) install_dir=${2:?missing install directory}; shift 2 ;;
        --download-base-url) download_base_url=${2:?missing download base URL}; shift 2 ;;
        *) echo "usage: $0 [--version X.Y.Z] [--install-dir PATH] [--download-base-url URL]" >&2; exit 2 ;;
    esac
done

if [ "$version_requested" = true ]; then
    case "$version" in
        ''|*[!0123456789.]*)
            echo "--version must be X.Y.Z" >&2
            exit 2
            ;;
    esac
    major=${version%%.*}
    remainder=${version#*.}
    minor=${remainder%%.*}
    patch=${remainder#*.}
    if [ "$remainder" = "$version" ] ||
       [ "$patch" = "$remainder" ] ||
       [ -z "$major" ] ||
       [ -z "$minor" ] ||
       [ -z "$patch" ]; then
        echo "--version must be X.Y.Z" >&2
        exit 2
    fi
    case "$patch" in
        *.*)
            echo "--version must be X.Y.Z" >&2
            exit 2
            ;;
    esac
fi

if [ -n "$download_base_url" ]; then
    if [ "$version_requested" != true ]; then
        echo "--download-base-url requires an explicit --version" >&2
        exit 2
    fi
    case "$download_base_url" in
        https://?*) ;;
        *)
            # CAR_GO_CLEAN_ALLOW_INSECURE_TEST_URL is a test-only escape hatch
            # for loopback HTTP and absolute file-backed rehearsal servers.
            if [ "${CAR_GO_CLEAN_ALLOW_INSECURE_TEST_URL-}" != 1 ]; then
                echo "--download-base-url must use https" >&2
                exit 2
            fi
            case "$download_base_url" in
                http://127.0.0.1|http://127.0.0.1:*|http://127.0.0.1/*|\
                http://localhost|http://localhost:*|http://localhost/*|\
                "http://[::1]"|"http://[::1]":*|"http://[::1]"/*|\
                file:///*) ;;
                *)
                    echo "insecure test download URL must use loopback HTTP or an absolute file URL" >&2
                    exit 2
                    ;;
            esac
            ;;
    esac
    download_base_url=${download_base_url%/}
fi

case "$(uname -s):$(uname -m)" in
    Darwin:arm64) target=aarch64-apple-darwin ;;
    Darwin:x86_64) target=x86_64-apple-darwin ;;
    Linux:aarch64|Linux:arm64) target=aarch64-unknown-linux-musl ;;
    Linux:x86_64|Linux:amd64) target=x86_64-unknown-linux-musl ;;
    *) echo "unsupported platform: $(uname -s) $(uname -m)" >&2; exit 1 ;;
esac

case "$version" in
    latest)
        tag=$(curl --proto '=https' --tlsv1.2 -fsSIL -o /dev/null -w '%{url_effective}' \
            https://github.com/dcchuck/car-go-clean/releases/latest | sed -n 's#.*/tag/\(v[^/]*\)$#\1#p')
        [ -n "$tag" ] || { echo "could not resolve the latest release tag" >&2; exit 1; }
        ;;
    *) tag="v$version" ;;
esac

archive_name="car-go-clean-$target.tar.xz"
checksum_name="$archive_name.sha256"
if [ -n "$download_base_url" ]; then
    base_url=$download_base_url
else
    base_url="https://github.com/dcchuck/car-go-clean/releases/download/$tag"
fi
work_dir=$(mktemp -d)
trap 'rm -rf "$work_dir"' EXIT HUP INT TERM

download_release_file() {
    destination=$1
    url=$2
    case "$url" in
        https://*) curl --proto '=https' --tlsv1.2 -fsSL -o "$destination" "$url" ;;
        http://*) curl --proto '=http' -fsSL -o "$destination" "$url" ;;
        file://*) curl --proto '=file' -fsSL -o "$destination" "$url" ;;
        *) echo "unsupported download URL: $url" >&2; exit 1 ;;
    esac
}

download_release_file "$work_dir/$archive_name" "$base_url/$archive_name"
download_release_file "$work_dir/$checksum_name" "$base_url/$checksum_name"

expected_hash=$(awk -v file="$archive_name" '
    NF {
        count++
        if (count != 1 || NF != 2 || $2 != file) {
            exit 1
        }
        print $1
    }
    END {
        if (count != 1) {
            exit 1
        }
    }
' "$work_dir/$checksum_name") || {
    echo "expected exactly one checksum for $archive_name" >&2
    exit 1
}
[ -n "$expected_hash" ] || {
    echo "expected exactly one checksum for $archive_name" >&2
    exit 1
}

case "$(uname -s)" in
    Darwin) actual_hash=$(shasum -a 256 "$work_dir/$archive_name" | awk '{ print $1 }') ;;
    Linux) actual_hash=$(sha256sum "$work_dir/$archive_name" | awk '{ print $1 }') ;;
    *) echo "unsupported platform" >&2; exit 1 ;;
esac

[ "$actual_hash" = "$expected_hash" ] || {
    echo "checksum verification failed for $archive_name" >&2
    exit 1
}

extract_dir="$work_dir/extracted"
mkdir "$extract_dir"
tar -xJf "$work_dir/$archive_name" -C "$extract_dir"
binary=$(find "$extract_dir" -type f -name car-go-clean -exec sh -c '
    for candidate do
        if [ -x "$candidate" ]; then
            printf "%s\\n" "$candidate"
        fi
    done
' sh {} +)
binary_count=$(printf '%s\n' "$binary" | awk 'NF { count++ } END { print count + 0 }')
[ "$binary_count" -eq 1 ] || {
    echo "archive must contain exactly one executable car-go-clean binary" >&2
    exit 1
}

mkdir -p "$install_dir"
install -m 755 "$binary" "$install_dir/.car-go-clean.$$"
mv -f "$install_dir/.car-go-clean.$$" "$install_dir/car-go-clean"

printf 'Installed car-go-clean to %s\n' "$install_dir/car-go-clean"
printf '%s\n' 'Restart an explicitly installed daemon with: car-go-clean service restart'
