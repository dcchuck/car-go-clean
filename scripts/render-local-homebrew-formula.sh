#!/bin/sh
set -eu

if test "$#" -ne 3 && test "$#" -ne 4
then
    echo "usage: $0 vX.Y.Z ARTIFACT_DIR OUTPUT [FILE_DOWNLOAD_BASE]" >&2
    exit 64
fi

tag=$1
artifact_dir=$2
output=$3
download_base=${4:-}
version=${tag#v}
if test "$tag" != "v$version" ||
    ! printf '%s\n' "$version" | grep -Eq '^[0-9]+\.[0-9]+\.[0-9]+$'
then
    echo "local formula tag must be exactly vX.Y.Z" >&2
    exit 64
fi
if test -n "$download_base"
then
    case "$download_base" in
        file:///*) download_base=${download_base%/} ;;
        *)
            echo "local formula download base must be an absolute file URL" >&2
            exit 64
            ;;
    esac
fi

script_dir=$(CDPATH='' cd -- "$(dirname -- "$0")" && pwd)
output_dir=$(dirname "$output")
test -d "$output_dir" || {
    echo "local formula output directory does not exist: $output_dir" >&2
    exit 66
}

work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT HUP INT TERM
rendered=$work/rendered.rb
"$script_dir/render-homebrew-formula.sh" "$tag" "$artifact_dir" "$rendered"

public_base="https://github.com/dcchuck/car-go-clean/releases/download/$tag"
awk \
    -v version="$version" \
    -v public_base="$public_base" \
    -v download_base="$download_base" '
    function replace_literal(value, needle, replacement, position) {
        if (replacement == "") {
            return value
        }
        while ((position = index(value, needle)) != 0) {
            value = substr(value, 1, position - 1) replacement \
                substr(value, position + length(needle))
        }
        return value
    }
    {
        line = replace_literal($0, public_base, download_base)
        print line
        if ($0 == "  license \"MIT\"") {
            printf "  version \"%s\"\n", version
            licenses++
        }
    }
    END {
        if (licenses != 1) {
            exit 1
        }
    }
' "$rendered" > "$output"

test "$(grep -Fxc "  version \"$version\"" "$output")" -eq 1 || {
    echo "local formula does not contain its exact explicit version" >&2
    exit 1
}
