#!/bin/sh
set -eu

usage() {
    echo "usage: $0 /absolute/path/to/inventory.tsv" >&2
}

die() {
    echo "tart cleanup: $*" >&2
    exit 1
}

test "$#" -eq 1 || {
    usage
    exit 2
}

inventory=$1
case "$inventory" in
    /*) ;;
    *) die "inventory path must be absolute" ;;
esac
test -f "$inventory" || die "inventory does not exist: $inventory"
command -v tart >/dev/null 2>&1 || die "tart is not available"
command -v python3 >/dev/null 2>&1 || die "python3 is required to parse Tart JSON"

work_dir=$(mktemp -d)
trap 'rm -rf "$work_dir"' EXIT HUP INT TERM
rows=$work_dir/rows.tsv

awk '
    /^#/ || NF == 0 { next }
    {
        if (NF != 4) {
            print "malformed inventory row: " $0 > "/dev/stderr"
            exit 1
        }
        if ($1 ~ /^-/ || $1 ~ /[\r\n]/ || seen[$1]++) {
            print "unsafe or duplicate inventory VM name: " $1 > "/dev/stderr"
            exit 1
        }
        print $0
    }
' FS='\t' OFS='\t' "$inventory" > "$rows" ||
    die "inventory validation failed"

echo "Concrete Tart inventory selected for irreversible deletion:"
cat "$inventory"
unknown_count=$(awk -F '\t' '$3 == "UNKNOWN_SOURCE" { count++ } END { print count + 0 }' "$rows")
if test "$unknown_count" -gt 0; then
    echo "WARNING: $unknown_count VM(s) have unknown source and cannot be reconstructed from this inventory." >&2
fi

test "${CAR_GO_CLEAN_TART_DELETE_ALL-}" = YES ||
    die "refusing deletion; set CAR_GO_CLEAN_TART_DELETE_ALL=YES after reviewing the concrete inventory above"

# CAR_GO_CLEAN_TART_HOME exists only for isolated harness tests. Normal
# operation follows Tart's supported TART_HOME, then Tart's default.
tart_home=${CAR_GO_CLEAN_TART_HOME:-${TART_HOME:-"$HOME/.tart"}}
df_target=$tart_home
while test ! -e "$df_target"; do
    parent=$(dirname "$df_target")
    test "$parent" != "$df_target" || break
    df_target=$parent
done
if test -e "$tart_home"; then
    before_bytes=$(du -sk "$tart_home" | awk '{ print $1 * 1024 }')
else
    before_bytes=0
fi
before_available_kib=$(df -Pk "$df_target" | awk 'NR == 2 { print $4 }')

while IFS='	' read -r name state source_reference source_digest; do
    test -n "$name" || continue
    printf 'Stopping exact VM %s (inventory state: %s, source: %s, digest: %s)\n' \
        "$name" "$state" "$source_reference" "$source_digest"
    tart stop "$name" >/dev/null 2>&1 || :
    tart delete "$name"
done < "$rows"

# Prune only caches after deleting the exact inventoried VM names. Never use
# `--entries vms`: that would broaden deletion beyond the concrete inventory.
tart prune --entries caches --space-budget 0

tart list --format json > "$work_dir/final.json"
python3 - "$work_dir/final.json" <<'PY'
import json
import pathlib
import sys

try:
    items = json.loads(pathlib.Path(sys.argv[1]).read_text())
except (OSError, json.JSONDecodeError) as error:
    raise SystemExit(f"could not parse final Tart inventory: {error}")
if not isinstance(items, list):
    raise SystemExit("final tart list was not an array")
if items:
    for item in items:
        name = item.get("Name", "<unknown>") if isinstance(item, dict) else repr(item)
        state = item.get("State", "<unknown>") if isinstance(item, dict) else "<unknown>"
        print(f"remaining Tart entry: {name}\t{state}", file=sys.stderr)
    raise SystemExit("final tart list is not empty")
PY

if test -e "$tart_home"; then
    after_bytes=$(du -sk "$tart_home" | awk '{ print $1 * 1024 }')
else
    after_bytes=0
fi
after_available_kib=$(df -Pk "$df_target" | awk 'NR == 2 { print $4 }')
case "$before_bytes:$after_bytes" in
    *[!0-9:]*) reclaimed_bytes=unknown ;;
    *)
        if test "$before_bytes" -ge "$after_bytes"; then
            reclaimed_bytes=$((before_bytes - after_bytes))
        else
            reclaimed_bytes=0
        fi
        ;;
esac

printf 'Tart storage bytes before: %s\n' "$before_bytes"
printf 'Tart storage bytes after: %s\n' "$after_bytes"
printf 'Estimated Tart bytes reclaimed: %s\n' "$reclaimed_bytes"
printf 'Host available KiB before: %s\n' "$before_available_kib"
printf 'Host available KiB after: %s\n' "$after_available_kib"
echo "Verified: tart list is empty."
