#!/bin/sh
set -eu

if test "$#" -ne 1
then
    echo "usage: $0 OWNER/REPOSITORY" >&2
    exit 64
fi

repository=$1
test "$repository" = dcchuck/car-go-clean || {
    echo "environment configuration is restricted to dcchuck/car-go-clean" >&2
    exit 64
}

viewer=$(gh api user)
reviewer_id=$(
    printf '%s\n' "$viewer" |
        jq -er '.id | select(type == "number" and . > 0 and . == floor)'
)
reviewer_login=$(
    printf '%s\n' "$viewer" |
        jq -er '.login | select(type == "string" and length > 0)'
)

work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT HUP INT TERM
payload=$work/environment.json

jq -n \
    --argjson reviewer_id "$reviewer_id" \
    '{
      wait_timer: 0,
      prevent_self_review: false,
      reviewers: [{type: "User", id: $reviewer_id}],
      deployment_branch_policy: null
    }' > "$payload"

for environment in v040-prerelease v040-stable
do
    endpoint="repos/$repository/environments/$environment"
    gh api \
        --method PUT \
        "$endpoint" \
        --input "$payload" \
        >/dev/null

    configured=$(gh api "$endpoint")
    printf '%s\n' "$configured" |
        jq -e \
            --arg environment "$environment" \
            --arg reviewer_login "$reviewer_login" \
            --argjson reviewer_id "$reviewer_id" '
              .name == $environment and
              .deployment_branch_policy == null and
              ([.protection_rules[]? |
                select(.type == "required_reviewers")] | length) == 1 and
              ([.protection_rules[]? |
                select(.type != "required_reviewers")] | length) == 0 and
              ([.protection_rules[]? |
                select(.type == "required_reviewers")][0] |
                .prevent_self_review == false and
                (.reviewers | length) == 1 and
                .reviewers[0].type == "User" and
                .reviewers[0].reviewer.id == $reviewer_id and
                .reviewers[0].reviewer.login == $reviewer_login)
            ' \
            >/dev/null || {
        echo "environment readback did not match requested protection: $environment" >&2
        exit 1
    }
    printf 'configured %s with required reviewer %s (%s)\n' \
        "$environment" "$reviewer_login" "$reviewer_id"
done
