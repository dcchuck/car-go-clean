#!/bin/sh
set -eu

usage() {
    echo "usage: $0 COMMIT_SHA X.Y.Z" >&2
    exit 2
}

test "$#" -eq 2 || usage
release_sha=$1
version=$2

case "$release_sha" in
    *[!0-9a-f]*|'') usage ;;
esac
test "${#release_sha}" -eq 40 || usage

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

if test -n "$(git status --porcelain)"
then
    echo "release checkout is dirty" >&2
    exit 1
fi

resolved_sha=$(git rev-parse "$release_sha^{commit}") || {
    echo "release commit is not reachable: $release_sha" >&2
    exit 1
}
if test "$resolved_sha" != "$release_sha"
then
    echo "release commit did not resolve exactly: $release_sha" >&2
    exit 1
fi

if ! git merge-base --is-ancestor "$release_sha" origin/main
then
    echo "release commit is not contained by origin/main: $release_sha" >&2
    exit 1
fi

metadata=$(cargo metadata --no-deps --format-version 1)
manifest_version=$(printf '%s\n' "$metadata" |
    jq -er '.packages[] | select(.name == "car-go-clean") | .version')
if test "$manifest_version" != "$version"
then
    echo "Cargo.toml version $manifest_version does not match requested version $version" >&2
    exit 1
fi

tag=v$version
if git rev-parse -q --verify "refs/tags/$tag" >/dev/null
then
    echo "release tag already exists locally: $tag" >&2
    exit 1
fi

set +e
git ls-remote --exit-code --tags origin "refs/tags/$tag" >/dev/null 2>&1
remote_tag_status=$?
set -e
case "$remote_tag_status" in
    0)
        echo "release tag already exists on origin: $tag" >&2
        exit 1
        ;;
    2) ;;
    *)
        echo "could not verify remote release tag: $tag" >&2
        exit 1
        ;;
esac

printf 'RELEASE_SHA=%s\n' "$release_sha"
printf 'VERSION=%s\n' "$version"
printf 'TAG=%s\n' "$tag"
