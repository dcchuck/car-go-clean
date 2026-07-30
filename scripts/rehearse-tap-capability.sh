#!/bin/sh
set -eu

tap_repository_expected=dcchuck/homebrew-tap
cleanup_name=car-go-clean-tap-rehearsal-cleanup.sh
state_name=car-go-clean-tap-rehearsal-state
umask 077

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
    state_tmp="$state_file.tmp"
    {
        printf 'version=2\n'
        printf 'repository=%s\n' "$state_repository"
        printf 'default_branch=%s\n' "$state_default_branch"
        printf 'parent_sha=%s\n' "$state_parent_sha"
        printf 'branch=%s\n' "$state_branch"
        printf 'local_commit_sha=%s\n' "$state_local_commit_sha"
        printf 'expected_head_sha=%s\n' "$state_expected_head_sha"
        printf 'branch_status=%s\n' "$state_branch_status"
        printf 'pr_status=%s\n' "$state_pr_status"
        printf 'pr_number=%s\n' "$state_pr_number"
    } > "$state_tmp"
    chmod 600 "$state_tmp"
    if ! mv "$state_tmp" "$state_file"
    then
        rm -f "$state_tmp"
        return 1
    fi
}

load_state() {
    test -f "$state_file" || die "tap cleanup state is missing: $state_file"
    test "$(wc -l < "$state_file" | tr -d ' ')" -eq 10 ||
        die "tap cleanup state has an invalid record count"

    state_version=$(sed -n 's/^version=//p' "$state_file")
    state_repository=$(sed -n 's/^repository=//p' "$state_file")
    state_default_branch=$(sed -n 's/^default_branch=//p' "$state_file")
    state_parent_sha=$(sed -n 's/^parent_sha=//p' "$state_file")
    state_branch=$(sed -n 's/^branch=//p' "$state_file")
    state_local_commit_sha=$(sed -n 's/^local_commit_sha=//p' "$state_file")
    state_expected_head_sha=$(sed -n 's/^expected_head_sha=//p' "$state_file")
    state_branch_status=$(sed -n 's/^branch_status=//p' "$state_file")
    state_pr_status=$(sed -n 's/^pr_status=//p' "$state_file")
    state_pr_number=$(sed -n 's/^pr_number=//p' "$state_file")

    test "$state_version" = 2 ||
        die "tap cleanup state has an unsupported version"
    validate_repository "$state_repository"
    case "$state_default_branch" in
        ''|*[!A-Za-z0-9._/-]*)
            die "tap cleanup state contains an invalid default branch"
            ;;
    esac
    case "$state_branch" in
        rehearsal/car-go-clean-*-*) ;;
        *) die "tap cleanup state contains an invalid rehearsal branch" ;;
    esac
    for state_sha in \
        "$state_parent_sha" \
        "$state_local_commit_sha" \
        "$state_expected_head_sha"
    do
        case "$state_sha" in
            *[!0-9a-f]*|'')
                die "tap cleanup state contains an invalid commit SHA"
                ;;
        esac
        test "${#state_sha}" -eq 40 ||
            die "tap cleanup state contains an invalid commit SHA"
    done
    test "$state_local_commit_sha" = "$state_expected_head_sha" ||
        die "tap cleanup state local commit does not match expected head"
    case "$state_branch_status" in
        pending|owned|absent) ;;
        *) die "tap cleanup state contains an invalid branch status" ;;
    esac
    case "$state_pr_status" in
        none|pending|known|closed|absent) ;;
        *) die "tap cleanup state contains an invalid PR status" ;;
    esac
    case "$state_pr_number" in
        ''|*[!0-9]*)
            test -z "$state_pr_number" ||
                die "tap cleanup state contains an invalid PR number"
            ;;
    esac
    case "$state_pr_status" in
        known|closed)
            test -n "$state_pr_number" ||
                die "tap cleanup state is missing the known PR number"
            ;;
        *)
            test -z "$state_pr_number" ||
                die "tap cleanup state has a PR number without known authority"
            ;;
    esac
}

print_branch_inspection() {
    echo "Manual inspection required; no rehearsal branch mutation was attempted." >&2
    printf "Inspect: gh api --method GET '%s'\n" \
        "repos/$state_repository/git/matching-refs/heads/$state_branch" >&2
    printf 'Expected exact ref: refs/heads/%s at %s\n' \
        "$state_branch" "$state_expected_head_sha" >&2
}

print_pr_inspection() {
    echo "Manual inspection required; no rehearsal PR mutation was attempted." >&2
    if test -n "$state_pr_number"
    then
        printf "Inspect: gh api --method GET '%s'\n" \
            "repos/$state_repository/pulls/$state_pr_number" >&2
    else
        printf "Inspect: gh api --method GET '%s' -f state=all -f '%s' -f '%s'\n" \
            "repos/$state_repository/pulls" \
            "head=dcchuck:$state_branch" \
            "base=$state_default_branch" >&2
    fi
    printf 'Expected draft PR: head dcchuck:%s at %s; base %s; repository %s\n' \
        "$state_branch" "$state_expected_head_sha" \
        "$state_default_branch" "$state_repository" >&2
}

reconcile_branch() {
    if test "$state_branch_status" = absent
    then
        reconciled_branch=absent
        return 0
    fi

    branch_inventory="$RUNNER_TEMP/car-go-clean-tap-branch-inventory.$$"
    if ! gh api \
        --method GET \
        "repos/$state_repository/git/matching-refs/heads/$state_branch" \
        > "$branch_inventory"
    then
        rm -f "$branch_inventory"
        print_branch_inspection
        return 1
    fi
    if ! jq -e --arg expected_ref "refs/heads/$state_branch" '
        type == "array" and
        all(.[];
            type == "object" and
            (.ref | type == "string") and
            (.object | type == "object") and
            (.object.sha | type == "string")
        ) and
        ([.[] | select(.ref == $expected_ref)] | length) <= 1
    ' "$branch_inventory" >/dev/null
    then
        rm -f "$branch_inventory"
        print_branch_inspection
        return 1
    fi
    branch_matches=$(jq -r --arg expected_ref "refs/heads/$state_branch" \
        '[.[] | select(.ref == $expected_ref)] | length' "$branch_inventory")
    if test "$branch_matches" -eq 0
    then
        reconciled_branch=absent
        rm -f "$branch_inventory"
        return 0
    fi
    reconciled_branch_sha=$(jq -r --arg expected_ref "refs/heads/$state_branch" \
        '.[] | select(.ref == $expected_ref) | .object.sha' "$branch_inventory")
    rm -f "$branch_inventory"
    if test "$reconciled_branch_sha" != "$state_expected_head_sha"
    then
        print_branch_inspection
        return 1
    fi
    reconciled_branch=owned
}

validate_pr_record() {
    pr_json_file=$1
    jq -e \
        --arg repository "$state_repository" \
        --arg branch "$state_branch" \
        --arg expected_head_sha "$state_expected_head_sha" \
        --arg default_branch "$state_default_branch" '
        type == "object" and
        (.number | type == "number") and
        (.number | floor) == .number and
        .number > 0 and
        .draft == true and
        (.state == "open" or .state == "closed") and
        .head.ref == $branch and
        .head.sha == $expected_head_sha and
        .head.repo.full_name == $repository and
        .base.ref == $default_branch and
        .base.repo.full_name == $repository
    ' "$pr_json_file" >/dev/null
}

reconcile_pr() {
    case "$state_pr_status" in
        none|absent)
            reconciled_pr=absent
            reconciled_pr_number=
            return 0
            ;;
        pending)
            pr_inventory="$RUNNER_TEMP/car-go-clean-tap-pr-inventory.$$"
            if ! gh api \
                --method GET \
                "repos/$state_repository/pulls" \
                -f state=all \
                -f "head=dcchuck:$state_branch" \
                -f "base=$state_default_branch" \
                > "$pr_inventory"
            then
                rm -f "$pr_inventory"
                print_pr_inspection
                return 1
            fi
            if ! jq -e 'type == "array"' "$pr_inventory" >/dev/null
            then
                rm -f "$pr_inventory"
                print_pr_inspection
                return 1
            fi
            pr_matches=$(jq -r 'length' "$pr_inventory")
            case "$pr_matches" in
                0)
                    reconciled_pr=absent
                    reconciled_pr_number=
                    rm -f "$pr_inventory"
                    return 0
                    ;;
                1) ;;
                *)
                    rm -f "$pr_inventory"
                    print_pr_inspection
                    return 1
                    ;;
            esac
            pr_record="$RUNNER_TEMP/car-go-clean-tap-pr-record.$$"
            jq '.[0]' "$pr_inventory" > "$pr_record"
            rm -f "$pr_inventory"
            ;;
        known|closed)
            pr_record="$RUNNER_TEMP/car-go-clean-tap-pr-record.$$"
            if ! gh api \
                --method GET \
                "repos/$state_repository/pulls/$state_pr_number" \
                > "$pr_record"
            then
                rm -f "$pr_record"
                print_pr_inspection
                return 1
            fi
            ;;
    esac

    if ! validate_pr_record "$pr_record"
    then
        rm -f "$pr_record"
        print_pr_inspection
        return 1
    fi
    reconciled_pr_number=$(jq -r '.number' "$pr_record")
    if test -n "$state_pr_number" &&
       test "$reconciled_pr_number" != "$state_pr_number"
    then
        rm -f "$pr_record"
        print_pr_inspection
        return 1
    fi
    reconciled_pr=$(jq -r '.state' "$pr_record")
    rm -f "$pr_record"
}

cleanup_loaded_state() {
    cleanup_failed=0
    if ! reconcile_pr
    then
        cleanup_failed=1
    fi
    if ! reconcile_branch
    then
        cleanup_failed=1
    fi
    if test "$cleanup_failed" -ne 0
    then
        echo "tap rehearsal cleanup is incomplete" >&2
        return 1
    fi

    checkpoint_needed=0
    case "$reconciled_pr" in
        absent)
            if test "$state_pr_status" != absent
            then
                state_pr_status=absent
                state_pr_number=
                checkpoint_needed=1
            fi
            ;;
        open)
            if test "$state_pr_status" != known ||
               test "$state_pr_number" != "$reconciled_pr_number"
            then
                state_pr_status=known
                state_pr_number=$reconciled_pr_number
                checkpoint_needed=1
            fi
            ;;
        closed)
            if test "$state_pr_status" != closed ||
               test "$state_pr_number" != "$reconciled_pr_number"
            then
                state_pr_status=closed
                state_pr_number=$reconciled_pr_number
                checkpoint_needed=1
            fi
            ;;
    esac
    case "$reconciled_branch" in
        absent)
            if test "$state_branch_status" != absent
            then
                state_branch_status=absent
                checkpoint_needed=1
            fi
            ;;
        owned)
            if test "$state_branch_status" != owned
            then
                state_branch_status=owned
                checkpoint_needed=1
            fi
            ;;
    esac
    if test "$checkpoint_needed" -eq 1 && ! write_state
    then
        echo "tap cleanup authority checkpoint failed; retry the cleanup hook" >&2
        return 1
    fi

    if test "$reconciled_pr" = open
    then
        if ! gh api \
            --method PATCH \
            "repos/$state_repository/pulls/$state_pr_number" \
            -f state=closed \
            --silent
        then
            printf "Manual cleanup required after exact ownership verification: gh api --method PATCH '%s' -f state=closed\n" \
                "repos/$state_repository/pulls/$state_pr_number" >&2
            return 1
        fi
        state_pr_status=closed
        if ! write_state
        then
            echo "PR was closed but its checkpoint failed; retry the cleanup hook to reconcile it" >&2
            return 1
        fi
    fi

    if test "$reconciled_branch" = owned
    then
        # GitHub exposes no conditional delete for refs. Reconciliation narrows
        # authority immediately before DELETE, but an API-check-to-write race
        # remains and is an unavoidable residual TOCTOU at this boundary.
        if ! gh api \
            --method DELETE \
            "repos/$state_repository/git/refs/heads/$state_branch" \
            --silent
        then
            printf "Manual cleanup required after exact ownership verification: gh api --method DELETE '%s'\n" \
                "repos/$state_repository/git/refs/heads/$state_branch" >&2
            return 1
        fi
        state_branch_status=absent
        if ! write_state
        then
            echo "branch was deleted but its checkpoint failed; retry the cleanup hook to reconcile it" >&2
            return 1
        fi
    fi

    rm -f "$state_file" "$state_file.tmp" "$cleanup_hook"
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
    for required_command in gh jq sed
    do
        command -v "$required_command" >/dev/null 2>&1 ||
            die "required command is unavailable: $required_command"
    done
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
preflight_exit() {
    exit_status=$?
    trap - EXIT HUP INT TERM
    rm -rf "$work"
    exit "$exit_status"
}
trap preflight_exit EXIT
trap 'exit 129' HUP
trap 'exit 130' INT
trap 'exit 143' TERM

clone="$work/tap"
gh repo clone "$tap_repository" "$clone"
clone_parent_sha=$(git -C "$clone" rev-parse "refs/remotes/origin/$default_branch^{commit}")
case "$clone_parent_sha" in
    *[!0-9a-f]*|'') die "cloned tap default ref did not resolve to a commit SHA" ;;
esac
test "${#clone_parent_sha}" -eq 40 ||
    die "cloned tap default ref did not resolve to a commit SHA"

default_ref_inventory="$work/default-ref.json"
gh api \
    --method GET \
    "repos/$tap_repository/git/ref/heads/$default_branch" \
    > "$default_ref_inventory"
api_parent_sha=$(jq -er --arg expected_ref "refs/heads/$default_branch" '
    select(
        type == "object" and
        .ref == $expected_ref and
        (.object | type == "object") and
        (.object.sha | type == "string")
    ) |
    .object.sha
' "$default_ref_inventory") ||
    die "GitHub tap default ref is malformed"
test "$api_parent_sha" = "$clone_parent_sha" ||
    die "cloned tap parent differs from GitHub default ref; refusing public mutation"

tree_inventory="$work/tree-inventory.json"
gh api \
    --method GET \
    "repos/$tap_repository/git/trees/$clone_parent_sha?recursive=1" \
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

git -C "$clone" checkout -qb "$branch" "$clone_parent_sha"
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
expected_head_sha=$(git -C "$clone" rev-parse "HEAD^{commit}")

state_repository=$tap_repository
state_default_branch=$default_branch
state_parent_sha=$clone_parent_sha
state_branch=$branch
state_local_commit_sha=$expected_head_sha
state_expected_head_sha=$expected_head_sha
state_branch_status=pending
state_pr_status=none
state_pr_number=

{
    printf '%s\n' '#!/bin/sh' 'set -eu'
    # shellcheck disable=SC2016 # expanded when the generated hook runs
    printf '%s\n' \
        'exec "$GITHUB_WORKSPACE/scripts/rehearse-tap-capability.sh" --cleanup-state "$RUNNER_TEMP/car-go-clean-tap-rehearsal-state"'
} > "$cleanup_hook"
chmod 700 "$cleanup_hook"
if ! write_state
then
    rm -f "$cleanup_hook" "$state_file" "$state_file.tmp"
    die "could not persist pending tap rehearsal authority"
fi

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

git \
    -c credential.helper= \
    -c credential.helper='!gh auth git-credential' \
    -C "$clone" \
    push --quiet origin \
    "HEAD:refs/heads/$branch" \
    --force-with-lease="refs/heads/$branch:"
if ! reconcile_branch || test "$reconciled_branch" != owned
then
    die "tap branch write could not be verified at the expected commit"
fi
state_branch_status=owned
write_state

current_default_branch=$(
    gh api \
        --method GET \
        "repos/$tap_repository" \
        --jq .default_branch
)
test "$current_default_branch" = "$default_branch" ||
    die "tap default branch changed before draft PR creation"
gh api \
    --method GET \
    "repos/$tap_repository/git/ref/heads/$default_branch" \
    > "$default_ref_inventory"
current_parent_sha=$(jq -er --arg expected_ref "refs/heads/$default_branch" '
    select(
        type == "object" and
        .ref == $expected_ref and
        (.object | type == "object") and
        (.object.sha | type == "string")
    ) |
    .object.sha
' "$default_ref_inventory") ||
    die "GitHub tap default ref became malformed before draft PR creation"
test "$current_parent_sha" = "$clone_parent_sha" ||
    die "tap default ref changed before draft PR creation"
if ! reconcile_branch || test "$reconciled_branch" != owned
then
    die "tap rehearsal branch changed before draft PR creation"
fi

state_pr_status=pending
state_pr_number=
write_state

# GitHub offers no atomic "verify these refs, then create PR" operation. These
# ref checks are immediately adjacent to POST, but a residual API-check-to-write
# TOCTOU remains at the public mutation boundary.
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
state_pr_status=known
state_pr_number=$pr_number
write_state

pr_record="$work/pr-record.json"
gh api \
    --method GET \
    "repos/$tap_repository/pulls/$pr_number" \
    > "$pr_record"
if ! validate_pr_record "$pr_record" ||
   test "$(jq -r '.number' "$pr_record")" != "$pr_number" ||
   test "$(jq -r '.state' "$pr_record")" != open
then
    die "tap draft PR write could not be verified"
fi

printf 'Tap capability verified for %s\n' "$tap_repository"
printf 'Temporary draft PR: %s\n' "$pr_number"
printf 'Temporary branch: %s\n' "$branch"
