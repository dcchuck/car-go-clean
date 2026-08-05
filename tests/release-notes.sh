#!/bin/sh
set -eu

repo_root=$(CDPATH='' cd -- "$(dirname -- "$0")/.." && pwd)
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT HUP INT TERM

printf 'generated install body\n' > "$work/generated.md"
"$repo_root/scripts/compose-release-notes.sh" \
  v0.4.1 "$work/generated.md" "$work/output.md"

{
  cat "$repo_root/docs/releases/v0.4.1.md"
  printf '\n\n---\n\ngenerated install body\n'
} > "$work/expected.md"
cmp "$work/expected.md" "$work/output.md"

if "$repo_root/scripts/compose-release-notes.sh" \
  v9.9.9 "$work/generated.md" "$work/missing-version.md"
then
  echo "missing versioned metadata was accepted" >&2
  exit 1
fi

for invalid_tag in v0.4 v1..2 v1.2.3x ' v1.2.3' 'v1.2.3
junk'
do
  if "$repo_root/scripts/compose-release-notes.sh" \
    "$invalid_tag" "$work/generated.md" "$work/invalid.md"
  then
    status=0
  else
    status=$?
  fi
  if test "$status" -ne 2
  then
    echo "invalid tag was not rejected by validation: $invalid_tag" >&2
    exit 1
  fi
done
