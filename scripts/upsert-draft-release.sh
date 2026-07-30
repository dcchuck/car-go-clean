#!/bin/sh
set -eu

if test "$#" -ne 5
then
    echo "usage: upsert-draft-release.sh TAG SHA TITLE NOTES ARTIFACT_DIR" >&2
    exit 64
fi

tag=$1
requested_sha=$2
title=$3
notes_file=$4
artifact_dir=$5
repository=${GITHUB_REPOSITORY:?GITHUB_REPOSITORY is required}

printf '%s\n' "$tag" | grep -Eq '^v[0-9]+\.[0-9]+\.[0-9]+$' || {
    echo "release tag must be exactly vX.Y.Z" >&2
    exit 64
}
printf '%s\n' "$requested_sha" | grep -Eq '^[0-9a-f]{40}$' || {
    echo "release commit must be a full lowercase 40-hex SHA" >&2
    exit 64
}
test -n "$title" || {
    echo "release title must not be empty" >&2
    exit 64
}
test -f "$notes_file" || {
    echo "release notes file does not exist: $notes_file" >&2
    exit 66
}
test -d "$artifact_dir" || {
    echo "release artifact directory does not exist: $artifact_dir" >&2
    exit 66
}

script_dir=$(CDPATH='' cd -- "$(dirname -- "$0")" && pwd)
"$script_dir/verify-release-assets.sh" "$tag" "$artifact_dir" >/dev/null

expected_assets='
car-go-clean-aarch64-apple-darwin.tar.xz
car-go-clean-aarch64-apple-darwin.tar.xz.sha256
car-go-clean-aarch64-unknown-linux-musl.tar.xz
car-go-clean-aarch64-unknown-linux-musl.tar.xz.sha256
car-go-clean-x86_64-apple-darwin.tar.xz
car-go-clean-x86_64-apple-darwin.tar.xz.sha256
car-go-clean-x86_64-unknown-linux-musl.tar.xz
car-go-clean-x86_64-unknown-linux-musl.tar.xz.sha256
'

work_dir=$(mktemp -d)
trap 'rm -rf "$work_dir"' EXIT HUP INT TERM
view_error="$work_dir/release-view-error"

resolved_repository=$(gh api "repos/$repository" --jq '.full_name')
test "$resolved_repository" = "$repository" || {
    echo "GitHub repository identity mismatch: $resolved_repository" >&2
    exit 1
}

tag_ref=$(
    gh api "repos/$repository/git/ref/tags/$tag" \
        --jq '[.object.type, .object.sha] | @tsv'
)
tag_ref_type=$(printf '%s\n' "$tag_ref" | awk -F '	' '{ print $1 }')
tag_object_sha=$(printf '%s\n' "$tag_ref" | awk -F '	' '{ print $2 }')
test "$tag_ref_type" = tag || {
    echo "release tag $tag is not annotated" >&2
    exit 1
}
printf '%s\n' "$tag_object_sha" | grep -Eq '^[0-9a-f]{40}$' || {
    echo "annotated tag object did not resolve to a full SHA" >&2
    exit 1
}

tag_object=$(
    gh api "repos/$repository/git/tags/$tag_object_sha" \
        --jq '[.object.type, .object.sha] | @tsv'
)
tag_target_type=$(printf '%s\n' "$tag_object" | awk -F '	' '{ print $1 }')
tag_target_sha=$(printf '%s\n' "$tag_object" | awk -F '	' '{ print $2 }')
test "$tag_target_type" = commit || {
    echo "annotated tag $tag does not point directly to a commit" >&2
    exit 1
}
test "$tag_target_sha" = "$requested_sha" || {
    echo "annotated tag $tag targets $tag_target_sha, not $requested_sha" >&2
    exit 1
}

view_release() {
    gh api "repos/$repository/releases/tags/$tag" \
        --jq '{
            tagName: .tag_name,
            isDraft: .draft,
            targetCommitish: .target_commitish,
            assets: .assets
        }'
}

validate_existing_release() {
    release_json=$1
    existing_tag=$(printf '%s\n' "$release_json" | jq -er '.tagName')
    existing_draft=$(printf '%s\n' "$release_json" | jq -er '.isDraft')
    existing_target=$(printf '%s\n' "$release_json" | jq -er '.targetCommitish')

    test "$existing_tag" = "$tag" || {
        echo "existing release tag does not match $tag" >&2
        exit 1
    }
    test "$existing_draft" = true || {
        echo "refusing to mutate published release $tag" >&2
        exit 1
    }

    resolved_target=$(
        gh api "repos/$repository/commits/$existing_target" --jq '.sha'
    )
    printf '%s\n' "$resolved_target" | grep -Eq '^[0-9a-f]{40}$' || {
        echo "existing release target did not resolve to a full commit SHA" >&2
        exit 1
    }
    test "$resolved_target" = "$requested_sha" || {
        echo "existing release $tag targets $resolved_target, not $requested_sha" >&2
        exit 1
    }
}

if release_json=$(view_release 2>"$view_error")
then
    validate_existing_release "$release_json"
    gh release edit "$tag" \
        --repo "$repository" \
        --draft \
        --target "$requested_sha" \
        --title "$title" \
        --notes-file "$notes_file"
else
    view_status=$?
    if ! grep -Fq '(HTTP 404)' "$view_error"
    then
        cat "$view_error" >&2
        exit "$view_status"
    fi
    gh release create "$tag" \
        --repo "$repository" \
        --draft \
        --target "$requested_sha" \
        --title "$title" \
        --notes-file "$notes_file"
fi

# Re-read and resolve the release after either mutation. This closes the
# create/edit boundary before any asset is replaced and makes a failed partial
# upload safe to retry.
release_json=$(view_release)
validate_existing_release "$release_json"

for asset in $expected_assets
do
    existing_count=$(
        printf '%s\n' "$release_json" |
            jq --arg name "$asset" '[.assets[] | select(.name == $name)] | length'
    )
    case "$existing_count" in
        0) ;;
        1)
            gh release delete-asset "$tag" "$asset" \
                --repo "$repository" \
                --yes
            ;;
        *)
            echo "release contains duplicate expected asset name: $asset" >&2
            exit 1
            ;;
    esac
done

for asset in $expected_assets
do
    gh release upload "$tag" "$artifact_dir/$asset" --repo "$repository"
done
