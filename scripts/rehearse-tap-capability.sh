#!/bin/sh
set -eu

tap_repository_expected=dcchuck/homebrew-tap
cleanup_name=car-go-clean-tap-rehearsal-cleanup.sh
state_name=car-go-clean-tap-rehearsal-state

die() {
    echo "$*" >&2
    exit 1
}

require_token() {
    test -n "${HOMEBREW_TAP_TOKEN:-}" ||
        die "HOMEBREW_TAP_TOKEN is required for tap capability rehearsal"
    GH_TOKEN=$HOMEBREW_TAP_TOKEN
    export GH_TOKEN
    unset HOMEBREW_TAP_TOKEN
}

validate_repository() {
    test "$1" = "$tap_repository_expected" ||
        die "tap rehearsal is restricted to $tap_repository_expected"
}

validate_run_number() {
    case "$2" in
        ''|*[!0-9]*)
            die "$1 must contain decimal digits only"
            ;;
    esac
}

write_state() {
    state_tmp="$state_file.tmp.$$"
    {
        printf 'repository=%s\n' "$state_repository"
        printf 'branch=%s\n' "$state_branch"
        printf 'branch_created=%s\n' "$state_branch_created"
        printf 'pr_number=%s\n' "$state_pr_number"
    } > "$state_tmp"
    chmod 600 "$state_tmp"
    mv "$state_tmp" "$state_file"
}

load_state() {
    test -f "$state_file" || die "tap cleanup state is missing: $state_file"
    test "$(wc -l < "$state_file" | tr -d ' ')" -eq 4 ||
        die "tap cleanup state has an invalid record count"

    state_repository=$(sed -n 's/^repository=//p' "$state_file")
    state_branch=$(sed -n 's/^branch=//p' "$state_file")
    state_branch_created=$(sed -n 's/^branch_created=//p' "$state_file")
    state_pr_number=$(sed -n 's/^pr_number=//p' "$state_file")

    validate_repository "$state_repository"
    case "$state_branch" in
        rehearsal/car-go-clean-*-*) ;;
        *) die "tap cleanup state contains an invalid rehearsal branch" ;;
    esac
    case "$state_branch_created" in
        0|1) ;;
        *) die "tap cleanup state contains an invalid branch state" ;;
    esac
    case "$state_pr_number" in
        ''|*[!0-9]*)
            test -z "$state_pr_number" ||
                die "tap cleanup state contains an invalid PR number"
            ;;
    esac
}

cleanup_loaded_state() {
    cleanup_failed=0

    if test -n "$state_pr_number"
    then
        if gh api \
            --method PATCH \
            "repos/$state_repository/pulls/$state_pr_number" \
            -f state=closed \
            --silent
        then
            state_pr_number=
            write_state
        else
            cleanup_failed=1
            printf "Manual cleanup required: gh api --method PATCH '%s' -f state=closed\n" \
                "repos/$state_repository/pulls/$state_pr_number" >&2
        fi
    fi

    if test "$state_branch_created" -eq 1
    then
        if gh api \
            --method DELETE \
            "repos/$state_repository/git/refs/heads/$state_branch" \
            --silent
        then
            state_branch_created=0
            write_state
        else
            cleanup_failed=1
            printf "Manual cleanup required: gh api --method DELETE '%s'\n" \
                "repos/$state_repository/git/refs/heads/$state_branch" >&2
        fi
    fi

    if test "$cleanup_failed" -ne 0
    then
        echo "tap rehearsal cleanup is incomplete" >&2
        return 1
    fi

    rm -f "$state_file" "$cleanup_hook"
}

cleanup_state() {
    load_state
    cleanup_loaded_state
}

test -n "${RUNNER_TEMP:-}" || die "RUNNER_TEMP is required"
state_file="$RUNNER_TEMP/$state_name"
cleanup_hook="$RUNNER_TEMP/$cleanup_name"

if test "${1:-}" = --cleanup-state
then
    test "$#" -eq 2 || die "usage: rehearse-tap-capability.sh --cleanup-state STATE"
    test "$2" = "$state_file" ||
        die "cleanup is restricted to the current runner state file"
    require_token
    cleanup_state
    exit 0
fi
test "$#" -eq 0 || die "usage: rehearse-tap-capability.sh"

require_token
for required_command in gh git jq sed
do
    command -v "$required_command" >/dev/null 2>&1 ||
        die "required command is unavailable: $required_command"
done

tap_repository=${TAP_REPOSITORY:-}
validate_repository "$tap_repository"
validate_run_number GITHUB_RUN_ID "${GITHUB_RUN_ID:-}"
validate_run_number GITHUB_RUN_ATTEMPT "${GITHUB_RUN_ATTEMPT:-}"
test -n "${GITHUB_WORKSPACE:-}" || die "GITHUB_WORKSPACE is required"

branch="rehearsal/car-go-clean-${GITHUB_RUN_ID}-${GITHUB_RUN_ATTEMPT}"
evidence_relative=".release-rehearsal/${GITHUB_RUN_ID}.txt"

default_branch=$(
    gh api \
        --method GET \
        "repos/$tap_repository" \
        --jq .default_branch
)
case "$default_branch" in
    ''|*[!A-Za-z0-9._/-]*)
        die "tap default branch is missing or unsafe"
        ;;
esac
test "$branch" != "$default_branch" ||
    die "refusing to use the tap default branch for rehearsal"

gh api \
    --method GET \
    "repos/$tap_repository/contents" \
    -f "ref=$default_branch" \
    --silent

branch_inventory=$(mktemp)
gh api \
    --method GET \
    "repos/$tap_repository/branches" \
    --paginate \
    --jq '.[].name' \
    > "$branch_inventory"
if grep -Fqx "$branch" "$branch_inventory"
then
    rm -f "$branch_inventory"
    die "refusing to reuse existing tap branch: $branch"
fi
rm -f "$branch_inventory"

work=$(mktemp -d)
state_repository=$tap_repository
state_branch=$branch
state_branch_created=0
state_pr_number=
write_state

umask 077
{
    printf '%s\n' '#!/bin/sh' 'set -eu'
    # shellcheck disable=SC2016 # expanded when the generated hook runs
    printf '%s\n' \
        'exec "$GITHUB_WORKSPACE/scripts/rehearse-tap-capability.sh" --cleanup-state "$RUNNER_TEMP/car-go-clean-tap-rehearsal-state"'
} > "$cleanup_hook"
chmod 700 "$cleanup_hook"

on_exit() {
    exit_status=$?
    trap - EXIT HUP INT TERM
    if ! cleanup_loaded_state
    then
        exit_status=1
    fi
    rm -rf "$work"
    exit "$exit_status"
}
trap on_exit EXIT
trap 'exit 129' HUP
trap 'exit 130' INT
trap 'exit 143' TERM

clone="$work/tap"
gh repo clone "$tap_repository" "$clone"
git -C "$clone" checkout -qb "$branch" "origin/$default_branch"
git -C "$clone" config user.name car-go-clean-release-rehearsal
git -C "$clone" config user.email car-go-clean-release-rehearsal@users.noreply.github.com
git -C "$clone" config commit.gpgsign false

mkdir -p "$clone/.release-rehearsal"
{
    printf 'car-go-clean release rehearsal\n'
    printf 'run_id=%s\n' "$GITHUB_RUN_ID"
    printf 'run_attempt=%s\n' "$GITHUB_RUN_ATTEMPT"
    printf 'branch=%s\n' "$branch"
} > "$clone/$evidence_relative"
git -C "$clone" add -- "$evidence_relative"

staged_paths=$(git -C "$clone" diff --cached --name-only)
test "$staged_paths" = "$evidence_relative" ||
    die "tap rehearsal staged files outside $evidence_relative"
status=$(git -C "$clone" status --porcelain --untracked-files=all)
test "$status" = "A  $evidence_relative" ||
    die "tap rehearsal checkout contains unexpected changes"
git -C "$clone" commit -qm \
    "chore: rehearse car-go-clean release ${GITHUB_RUN_ID}-${GITHUB_RUN_ATTEMPT}"

tree_inventory="$work/tree-inventory.json"
gh api \
    --method GET \
    "repos/$tap_repository/git/trees/$default_branch?recursive=1" \
    > "$tree_inventory"
if ! jq -e \
    '.truncated == false and (.tree | type == "array")' \
    "$tree_inventory" >/dev/null
then
    die "tap tree inventory is truncated or malformed; refusing public mutation"
fi
workflow_paths="$work/workflow-paths"
jq -r \
    '.tree[] | select(.type == "blob" and (.path | startswith(".github/workflows/"))) | .path' \
    "$tree_inventory" > "$workflow_paths"
if test -s "$workflow_paths"
then
    echo "tap workflow files appeared before mutation:" >&2
    sed 's/^/  /' "$workflow_paths" >&2
    die "refusing rehearsal until every tap workflow has an independently validated rehearsal/** ignore rule"
fi

git \
    -c credential.helper= \
    -c credential.helper='!gh auth git-credential' \
    -C "$clone" \
    push --quiet origin \
    "HEAD:refs/heads/$branch" \
    --force-with-lease="refs/heads/$branch:"
state_branch_created=1
write_state

actual_ref=$(
    gh api \
        --method GET \
        "repos/$tap_repository/git/ref/heads/$branch" \
        --jq .ref
)
test "$actual_ref" = "refs/heads/$branch" ||
    die "tap branch write could not be verified"

pr_number=$(
    gh api \
        --method POST \
        "repos/$tap_repository/pulls" \
        -f "title=Release rehearsal ${GITHUB_RUN_ID}-${GITHUB_RUN_ATTEMPT}" \
        -f "head=$branch" \
        -f "base=$default_branch" \
        -f "body=Temporary permission proof; no formula or default-branch mutation." \
        -F draft=true \
        --jq .number
)
case "$pr_number" in
    ''|*[!0-9]*) die "tap draft PR creation returned an invalid PR number" ;;
esac
state_pr_number=$pr_number
write_state

pr_facts=$(
    gh api \
        --method GET \
        "repos/$tap_repository/pulls/$pr_number" \
        --jq '"\(.number)\t\(.draft)\t\(.state)\t\(.head.ref)\t\(.base.ref)"'
)
expected_pr_facts=$(printf '%s\ttrue\topen\t%s\t%s' \
    "$pr_number" "$branch" "$default_branch")
test "$pr_facts" = "$expected_pr_facts" ||
    die "tap draft PR write could not be verified"

printf 'Tap capability verified for %s\n' "$tap_repository"
printf 'Temporary draft PR: %s\n' "$pr_number"
printf 'Temporary branch: %s\n' "$branch"
