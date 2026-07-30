#!/bin/sh
set -eu

usage() {
    echo "usage: $0 /absolute/path/to/inventory.tsv [/absolute/path/to/source-map.tsv]" >&2
}

die() {
    echo "tart inventory: $*" >&2
    exit 1
}

test "$#" -ge 1 && test "$#" -le 2 || {
    usage
    exit 2
}

inventory=$1
source_map=${2-}
case "$inventory" in
    /*) ;;
    *) die "inventory path must be absolute" ;;
esac
if test -n "$source_map"; then
    case "$source_map" in
        /*) ;;
        *) die "source-map path must be absolute" ;;
    esac
    test -f "$source_map" || die "source map does not exist: $source_map"
fi

command -v tart >/dev/null 2>&1 || die "tart is not available"
command -v python3 >/dev/null 2>&1 || die "python3 is required to parse Tart JSON"

inventory_parent=$(dirname "$inventory")
test -d "$inventory_parent" || die "inventory parent does not exist: $inventory_parent"
work_dir=$(mktemp -d "$inventory_parent/.tart-inventory.XXXXXX")
trap 'rm -rf "$work_dir"' EXIT HUP INT TERM

tart list --source local --format json > "$work_dir/list.json"

python3 - "$work_dir/list.json" "$source_map" > "$work_dir/rows.tsv" <<'PY'
import json
import pathlib
import re
import sys

list_path = pathlib.Path(sys.argv[1])
source_map_path = sys.argv[2]
digest_re = re.compile(r"^[0-9a-f]{64}$")
reference_re = re.compile(r"^ghcr\.io/[^@\s]+@sha256:([0-9a-f]{64})$")

source_by_name = {}
if source_map_path:
    for line_number, line in enumerate(
        pathlib.Path(source_map_path).read_text().splitlines(), 1
    ):
        if not line or line.startswith("#"):
            continue
        fields = line.split("\t")
        if len(fields) != 3 or not all(fields):
            raise SystemExit(f"malformed source map line {line_number}")
        name, reference, digest = fields
        if name in source_by_name:
            raise SystemExit(f"duplicate source-map VM name: {name}")
        match = reference_re.fullmatch(reference)
        if not match or not digest_re.fullmatch(digest) or match.group(1) != digest:
            raise SystemExit(f"invalid immutable source on line {line_number}")
        source_by_name[name] = (reference, digest)

try:
    items = json.loads(list_path.read_text())
except (OSError, json.JSONDecodeError) as error:
    raise SystemExit(f"could not parse tart list JSON: {error}")
if not isinstance(items, list):
    raise SystemExit("tart list JSON must be an array")

seen = set()
for item in sorted(items, key=lambda value: value.get("Name", "")):
    if not isinstance(item, dict):
        raise SystemExit("tart list entry must be an object")
    name = item.get("Name")
    state = item.get("State")
    if (
        not isinstance(name, str)
        or not name
        or any(character in name for character in "\t\r\n")
        or name.startswith("-")
    ):
        raise SystemExit("tart list contains an unsafe VM name")
    if (
        not isinstance(state, str)
        or not state
        or any(character in state for character in "\t\r\n")
    ):
        raise SystemExit(f"tart list contains an unsafe state for {name}")
    if name in seen:
        raise SystemExit(f"tart list contains duplicate VM name: {name}")
    seen.add(name)
    reference, digest = source_by_name.get(
        name, ("UNKNOWN_SOURCE", "UNKNOWN_DIGEST")
    )
    print(name, state, reference, digest, sep="\t")
PY

# CAR_GO_CLEAN_TART_HOME exists only for isolated harness tests. Normal
# operation follows Tart's supported TART_HOME, then Tart's default.
tart_home=${CAR_GO_CLEAN_TART_HOME:-${TART_HOME:-"$HOME/.tart"}}
if test -e "$tart_home"; then
    tart_storage_bytes=$(du -sk "$tart_home" | awk '{ print $1 * 1024 }')
else
    tart_storage_bytes=0
fi
df_target=$tart_home
while test ! -e "$df_target"; do
    parent=$(dirname "$df_target")
    test "$parent" != "$df_target" || break
    df_target=$parent
done
host_df=$(df -Pk "$df_target" | awk 'NR == 2 {
    printf "%s,total_kib=%s,used_kib=%s,available_kib=%s,capacity=%s,mounted=%s",
        $1, $2, $3, $4, $5, $6
}')
test -n "$host_df" || die "could not read host free-space metrics"

{
    cat "$work_dir/rows.tsv"
    printf '# tart_storage_bytes\t%s\n' "$tart_storage_bytes"
    printf '# host_df\t%s\n' "$host_df"
} > "$work_dir/inventory.tsv"

chmod 600 "$work_dir/inventory.tsv"
mv -f "$work_dir/inventory.tsv" "$inventory"
printf 'Wrote Tart inventory to %s\n' "$inventory"
cat "$inventory"
