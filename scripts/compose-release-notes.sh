#!/bin/sh
set -eu

test "$#" -eq 3
tag=$1
generated=$2
output=$3

case "$tag" in
  v*) ;;
  *) echo "tag must be vX.Y.Z" >&2; exit 2 ;;
esac

version=${tag#v}
if ! printf '%s\n' "$version" |
  awk '/^[0-9]+\.[0-9]+\.[0-9]+$/ { valid=1 } END { exit !valid }'
then
  echo "tag must be vX.Y.Z" >&2
  exit 2
fi

repo_root=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
versioned="$repo_root/docs/releases/$tag.md"
test -r "$versioned"
test -r "$generated"

{
  cat "$versioned"
  printf '\n\n---\n\n'
  cat "$generated"
} > "$output"
