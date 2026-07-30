#!/bin/sh
set -eu

if test "$#" -ne 4
then
    echo "usage: $0 publish-prerelease|promote-stable TAG SHA RELEASE_ID" >&2
    exit 64
fi

mode=$1
tag=$2
expected_sha=$3
expected_release_id=$4
repository=${GITHUB_REPOSITORY:?GITHUB_REPOSITORY is required}
test "$repository" = dcchuck/car-go-clean || {
    echo "release transitions are restricted to dcchuck/car-go-clean" >&2
    exit 64
}
case "$mode" in
    publish-prerelease|promote-stable) ;;
    *) echo "unknown release transition: $mode" >&2; exit 64 ;;
esac
printf '%s\n' "$tag" | grep -Eq '^v[0-9]+\.[0-9]+\.[0-9]+$' || {
    echo "release tag must be exactly vX.Y.Z" >&2
    exit 64
}
printf '%s\n' "$expected_sha" | grep -Eq '^[0-9a-f]{40}$' || {
    echo "release commit must be a full lowercase SHA" >&2
    exit 64
}
case "$expected_release_id" in
    ''|*[!0-9]*|0) echo "release ID must be a positive integer" >&2; exit 64 ;;
esac

owner=${repository%%/*}
repo_name=${repository#*/}
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT HUP INT TERM
cat > "$work/expected-assets" <<'EOF'
car-go-clean-aarch64-apple-darwin.tar.xz
car-go-clean-aarch64-apple-darwin.tar.xz.sha256
car-go-clean-aarch64-unknown-linux-musl.tar.xz
car-go-clean-aarch64-unknown-linux-musl.tar.xz.sha256
car-go-clean-installer.sh
car-go-clean-shell-assets.sha256
car-go-clean-upgrade.sh
car-go-clean-x86_64-apple-darwin.tar.xz
car-go-clean-x86_64-apple-darwin.tar.xz.sha256
car-go-clean-x86_64-unknown-linux-musl.tar.xz
car-go-clean-x86_64-unknown-linux-musl.tar.xz.sha256
car-go-clean.rb
sha256.sum
source.tar.gz
source.tar.gz.sha256
EOF

load_and_validate() {
    # Dollar-prefixed names in this query are GraphQL variables.
    # shellcheck disable=SC2016
    query='
      query TransitionRelease($owner: String!, $name: String!, $tagName: String!) {
        repository(owner: $owner, name: $name) {
          release(tagName: $tagName) {
            databaseId
            isDraft
            isPrerelease
            isLatest
            tagName
            tagCommit {
              oid
            }
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
          (.data.repository.release | type == "object") and
          (.data.repository.release.databaseId | type == "number") and
          (.data.repository.release.isDraft | type == "boolean") and
          (.data.repository.release.isPrerelease | type == "boolean") and
          (.data.repository.release.isLatest | type == "boolean") and
          (.data.repository.release.tagName | type == "string") and
          (.data.repository.release.tagCommit.oid |
            type == "string")
        ' >/dev/null || {
        echo "release discovery returned an invalid response" >&2
        exit 1
    }

    release_id=$(
        printf '%s\n' "$discovery" |
            jq -r '.data.repository.release.databaseId'
    )
    release_draft=$(
        printf '%s\n' "$discovery" |
            jq -r '.data.repository.release.isDraft'
    )
    release_prerelease=$(
        printf '%s\n' "$discovery" |
            jq -r '.data.repository.release.isPrerelease'
    )
    release_latest=$(
        printf '%s\n' "$discovery" |
            jq -r '.data.repository.release.isLatest'
    )
    release_tag=$(
        printf '%s\n' "$discovery" |
            jq -r '.data.repository.release.tagName'
    )
    tag_commit=$(
        printf '%s\n' "$discovery" |
            jq -r '.data.repository.release.tagCommit.oid'
    )
    test "$release_id" = "$expected_release_id" || {
        echo "release ID changed after authenticated draft verification" >&2
        exit 1
    }
    test "$release_tag" = "$tag" && test "$tag_commit" = "$expected_sha" || {
        echo "release tag identity does not match the approved commit" >&2
        exit 1
    }

    release_json=$(gh api "repos/$repository/releases/$expected_release_id")
    printf '%s\n' "$release_json" |
        jq -e '
          (.id | type == "number") and
          (.tag_name | type == "string") and
          (.target_commitish | type == "string") and
          (.draft | type == "boolean") and
          (.prerelease | type == "boolean") and
          (.assets | type == "array") and
          all(.assets[]; (.name | type == "string"))
        ' >/dev/null || {
        echo "numeric release lookup returned an invalid response" >&2
        exit 1
    }
    test "$(printf '%s\n' "$release_json" | jq -r '.id')" = \
        "$expected_release_id" || {
        echo "numeric release identity changed" >&2
        exit 1
    }
    test "$(printf '%s\n' "$release_json" | jq -r '.tag_name')" = "$tag" || {
        echo "numeric release tag changed" >&2
        exit 1
    }
    test "$(printf '%s\n' "$release_json" | jq -r '.draft')" = \
        "$release_draft" &&
        test "$(printf '%s\n' "$release_json" | jq -r '.prerelease')" = \
            "$release_prerelease" || {
        echo "GraphQL and numeric release states disagree" >&2
        exit 1
    }

    target=$(
        printf '%s\n' "$release_json" |
            jq -r '.target_commitish'
    )
    resolved_target=$(
        gh api "repos/$repository/commits/$target" --jq '.sha'
    )
    test "$resolved_target" = "$expected_sha" || {
        echo "release target does not match the approved commit" >&2
        exit 1
    }

    printf '%s\n' "$release_json" |
        jq -r '.assets[].name' |
        LC_ALL=C sort > "$work/actual-assets"
    cmp -s "$work/expected-assets" "$work/actual-assets" || {
        echo "release asset inventory changed after verification" >&2
        exit 1
    }
}

transition_decision() {
    state=$release_draft:$release_prerelease:$release_latest
    case "$mode:$state" in
        publish-prerelease:true:false:false)
            printf '%s\n' patch
            ;;
        publish-prerelease:false:true:false)
            printf '%s\n' noop-prerelease
            ;;
        publish-prerelease:false:false:true|\
        publish-prerelease:false:false:false)
            printf '%s\n' noop-stable
            ;;
        promote-stable:false:true:false)
            printf '%s\n' patch
            ;;
        promote-stable:false:false:true|\
        promote-stable:false:false:false)
            printf '%s\n' noop-stable
            ;;
        *)
            echo "release is in an invalid state for $mode: $state" >&2
            exit 1
            ;;
    esac
}

load_and_validate
decision=$(transition_decision)
case "$decision" in
    noop-*)
        printf 'release %s is already in monotonic state %s\n' "$tag" "$decision"
        exit 0
        ;;
esac

# Approvals can wait. Re-read every identity and state immediately before the
# numeric-ID PATCH rather than relying on the earlier verification job.
load_and_validate
decision=$(transition_decision)
case "$decision" in
    noop-*)
        printf 'release %s advanced before mutation; leaving %s unchanged\n' \
            "$tag" "$decision"
        exit 0
        ;;
esac

case "$mode" in
    publish-prerelease)
        jq -n '{
          draft: false,
          prerelease: true,
          make_latest: "false"
        }' > "$work/transition.json"
        ;;
    promote-stable)
        jq -n '{
          draft: false,
          prerelease: false,
          make_latest: "true"
        }' > "$work/transition.json"
        ;;
esac
gh api \
    --method PATCH \
    "repos/$repository/releases/$expected_release_id" \
    --input "$work/transition.json" \
    >/dev/null

load_and_validate
case "$mode:$release_draft:$release_prerelease:$release_latest" in
    publish-prerelease:false:true:false)
        printf 'published %s as a non-latest prerelease\n' "$tag"
        ;;
    promote-stable:false:false:true)
        printf 'promoted %s to stable/latest\n' "$tag"
        ;;
    *)
        echo "release did not reach the exact requested post-transition state" >&2
        exit 1
        ;;
esac
