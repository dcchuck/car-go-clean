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
plan_manifest=${CARGO_DIST_PLAN_MANIFEST:?CARGO_DIST_PLAN_MANIFEST is required}
global_manifest=${CARGO_DIST_GLOBAL_MANIFEST:?CARGO_DIST_GLOBAL_MANIFEST is required}

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
test -f "$notes_file" && test ! -L "$notes_file" || {
    echo "release notes must be a regular, non-symlink file: $notes_file" >&2
    exit 66
}
test -d "$artifact_dir" || {
    echo "release artifact directory does not exist: $artifact_dir" >&2
    exit 66
}
for manifest in "$plan_manifest" "$global_manifest"
do
    test -f "$manifest" && test ! -L "$manifest" || {
        echo "cargo-dist manifest must be a regular, non-symlink file: $manifest" >&2
        exit 66
    }
done
case "$repository" in
    */*) ;;
    *) echo "GITHUB_REPOSITORY must be owner/name" >&2; exit 64 ;;
esac
test "${repository#*/}" != "$repository" &&
    test "${repository#*/}" = "${repository##*/}" || {
        echo "GITHUB_REPOSITORY must be exactly owner/name" >&2
        exit 64
    }
owner=${repository%%/*}
repo_name=${repository#*/}

expected_assets='
car-go-clean-aarch64-apple-darwin.tar.xz
car-go-clean-aarch64-apple-darwin.tar.xz.sha256
car-go-clean-aarch64-unknown-linux-musl.tar.xz
car-go-clean-aarch64-unknown-linux-musl.tar.xz.sha256
car-go-clean-x86_64-apple-darwin.tar.xz
car-go-clean-x86_64-apple-darwin.tar.xz.sha256
car-go-clean-x86_64-unknown-linux-musl.tar.xz
car-go-clean-x86_64-unknown-linux-musl.tar.xz.sha256
car-go-clean.rb
sha256.sum
source.tar.gz
source.tar.gz.sha256
'
global_assets='
car-go-clean.rb
sha256.sum
source.tar.gz
source.tar.gz.sha256
'

work_dir=$(mktemp -d)
trap 'rm -rf "$work_dir"' EXIT HUP INT TERM
expected_file="$work_dir/expected-assets"
global_expected_file="$work_dir/global-assets"
actual_file="$work_dir/actual-assets"
printf '%s\n' "$expected_assets" | sed '/^$/d' | LC_ALL=C sort > "$expected_file"
printf '%s\n' "$global_assets" | sed '/^$/d' | LC_ALL=C sort > "$global_expected_file"

# Preflight the complete upload inventory before the first GitHub call.
: > "$actual_file"
for entry in "$artifact_dir"/* "$artifact_dir"/.[!.]* "$artifact_dir"/..?*
do
    test -e "$entry" || test -L "$entry" || continue
    test -f "$entry" && test ! -L "$entry" || {
        echo "release artifact must be a regular, non-symlink file: $entry" >&2
        exit 66
    }
    basename "$entry" >> "$actual_file"
done
LC_ALL=C sort -o "$actual_file" "$actual_file"
cmp -s "$expected_file" "$actual_file" || {
    echo "release artifact inventory does not match the reviewed cargo-dist plan" >&2
    diff -u "$expected_file" "$actual_file" >&2 || true
    exit 1
}

manifest_names() {
    jq -er '.artifacts | keys[]' "$1" | LC_ALL=C sort
}
test "$(
    jq -r '.dist_version // empty' "$plan_manifest"
)" = 0.32.0 &&
test "$(
    jq -r '.announcement_tag // empty' "$plan_manifest"
)" = "$tag" || {
    echo "cargo-dist plan version or tag does not match this release" >&2
    exit 1
}
manifest_names "$plan_manifest" > "$work_dir/plan-assets"
cmp -s "$expected_file" "$work_dir/plan-assets" || {
    echo "cargo-dist plan artifact inventory is not the reviewed 12-file set" >&2
    exit 1
}
test "$(
    jq -r '.dist_version // empty' "$global_manifest"
)" = 0.32.0 &&
test "$(
    jq -r '.announcement_tag // empty' "$global_manifest"
)" = "$tag" || {
    echo "cargo-dist global manifest version or tag does not match this release" >&2
    exit 1
}
manifest_names "$global_manifest" > "$work_dir/global-manifest-assets"
jq -er '.upload_files[]' "$global_manifest" |
    while IFS= read -r upload
    do
        basename "$upload"
    done |
    LC_ALL=C sort > "$work_dir/global-upload-assets"
if ! cmp -s "$global_expected_file" "$work_dir/global-manifest-assets" ||
    ! cmp -s "$global_expected_file" "$work_dir/global-upload-assets"
then
    echo "cargo-dist global manifest does not name the reviewed global artifacts" >&2
    exit 1
fi

target_assets="$work_dir/target-assets"
mkdir "$target_assets"
for asset in $expected_assets
do
    case "$asset" in
        *.tar.xz|*.tar.xz.sha256)
            cp "$artifact_dir/$asset" "$target_assets/$asset"
            ;;
    esac
done
script_dir=$(CDPATH='' cd -- "$(dirname -- "$0")" && pwd)
"$script_dir/verify-release-assets.sh" "$tag" "$target_assets" >/dev/null
source_expected=$(
    awk 'NF { print $1; exit }' "$artifact_dir/source.tar.gz.sha256"
)
if command -v shasum >/dev/null 2>&1
then
    source_actual=$(shasum -a 256 "$artifact_dir/source.tar.gz" | awk '{ print $1 }')
else
    source_actual=$(sha256sum "$artifact_dir/source.tar.gz" | awk '{ print $1 }')
fi
test "$source_expected" = "$source_actual" || {
    echo "source archive checksum does not match source.tar.gz.sha256" >&2
    exit 1
}

resolved_repository=$(gh api "repos/$repository" --jq '.full_name')
test "$resolved_repository" = "$repository" || {
    echo "GitHub repository identity mismatch: $resolved_repository" >&2
    exit 1
}

verify_tag() {
    preserved_object=${1:-}
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
    if test -n "$preserved_object" && test "$tag_object_sha" != "$preserved_object"
    then
        echo "annotated tag object changed during draft upsert" >&2
        exit 1
    fi
    tag_object=$(
        gh api "repos/$repository/git/tags/$tag_object_sha" \
            --jq '[.object.type, .object.sha] | @tsv'
    )
    tag_target_type=$(printf '%s\n' "$tag_object" | awk -F '	' '{ print $1 }')
    tag_target_sha=$(printf '%s\n' "$tag_object" | awk -F '	' '{ print $2 }')
    test "$tag_target_type" = commit && test "$tag_target_sha" = "$requested_sha" || {
        echo "annotated tag $tag does not point directly to $requested_sha" >&2
        exit 1
    }
    printf '%s\n' "$tag_object_sha"
}

discover_release_id() {
    # The dollar-prefixed names below are GraphQL variables, not shell values.
    # shellcheck disable=SC2016
    query='
      query PendingRelease($owner: String!, $name: String!, $tagName: String!) {
        repository(owner: $owner, name: $name) {
          release(tagName: $tagName) {
            databaseId
            isDraft
          }
        }
      }'
    discovery=$(
        gh api graphql \
            -f query="$query" \
            -F owner="$owner" \
            -F name="$repo_name" \
            -F tagName="$tag"
    )
    printf '%s\n' "$discovery" |
        jq -e '
            .errors == null and
            (.data.repository | type == "object") and
            (.data.repository | has("release"))
        ' \
            >/dev/null || {
        echo "GitHub draft discovery returned an invalid or failed query response" >&2
        exit 1
    }
    release_type=$(printf '%s\n' "$discovery" | jq -r '.data.repository.release | type')
    case "$release_type" in
        null) printf '%s\n' absent ;;
        object)
            printf '%s\n' "$discovery" |
                jq -er '
                    .data.repository.release |
                    select(
                        (.databaseId | type) == "number" and
                        .databaseId > 0 and
                        (.isDraft | type) == "boolean"
                    ) |
                    .databaseId
                '
            ;;
        *)
            echo "GitHub draft discovery returned an invalid release value" >&2
            exit 1
            ;;
    esac
}

fetch_release_by_id() {
    gh api "repos/$repository/releases/$1" \
        --jq '{
            databaseId: .id,
            tagName: .tag_name,
            isDraft: .draft,
            targetCommitish: .target_commitish,
            name: .name,
            body: .body,
            assets: [.assets[] | {id: .id, name: .name}]
        }'
}

validate_release() {
    release_json=$1
    expected_id=$2
    printf '%s\n' "$release_json" |
        jq -e --argjson id "$expected_id" --arg tag "$tag" '
            .databaseId == $id and
            .tagName == $tag and
            .isDraft == true and
            (.targetCommitish | type) == "string" and
            (.assets | type) == "array" and
            all(.assets[];
                (.id | type) == "number" and
                (.name | type) == "string"
            )
        ' >/dev/null || {
        echo "release $expected_id is not the expected commit-bound draft" >&2
        exit 1
    }
    existing_target=$(printf '%s\n' "$release_json" | jq -er '.targetCommitish')
    resolved_target=$(
        gh api "repos/$repository/commits/$existing_target" --jq '.sha'
    )
    test "$resolved_target" = "$requested_sha" || {
        echo "existing release $tag targets $resolved_target, not $requested_sha" >&2
        exit 1
    }
}

proved_tag_object_sha=$(verify_tag)
release_id=$(discover_release_id)
if test "$release_id" = absent
then
    gh release create "$tag" \
        --repo "$repository" \
        --draft \
        --verify-tag \
        --target "$requested_sha" \
        --title "$title" \
        --notes-file "$notes_file"
    release_id=$(discover_release_id)
    test "$release_id" != absent || {
        echo "draft release creation did not produce a discoverable release" >&2
        exit 1
    }
else
    release_json=$(fetch_release_by_id "$release_id")
    validate_release "$release_json" "$release_id"
    jq -n \
        --arg tag "$tag" \
        --arg target "$requested_sha" \
        --arg title "$title" \
        --rawfile body "$notes_file" \
        '{
            tag_name: $tag,
            target_commitish: $target,
            name: $title,
            body: $body,
            draft: true
        }' > "$work_dir/release-patch.json"
    gh api --method PATCH "repos/$repository/releases/$release_id" \
        --input "$work_dir/release-patch.json" >/dev/null
fi

rediscovered_id=$(discover_release_id)
test "$rediscovered_id" = "$release_id" || {
    echo "release identity changed after create/update" >&2
    exit 1
}
release_json=$(fetch_release_by_id "$release_id")
validate_release "$release_json" "$release_id"
verify_tag "$proved_tag_object_sha" >/dev/null

# This is the final identity boundary before asset mutation. GitHub has no
# transaction spanning tags, releases, and uploads, so a residual TOCTOU window
# remains after this check; ID-addressed deletion prevents deleting a replacement
# asset if that race is lost.
rediscovered_id=$(discover_release_id)
test "$rediscovered_id" = "$release_id" || {
    echo "release identity changed before asset mutation" >&2
    exit 1
}
release_json=$(fetch_release_by_id "$release_id")
validate_release "$release_json" "$release_id"
verify_tag "$proved_tag_object_sha" >/dev/null

for asset in $expected_assets
do
    existing_count=$(
        printf '%s\n' "$release_json" |
            jq --arg name "$asset" '[.assets[] | select(.name == $name)] | length'
    )
    case "$existing_count" in
        0) ;;
        1)
            asset_id=$(
                printf '%s\n' "$release_json" |
                    jq -er --arg name "$asset" \
                        '.assets[] | select(.name == $name) | .id'
            )
            gh api --method DELETE \
                "repos/$repository/releases/assets/$asset_id"
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
