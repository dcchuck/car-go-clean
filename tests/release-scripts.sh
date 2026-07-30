#!/bin/sh
set -eu

repo_root=$(CDPATH='' cd -- "$(dirname -- "$0")/.." && pwd)
validator="$repo_root/scripts/validate-release-inputs.sh"
dist_installer="$repo_root/scripts/install-cargo-dist.sh"
asset_verifier="$repo_root/scripts/verify-release-assets.sh"
formula_renderer="$repo_root/scripts/render-homebrew-formula.sh"
tap_rehearsal="$repo_root/scripts/rehearse-tap-capability.sh"
draft_upserter="$repo_root/scripts/upsert-draft-release.sh"
environment_configurator="$repo_root/scripts/configure-release-environments.sh"
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT HUP INT TERM

expect_failure() {
    description=$1
    shift
    if "$@"
    then
        echo "unexpected success: $description" >&2
        exit 1
    fi
}

hash_file() {
    if command -v shasum >/dev/null 2>&1
    then
        shasum -a 256 "$1" | awk '{ print $1 }'
    else
        sha256sum "$1" | awk '{ print $1 }'
    fi
}

file_mode() {
    mode=$(stat -f '%Lp' "$1" 2>/dev/null || :)
    case "$mode" in
        ''|*[!0-9]*) mode=$(stat -c '%a' "$1") ;;
    esac
    printf '%s\n' "$mode"
}

run_validator() {
    validation_worktree=$1
    shift
    (
        cd "$validation_worktree"
        "$validator" "$@"
    )
}

for script in \
    "$validator" \
    "$dist_installer" \
    "$asset_verifier" \
    "$formula_renderer" \
    "$tap_rehearsal" \
    "$draft_upserter" \
    "$environment_configurator"
do
    sh -n "$script"
done

validation_repo="$work/validation-repo"
origin="$work/origin.git"
mkdir -p "$validation_repo/src"
git init -q --bare "$origin"
git -C "$validation_repo" init -q -b main
git -C "$validation_repo" config user.name "Release Test"
git -C "$validation_repo" config user.email "release-test@example.invalid"
git -C "$validation_repo" config commit.gpgsign false
printf '%s\n' \
    '[package]' \
    'name = "car-go-clean"' \
    'version = "0.4.0"' \
    'edition = "2021"' \
    > "$validation_repo/Cargo.toml"
printf '%s\n' 'pub fn fixture() {}' > "$validation_repo/src/lib.rs"
git -C "$validation_repo" add Cargo.toml src/lib.rs
git -C "$validation_repo" commit -qm "fixture"
git -C "$validation_repo" remote add origin "$origin"
git -C "$validation_repo" push -qu origin main
release_sha=$(git -C "$validation_repo" rev-parse HEAD)

validation_output=$(
    cd "$validation_repo"
    "$validator" "$release_sha" 0.4.0
)
printf '%s\n' "$validation_output" | grep -qx "RELEASE_SHA=$release_sha"
printf '%s\n' "$validation_output" | grep -qx 'VERSION=0.4.0'
printf '%s\n' "$validation_output" | grep -qx 'TAG=v0.4.0'

ancestor_release_sha=$release_sha
printf '%s\n' 'second same-version commit' > "$validation_repo/second-commit"
git -C "$validation_repo" add second-commit
git -C "$validation_repo" commit -qm "second same-version fixture"
git -C "$validation_repo" push -qu origin main
release_sha=$(git -C "$validation_repo" rev-parse HEAD)
expect_failure "requested ancestor differs from checkout HEAD" \
    run_validator "$validation_repo" "$ancestor_release_sha" 0.4.0

expect_failure "short commit SHA" \
    run_validator "$validation_repo" 01234567 0.4.0
expect_failure "unreachable commit SHA" \
    run_validator "$validation_repo" ffffffffffffffffffffffffffffffffffffffff 0.4.0
expect_failure "malformed version" \
    run_validator "$validation_repo" "$release_sha" 0.4
expect_failure "Cargo version mismatch" \
    run_validator "$validation_repo" "$release_sha" 0.4.1

printf '%s\n' '# dirty' >> "$validation_repo/Cargo.toml"
expect_failure "dirty checkout" \
    run_validator "$validation_repo" "$release_sha" 0.4.0
git -C "$validation_repo" checkout -q -- Cargo.toml

git -C "$validation_repo" switch -qc not-on-main
printf '%s\n' 'not on main' > "$validation_repo/branch-only"
git -C "$validation_repo" add branch-only
git -C "$validation_repo" commit -qm "branch only"
branch_sha=$(git -C "$validation_repo" rev-parse HEAD)
expect_failure "commit not contained by origin/main" \
    run_validator "$validation_repo" "$branch_sha" 0.4.0
git -C "$validation_repo" switch -q main

git -C "$validation_repo" tag v0.4.0 "$release_sha"
expect_failure "existing local version tag" \
    run_validator "$validation_repo" "$release_sha" 0.4.0
git -C "$validation_repo" push -q origin refs/tags/v0.4.0
git -C "$validation_repo" tag -d v0.4.0 >/dev/null
expect_failure "existing remote version tag" \
    run_validator "$validation_repo" "$release_sha" 0.4.0

fake_bin="$work/fake-bin"
mkdir -p "$fake_bin"
printf '%s\n' '#!/bin/sh' 'exit 0' > "$work/original-installer.sh"
cp "$work/original-installer.sh" "$work/mutated-installer.sh"
printf 'x' >> "$work/mutated-installer.sh"
cat > "$fake_bin/curl" <<'EOF'
#!/bin/sh
set -eu
output=
while test "$#" -gt 0
do
    case "$1" in
        -o) output=$2; shift 2 ;;
        *) shift ;;
    esac
done
cp "$MUTATED_INSTALLER" "$output"
EOF
chmod +x "$fake_bin/curl"
expect_failure "one-byte-mutated cargo-dist installer" \
    env PATH="$fake_bin:$PATH" MUTATED_INSTALLER="$work/mutated-installer.sh" "$dist_installer"

artifacts="$work/artifacts"
mkdir -p "$artifacts"
archives='
car-go-clean-aarch64-apple-darwin.tar.xz
car-go-clean-x86_64-apple-darwin.tar.xz
car-go-clean-aarch64-unknown-linux-musl.tar.xz
car-go-clean-x86_64-unknown-linux-musl.tar.xz
'
for archive in $archives
do
    printf 'archive fixture: %s\n' "$archive" > "$artifacts/$archive"
    case "$archive" in
        car-go-clean-aarch64-apple-darwin.tar.xz)
            # Preserve cargo-dist 0.32.0's complete emitted bytes: binary
            # marker followed by its observed single terminal empty record.
            printf '%s *%s\n\n' "$(hash_file "$artifacts/$archive")" "$archive" \
                > "$artifacts/$archive.sha256"
            ;;
        *)
            printf '%s  %s\n' "$(hash_file "$artifacts/$archive")" "$archive" \
                > "$artifacts/$archive.sha256"
            ;;
    esac
done

inventory=$("$asset_verifier" v0.4.0 "$artifacts")
test "$(printf '%s\n' "$inventory" | awk 'NF { count++ } END { print count + 0 }')" -eq 4
for archive in $archives
do
    printf '%s\n' "$inventory" | awk -F '	' -v archive="$archive" '
        $1 == archive && $2 ~ /^[0-9a-f]{64}$/ { found++ }
        END { exit(found == 1 ? 0 : 1) }
    '
done

rm "$artifacts/car-go-clean-aarch64-apple-darwin.tar.xz"
expect_failure "missing archive" "$asset_verifier" v0.4.0 "$artifacts"
printf 'archive fixture: %s\n' car-go-clean-aarch64-apple-darwin.tar.xz \
    > "$artifacts/car-go-clean-aarch64-apple-darwin.tar.xz"
printf '%s  %s\n' \
    "$(hash_file "$artifacts/car-go-clean-aarch64-apple-darwin.tar.xz")" \
    car-go-clean-aarch64-apple-darwin.tar.xz \
    > "$artifacts/car-go-clean-aarch64-apple-darwin.tar.xz.sha256"

rm "$artifacts/car-go-clean-x86_64-apple-darwin.tar.xz.sha256"
expect_failure "missing checksum" "$asset_verifier" v0.4.0 "$artifacts"
printf '%s  %s\n' \
    "$(hash_file "$artifacts/car-go-clean-x86_64-apple-darwin.tar.xz")" \
    car-go-clean-x86_64-apple-darwin.tar.xz \
    > "$artifacts/car-go-clean-x86_64-apple-darwin.tar.xz.sha256"

duplicate_dir="$artifacts/duplicate"
mkdir "$duplicate_dir"
cp "$artifacts/car-go-clean-aarch64-apple-darwin.tar.xz" "$duplicate_dir/"
expect_failure "duplicate archive basename" "$asset_verifier" v0.4.0 "$artifacts"
rm -rf "$duplicate_dir"

checksum="$artifacts/car-go-clean-aarch64-apple-darwin.tar.xz.sha256"
cp "$checksum" "$work/good-checksum"
printf '%s\n' 'malformed checksum line' > "$checksum"
expect_failure "malformed checksum line" "$asset_verifier" v0.4.0 "$artifacts"
cp "$work/good-checksum" "$checksum"
cat "$work/good-checksum" >> "$checksum"
expect_failure "duplicate checksum lines" "$asset_verifier" v0.4.0 "$artifacts"
cp "$work/good-checksum" "$checksum"

checksum_hash=$(hash_file "$artifacts/car-go-clean-aarch64-apple-darwin.tar.xz")
printf '\n%s *%s\n' \
    "$checksum_hash" \
    car-go-clean-aarch64-apple-darwin.tar.xz \
    > "$checksum"
expect_failure "checksum with a leading blank record" \
    "$asset_verifier" v0.4.0 "$artifacts"
printf '%s *%s\n\n%s *%s\n' \
    "$checksum_hash" \
    car-go-clean-aarch64-apple-darwin.tar.xz \
    "$checksum_hash" \
    car-go-clean-aarch64-apple-darwin.tar.xz \
    > "$checksum"
expect_failure "checksum with an embedded blank record" \
    "$asset_verifier" v0.4.0 "$artifacts"
printf '%s *%s\n\n\n' \
    "$checksum_hash" \
    car-go-clean-aarch64-apple-darwin.tar.xz \
    > "$checksum"
expect_failure "checksum with two terminal blank records" \
    "$asset_verifier" v0.4.0 "$artifacts"
printf '%s *%s\n' \
    "$checksum_hash" \
    car-go-clean-wrong-target.tar.xz \
    > "$checksum"
expect_failure "binary checksum for the wrong basename" \
    "$asset_verifier" v0.4.0 "$artifacts"
printf '%s   %s\n' \
    "$checksum_hash" \
    car-go-clean-aarch64-apple-darwin.tar.xz \
    > "$checksum"
expect_failure "checksum with a nonstandard marker" \
    "$asset_verifier" v0.4.0 "$artifacts"
printf '%s -%s\n' \
    "$checksum_hash" \
    car-go-clean-aarch64-apple-darwin.tar.xz \
    > "$checksum"
expect_failure "checksum with a malformed marker" \
    "$asset_verifier" v0.4.0 "$artifacts"
printf '%s *%s\n' \
    "$(printf '%s' "$checksum_hash" | tr 'a-f' 'A-F')" \
    car-go-clean-aarch64-apple-darwin.tar.xz \
    > "$checksum"
expect_failure "checksum with uppercase hexadecimal" \
    "$asset_verifier" v0.4.0 "$artifacts"
cp "$work/good-checksum" "$checksum"

extra=car-go-clean-extra-target.tar.xz
printf '%s\n' extra > "$artifacts/$extra"
printf '%s  %s\n' "$(hash_file "$artifacts/$extra")" "$extra" > "$artifacts/$extra.sha256"
expect_failure "extra archive and checksum" "$asset_verifier" v0.4.0 "$artifacts"
rm "$artifacts/$extra" "$artifacts/$extra.sha256"

formula="$work/car-go-clean.rb"
"$formula_renderer" v0.4.0 "$artifacts" "$formula"
if grep -Eq '__[A-Z0-9_]+__' "$formula"
then
    echo "rendered formula contains an unresolved placeholder" >&2
    exit 1
fi
test "$(grep -o __TAG__ "$repo_root/packaging/release/homebrew/car-go-clean.rb.in" | wc -l | tr -d ' ')" -eq 4
for placeholder in \
    __AARCH64_APPLE_SHA256__ \
    __X86_64_APPLE_SHA256__ \
    __AARCH64_LINUX_SHA256__ \
    __X86_64_LINUX_SHA256__
do
    test "$(grep -o "$placeholder" "$repo_root/packaging/release/homebrew/car-go-clean.rb.in" | wc -l | tr -d ' ')" -eq 1
done
for archive in $archives
do
    hash=$(hash_file "$artifacts/$archive")
    test "$(grep -F -c "https://github.com/dcchuck/car-go-clean/releases/download/v0.4.0/$archive" "$formula")" -eq 1
    test "$(grep -F -c "sha256 \"$hash\"" "$formula")" -eq 1
done
expect_failure "malformed formula tag" "$formula_renderer" 0.4.0 "$artifacts" "$work/bad.rb"

environment_fake_bin="$work/environment-fake-bin"
mkdir -p "$environment_fake_bin"
cat > "$environment_fake_bin/gh" <<'EOF'
#!/bin/sh
set -eu

printf '%s\n' "$*" >> "$FAKE_ENV_LOG"
test "$1" = api
shift

method=GET
endpoint=
input=
while test "$#" -gt 0
do
    case "$1" in
        --method) method=$2; shift 2 ;;
        --input) input=$2; shift 2 ;;
        --silent) shift ;;
        -*) echo "unexpected fake gh option: $1" >&2; exit 2 ;;
        *) test -z "$endpoint"; endpoint=$1; shift ;;
    esac
done

case "$method:$endpoint" in
    GET:user)
        printf '%s\n' '{"id":4242,"login":"release-operator"}'
        ;;
    PUT:repos/dcchuck/car-go-clean/environments/v040-prerelease|\
    PUT:repos/dcchuck/car-go-clean/environments/v040-stable)
        test -f "$input"
        environment=${endpoint##*/}
        cp "$input" "$FAKE_ENV_STATE/$environment.put.json"
        printf '%s\n' '{}'
        ;;
    GET:repos/dcchuck/car-go-clean/environments/v040-prerelease|\
    GET:repos/dcchuck/car-go-clean/environments/v040-stable)
        environment=${endpoint##*/}
        case "${FAKE_ENV_READBACK:-exact}" in
            exact)
                jq -n --arg environment "$environment" '{
                  name: $environment,
                  protection_rules: [{
                    type: "required_reviewers",
                    prevent_self_review: false,
                    reviewers: [{
                      type: "User",
                      reviewer: {id: 4242, login: "release-operator"}
                    }]
                  }],
                  deployment_branch_policy: null
                }'
                ;;
            self-review)
                jq -n --arg environment "$environment" '{
                  name: $environment,
                  protection_rules: [{
                    type: "required_reviewers",
                    prevent_self_review: true,
                    reviewers: [{
                      type: "User",
                      reviewer: {id: 4242, login: "release-operator"}
                    }]
                  }],
                  deployment_branch_policy: null
                }'
                ;;
            wrong-reviewer)
                jq -n --arg environment "$environment" '{
                  name: $environment,
                  protection_rules: [{
                    type: "required_reviewers",
                    prevent_self_review: false,
                    reviewers: [{
                      type: "User",
                      reviewer: {id: 99, login: "someone-else"}
                    }]
                  }],
                  deployment_branch_policy: null
                }'
                ;;
            extra-reviewer)
                jq -n --arg environment "$environment" '{
                  name: $environment,
                  protection_rules: [{
                    type: "required_reviewers",
                    prevent_self_review: false,
                    reviewers: [
                      {
                        type: "User",
                        reviewer: {id: 4242, login: "release-operator"}
                      },
                      {
                        type: "User",
                        reviewer: {id: 99, login: "someone-else"}
                      }
                    ]
                  }],
                  deployment_branch_policy: null
                }'
                ;;
            *) exit 2 ;;
        esac
        ;;
    *) echo "unexpected fake gh call: $method $endpoint" >&2; exit 2 ;;
esac
EOF
chmod +x "$environment_fake_bin/gh"

wrong_repository_state="$work/environment-wrong-repository"
mkdir -p "$wrong_repository_state"
: > "$wrong_repository_state/gh.log"
expect_failure "environment configuration for another repository" \
    env \
        PATH="$environment_fake_bin:$PATH" \
        FAKE_ENV_LOG="$wrong_repository_state/gh.log" \
        FAKE_ENV_STATE="$wrong_repository_state" \
        "$environment_configurator" dcchuck/car-go-cleen
test ! -s "$wrong_repository_state/gh.log"

environment_state="$work/environment-state"
mkdir -p "$environment_state"
env \
    PATH="$environment_fake_bin:$PATH" \
    FAKE_ENV_LOG="$environment_state/gh.log" \
    FAKE_ENV_STATE="$environment_state" \
    "$environment_configurator" dcchuck/car-go-clean
for environment in v040-prerelease v040-stable
do
    jq -e '
      (keys | sort) == [
        "deployment_branch_policy",
        "prevent_self_review",
        "reviewers",
        "wait_timer"
      ] and
      .wait_timer == 0 and
      .prevent_self_review == false and
      .reviewers == [{type: "User", id: 4242}] and
      .deployment_branch_policy == null
    ' "$environment_state/$environment.put.json" >/dev/null
    test "$(grep -F -c \
        "api --method PUT repos/dcchuck/car-go-clean/environments/$environment --input" \
        "$environment_state/gh.log")" -eq 1
    test "$(grep -F -c \
        "api repos/dcchuck/car-go-clean/environments/$environment" \
        "$environment_state/gh.log")" -eq 1
done
test "$(grep -F -c 'api user' "$environment_state/gh.log")" -eq 1

for invalid_readback in self-review wrong-reviewer extra-reviewer
do
    invalid_state="$work/environment-$invalid_readback"
    mkdir -p "$invalid_state"
    expect_failure "environment readback $invalid_readback" \
        env \
            PATH="$environment_fake_bin:$PATH" \
            FAKE_ENV_LOG="$invalid_state/gh.log" \
            FAKE_ENV_STATE="$invalid_state" \
            FAKE_ENV_READBACK="$invalid_readback" \
            "$environment_configurator" dcchuck/car-go-clean
done

make_tap_origin() {
    fixture_root=$1
    fixture_source="$fixture_root/source"
    fixture_origin="$fixture_root/origin.git"
    mkdir -p "$fixture_source/Formula"
    git init -q --bare "$fixture_origin"
    git -C "$fixture_source" init -q -b main
    git -C "$fixture_source" config user.name "Tap Test"
    git -C "$fixture_source" config user.email "tap-test@example.invalid"
    git -C "$fixture_source" config commit.gpgsign false
    printf '%s\n' 'class CarGoClean < Formula; end' \
        > "$fixture_source/Formula/car-go-clean.rb"
    git -C "$fixture_source" add Formula/car-go-clean.rb
    git -C "$fixture_source" commit -qm "seed tap"
    git -C "$fixture_source" remote add origin "$fixture_origin"
    git -C "$fixture_source" push -qu origin main
    git --git-dir="$fixture_origin" symbolic-ref HEAD refs/heads/main
}

make_tap_workflow_commit() {
    fixture_root=$1
    fixture_source="$fixture_root/source"
    fixture_origin="$fixture_root/origin.git"
    mkdir -p "$fixture_source/.github/workflows"
    printf '%s\n' 'name: appeared-after-inventory' \
        > "$fixture_source/.github/workflows/ci.yml"
    git -C "$fixture_source" add .github/workflows/ci.yml
    git -C "$fixture_source" commit -qm "add workflow after rehearsal inventory"
    workflow_sha=$(git -C "$fixture_source" rev-parse HEAD)
    git -C "$fixture_source" push -qu origin \
        "HEAD:refs/heads/rehearsal-test-workflow-source"
    git --git-dir="$fixture_origin" update-ref \
        -d refs/heads/rehearsal-test-workflow-source
    printf '%s\n' "$workflow_sha"
}

tap_fake_bin="$work/tap-fake-bin"
mkdir -p "$tap_fake_bin"
real_git=$(command -v git)
real_chmod=$(command -v chmod)
real_mv=$(command -v mv)
cat > "$tap_fake_bin/git" <<'EOF'
#!/bin/sh
set -eu

is_push=0
for argument in "$@"
do
    if test "$argument" = push
    then
        is_push=1
    fi
done

if test "$is_push" -eq 1 &&
   test -n "${FAKE_PUSH_ATTEMPT_MARKER:-}"
then
    : > "$FAKE_PUSH_ATTEMPT_MARKER"
fi

if test "$is_push" -eq 1 &&
   { test -n "${FAKE_PUSH_LOST_RESPONSE:-}" ||
     test -n "${FAKE_PUSH_SIGNAL_AFTER_SUCCESS:-}" ||
     test -n "${FAKE_FAIL_NEXT_STATE_AFTER_PUSH_MARKER:-}"; }
then
    "$REAL_GIT" "$@"
    if test -n "${FAKE_FAIL_NEXT_STATE_AFTER_PUSH_MARKER:-}"
    then
        : > "$FAKE_FAIL_NEXT_STATE_AFTER_PUSH_MARKER"
    fi
    if test -n "${FAKE_PUSH_SIGNAL_AFTER_SUCCESS:-}"
    then
        kill -TERM "$PPID"
        exit 143
    fi
    if test -n "${FAKE_PUSH_LOST_RESPONSE:-}"
    then
        exit 1
    fi
    exit 0
fi

exec "$REAL_GIT" "$@"
EOF
chmod +x "$tap_fake_bin/git"

cat > "$tap_fake_bin/chmod" <<'EOF'
#!/bin/sh
set -eu

if test "$#" -eq 2 &&
   test "$2" = "$RUNNER_TEMP/car-go-clean-tap-rehearsal-state.tmp"
then
    count=0
    if test -n "${FAKE_STATE_CHMOD_COUNT_FILE:-}" &&
       test -f "$FAKE_STATE_CHMOD_COUNT_FILE"
    then
        count=$(cat "$FAKE_STATE_CHMOD_COUNT_FILE")
    fi
    count=$((count + 1))
    if test -n "${FAKE_STATE_CHMOD_COUNT_FILE:-}"
    then
        printf '%s\n' "$count" > "$FAKE_STATE_CHMOD_COUNT_FILE"
    fi
    if test -n "${FAKE_FAIL_STATE_CHMOD_NUMBER:-}" &&
       test "$count" -eq "$FAKE_FAIL_STATE_CHMOD_NUMBER"
    then
        exit 1
    fi
fi

exec "$REAL_CHMOD" "$@"
EOF
chmod +x "$tap_fake_bin/chmod"

cat > "$tap_fake_bin/mv" <<'EOF'
#!/bin/sh
set -eu

if test -n "${FAKE_STATE_WRITE_COUNT_FILE:-}"
then
    count=0
    if test -f "$FAKE_STATE_WRITE_COUNT_FILE"
    then
        count=$(cat "$FAKE_STATE_WRITE_COUNT_FILE")
    fi
    count=$((count + 1))
    printf '%s\n' "$count" > "$FAKE_STATE_WRITE_COUNT_FILE"
    if test -n "${FAKE_FAIL_STATE_WRITE_NUMBER:-}" &&
       test "$count" -eq "$FAKE_FAIL_STATE_WRITE_NUMBER"
    then
        exit 1
    fi
fi

for marker in \
    "${FAKE_FAIL_NEXT_STATE_AFTER_PUSH_MARKER:-}" \
    "${FAKE_FAIL_NEXT_STATE_AFTER_CLOSE_MARKER:-}" \
    "${FAKE_FAIL_NEXT_STATE_AFTER_DELETE_MARKER:-}"
do
    if test -n "$marker" && test -e "$marker"
    then
        rm -f "$marker"
        exit 1
    fi
done

exec "$REAL_MV" "$@"
EOF
chmod +x "$tap_fake_bin/mv"

cat > "$tap_fake_bin/gh" <<'EOF'
#!/bin/sh
set -eu

test "${GH_TOKEN:-}" = "$EXPECTED_TOKEN"
printf '%s\n' "$*" >> "$GH_LOG"

if test "$1" = repo && test "$2" = clone
then
    test "$3" = dcchuck/homebrew-tap
    git clone -q "$FAKE_TAP_ORIGIN" "$4"
    exit 0
fi

test "$1" = api
shift
method=GET
endpoint=
has_jq=0
jq_expression=
request_head=
request_base=
request_draft=
request_state=
while test "$#" -gt 0
do
    case "$1" in
        --method)
            method=$2
            shift 2
            ;;
        --method=*)
            method=${1#--method=}
            shift
            ;;
        --jq)
            has_jq=1
            jq_expression=$2
            shift 2
            ;;
        -f|-F)
            case "$2" in
                head=*) request_head=${2#head=} ;;
                base=*) request_base=${2#base=} ;;
                draft=*) request_draft=${2#draft=} ;;
                state=*) request_state=${2#state=} ;;
            esac
            shift 2
            ;;
        --silent|--paginate)
            shift
            ;;
        -*)
            shift
            ;;
        *)
            if test -z "$endpoint"
            then
                endpoint=$1
            fi
            shift
            ;;
    esac
done

branch="rehearsal/car-go-clean-${GITHUB_RUN_ID}-${GITHUB_RUN_ATTEMPT}"
default_branch=${FAKE_DEFAULT_BRANCH:-main}
default_ref="refs/heads/$default_branch"
default_sha=$(git --git-dir="$FAKE_TAP_ORIGIN" rev-parse "$default_ref")
branch_ref="refs/heads/$branch"
case "$method:$endpoint" in
    GET:repos/dcchuck/homebrew-tap)
        printf '%s\n' "$default_branch"
        ;;
    GET:repos/dcchuck/homebrew-tap/contents)
        ;;
    GET:repos/dcchuck/homebrew-tap/branches)
        git --git-dir="$FAKE_TAP_ORIGIN" for-each-ref \
            --format='%(refname:strip=2)' refs/heads
        ;;
    GET:repos/dcchuck/homebrew-tap/git/trees/*)
        if test -n "${FAKE_TREE_TRUNCATED:-}"
        then
            if test "$has_jq" -eq 0
            then
                printf '%s\n' '{"truncated":true,"tree":[]}'
            fi
        elif test "$has_jq" -eq 0
        then
            if test -n "${FAKE_WORKFLOW_PATHS_FILE:-}"
            then
                workflow_path=$(sed -n '1p' "$FAKE_WORKFLOW_PATHS_FILE")
                printf '{"truncated":false,"tree":[{"path":"%s","type":"blob"}]}\n' \
                    "$workflow_path"
            else
                printf '%s\n' '{"truncated":false,"tree":[]}'
            fi
        elif test -n "${FAKE_WORKFLOW_PATHS_FILE:-}"
        then
            cat "$FAKE_WORKFLOW_PATHS_FILE"
        fi
        ;;
    GET:repos/dcchuck/homebrew-tap/git/ref/heads/main)
        ref_calls=0
        if test -n "${FAKE_DEFAULT_REF_CALLS:-}" &&
           test -f "$FAKE_DEFAULT_REF_CALLS"
        then
            ref_calls=$(cat "$FAKE_DEFAULT_REF_CALLS")
        fi
        ref_calls=$((ref_calls + 1))
        if test -n "${FAKE_DEFAULT_REF_CALLS:-}"
        then
            printf '%s\n' "$ref_calls" > "$FAKE_DEFAULT_REF_CALLS"
        fi
        if test "$ref_calls" -ge 2 &&
           test -n "${FAKE_DEFAULT_SHA_SECOND:-}"
        then
            git --git-dir="$FAKE_TAP_ORIGIN" update-ref \
                "$default_ref" "$FAKE_DEFAULT_SHA_SECOND"
            default_sha=$FAKE_DEFAULT_SHA_SECOND
        elif test "$ref_calls" -eq 1 &&
             test -n "${FAKE_DEFAULT_SHA_FIRST:-}"
        then
            default_sha=$FAKE_DEFAULT_SHA_FIRST
        fi
        case "$jq_expression" in
            .object.sha) printf '%s\n' "$default_sha" ;;
            .ref) printf '%s\n' "$default_ref" ;;
            *) printf '{"ref":"%s","object":{"sha":"%s"}}\n' \
                   "$default_ref" "$default_sha" ;;
        esac
        ;;
    GET:repos/dcchuck/homebrew-tap/git/matching-refs/heads/*)
        matching_calls=0
        if test -n "${FAKE_BRANCH_REF_CALLS:-}" &&
           test -f "$FAKE_BRANCH_REF_CALLS"
        then
            matching_calls=$(cat "$FAKE_BRANCH_REF_CALLS")
        fi
        matching_calls=$((matching_calls + 1))
        if test -n "${FAKE_BRANCH_REF_CALLS:-}"
        then
            printf '%s\n' "$matching_calls" > "$FAKE_BRANCH_REF_CALLS"
        fi
        if test "$matching_calls" -ge 2 &&
           test -n "${FAKE_RETARGET_BRANCH_SHA:-}" &&
           git --git-dir="$FAKE_TAP_ORIGIN" show-ref --verify --quiet "$branch_ref"
        then
            git --git-dir="$FAKE_TAP_ORIGIN" update-ref \
                "$branch_ref" "$FAKE_RETARGET_BRANCH_SHA"
        fi
        if git --git-dir="$FAKE_TAP_ORIGIN" show-ref --verify --quiet "$branch_ref"
        then
            branch_sha=$(git --git-dir="$FAKE_TAP_ORIGIN" rev-parse "$branch_ref")
            printf '[{"ref":"refs/heads/%s","object":{"sha":"%s"}}]\n' \
                "$branch" "$branch_sha"
        else
            printf '%s\n' '[]'
        fi
        ;;
    GET:repos/dcchuck/homebrew-tap/git/ref/heads/*)
        git --git-dir="$FAKE_TAP_ORIGIN" show-ref --verify --quiet \
            "$branch_ref"
        branch_sha=$(git --git-dir="$FAKE_TAP_ORIGIN" rev-parse "$branch_ref")
        case "$jq_expression" in
            .object.sha) printf '%s\n' "$branch_sha" ;;
            .ref) printf 'refs/heads/%s\n' "$branch" ;;
            *) printf '{"ref":"refs/heads/%s","object":{"sha":"%s"}}\n' \
                   "$branch" "$branch_sha" ;;
        esac
        ;;
    POST:repos/dcchuck/homebrew-tap/pulls)
        git --git-dir="$FAKE_TAP_ORIGIN" show-ref --verify --quiet \
            "$branch_ref"
        test "$request_head" = "$branch"
        test "$request_base" = "$default_branch"
        test "$request_draft" = true
        git --git-dir="$FAKE_TAP_ORIGIN" diff-tree \
            --no-commit-id --name-only -r "$branch_ref" \
            > "$FAKE_PR_FILES"
        git --git-dir="$FAKE_TAP_ORIGIN" rev-parse "$branch_ref" \
            > "$FAKE_PR_HEAD_SHA_FILE"
        printf '%s\n' open-draft > "$FAKE_PR_STATE"
        if test -n "${FAKE_PR_SIGNAL_AFTER_SUCCESS:-}"
        then
            kill -TERM "$PPID"
            exit 143
        fi
        if test -n "${FAKE_PR_LOST_RESPONSE:-}"
        then
            exit 1
        fi
        printf '%s\n' 73
        ;;
    GET:repos/dcchuck/homebrew-tap/pulls)
        test "$request_head" = "dcchuck:$branch"
        test "$request_base" = "$default_branch"
        test "$request_state" = all
        if test ! -f "$FAKE_PR_STATE"
        then
            printf '%s\n' '[]'
            exit 0
        fi
        pr_state=$(cat "$FAKE_PR_STATE")
        case "$pr_state" in
            open-draft) api_state=open ;;
            closed) api_state=closed ;;
            *) exit 1 ;;
        esac
        branch_sha=${FAKE_PR_HEAD_SHA_OVERRIDE:-$(cat "$FAKE_PR_HEAD_SHA_FILE")}
        printf '[{"number":73,"draft":true,"state":"%s",' "$api_state"
        printf '"head":{"ref":"%s","sha":"%s","repo":{"full_name":"dcchuck/homebrew-tap"}},' \
            "$branch" "$branch_sha"
        if test -n "${FAKE_DUPLICATE_PR:-}"
        then
            printf '"base":{"ref":"%s","repo":{"full_name":"dcchuck/homebrew-tap"}}},' \
                "$default_branch"
            printf '{"number":74,"draft":true,"state":"open",'
            printf '"head":{"ref":"%s","sha":"%s","repo":{"full_name":"dcchuck/homebrew-tap"}},' \
                "$branch" "$branch_sha"
            printf '"base":{"ref":"%s","repo":{"full_name":"dcchuck/homebrew-tap"}}}]\n' \
                "$default_branch"
        else
            printf '"base":{"ref":"%s","repo":{"full_name":"dcchuck/homebrew-tap"}}}]\n' \
                "$default_branch"
        fi
        ;;
    GET:repos/dcchuck/homebrew-tap/pulls/73)
        if test -n "${FAIL_PR_VERIFY:-}"
        then
            exit 1
        fi
        pr_state=$(cat "$FAKE_PR_STATE")
        case "$pr_state" in
            open-draft) api_state=open ;;
            closed) api_state=closed ;;
            *) exit 1 ;;
        esac
        branch_sha=${FAKE_PR_HEAD_SHA_OVERRIDE:-$(cat "$FAKE_PR_HEAD_SHA_FILE")}
        case "$jq_expression" in
            '')
                printf '{"number":73,"draft":true,"state":"%s",' "$api_state"
                printf '"head":{"ref":"%s","sha":"%s","repo":{"full_name":"dcchuck/homebrew-tap"}},' \
                    "$branch" "$branch_sha"
                printf '"base":{"ref":"%s","repo":{"full_name":"dcchuck/homebrew-tap"}}}\n' \
                    "$default_branch"
                ;;
            *)
                printf '73\ttrue\t%s\t%s\t%s\n' \
                    "$api_state" "$branch" "$default_branch"
                ;;
        esac
        ;;
    PATCH:repos/dcchuck/homebrew-tap/pulls/73)
        if test -n "${FAIL_CLOSE_ONCE_FILE:-}" &&
           test ! -e "$FAIL_CLOSE_ONCE_FILE"
        then
            : > "$FAIL_CLOSE_ONCE_FILE"
            exit 1
        fi
        printf '%s\n' closed > "$FAKE_PR_STATE"
        if test -n "${FAKE_FAIL_NEXT_STATE_AFTER_CLOSE_MARKER:-}"
        then
            : > "$FAKE_FAIL_NEXT_STATE_AFTER_CLOSE_MARKER"
        fi
        if test -n "${FAKE_RETARGET_BRANCH_AFTER_CLOSE_SHA:-}"
        then
            git --git-dir="$FAKE_TAP_ORIGIN" update-ref \
                "$branch_ref" "$FAKE_RETARGET_BRANCH_AFTER_CLOSE_SHA"
        fi
        if test -n "${FAKE_REMOVE_BRANCH_AFTER_CLOSE:-}"
        then
            git --git-dir="$FAKE_TAP_ORIGIN" update-ref \
                -d "$branch_ref"
        fi
        ;;
    DELETE:repos/dcchuck/homebrew-tap/git/refs/heads/*)
        git --git-dir="$FAKE_TAP_ORIGIN" update-ref \
            -d "$branch_ref"
        if test -n "${FAKE_FAIL_NEXT_STATE_AFTER_DELETE_MARKER:-}"
        then
            : > "$FAKE_FAIL_NEXT_STATE_AFTER_DELETE_MARKER"
        fi
        ;;
    *)
        echo "unexpected fake gh call: $method $endpoint" >&2
        exit 1
        ;;
esac
EOF
chmod +x "$tap_fake_bin/gh"

run_tap_rehearsal() {
    tap_case=$1
    shift
    env \
        PATH="$tap_fake_bin:$PATH" \
        HOMEBREW_TAP_TOKEN='tap-secret-must-not-appear' \
        EXPECTED_TOKEN='tap-secret-must-not-appear' \
        TAP_REPOSITORY=dcchuck/homebrew-tap \
        GITHUB_RUN_ID=4242 \
        GITHUB_RUN_ATTEMPT=3 \
        GITHUB_WORKSPACE="$repo_root" \
        RUNNER_TEMP="$tap_case/runner-temp" \
        FAKE_TAP_ORIGIN="$tap_case/origin.git" \
        GH_LOG="$tap_case/gh.log" \
        FAKE_PR_FILES="$tap_case/pr-files" \
        FAKE_PR_STATE="$tap_case/pr-state" \
        FAKE_PR_HEAD_SHA_FILE="$tap_case/pr-head-sha" \
        FAKE_DEFAULT_REF_CALLS="$tap_case/default-ref-calls" \
        FAKE_BRANCH_REF_CALLS="$tap_case/branch-ref-calls" \
        FAKE_STATE_CHMOD_COUNT_FILE="$tap_case/state-chmod-count" \
        FAKE_STATE_WRITE_COUNT_FILE="$tap_case/state-write-count" \
        REAL_CHMOD="$real_chmod" \
        REAL_GIT="$real_git" \
        REAL_MV="$real_mv" \
        "$@" \
        "$tap_rehearsal"
}

run_tap_cleanup_hook() {
    tap_case=$1
    shift
    env \
        PATH="$tap_fake_bin:$PATH" \
        HOMEBREW_TAP_TOKEN='tap-secret-must-not-appear' \
        EXPECTED_TOKEN='tap-secret-must-not-appear' \
        GITHUB_RUN_ID=4242 \
        GITHUB_RUN_ATTEMPT=3 \
        GITHUB_WORKSPACE="$repo_root" \
        RUNNER_TEMP="$tap_case/runner-temp" \
        FAKE_TAP_ORIGIN="$tap_case/origin.git" \
        GH_LOG="$tap_case/gh.log" \
        FAKE_PR_FILES="$tap_case/pr-files" \
        FAKE_PR_STATE="$tap_case/pr-state" \
        FAKE_PR_HEAD_SHA_FILE="$tap_case/pr-head-sha" \
        FAKE_DEFAULT_REF_CALLS="$tap_case/default-ref-calls" \
        FAKE_BRANCH_REF_CALLS="$tap_case/branch-ref-calls" \
        FAKE_STATE_CHMOD_COUNT_FILE="$tap_case/state-chmod-count" \
        FAKE_STATE_WRITE_COUNT_FILE="$tap_case/state-write-count" \
        REAL_CHMOD="$real_chmod" \
        REAL_GIT="$real_git" \
        REAL_MV="$real_mv" \
        "$@" \
        "$tap_case/runner-temp/car-go-clean-tap-rehearsal-cleanup.sh"
}

tap_success="$work/tap-success"
mkdir -p "$tap_success/runner-temp"
make_tap_origin "$tap_success"
tap_main_before=$(git --git-dir="$tap_success/origin.git" rev-parse refs/heads/main)
tap_output=$(run_tap_rehearsal "$tap_success" 2>&1)
tap_main_after=$(git --git-dir="$tap_success/origin.git" rev-parse refs/heads/main)
test "$tap_main_before" = "$tap_main_after"
test "$(cat "$tap_success/pr-files")" = '.release-rehearsal/4242.txt'
test "$(cat "$tap_success/pr-state")" = closed
test -z "$(
    git --git-dir="$tap_success/origin.git" for-each-ref \
        --format='%(refname)' refs/heads/rehearsal
)"
test "$(git --git-dir="$tap_success/origin.git" for-each-ref \
    --format='%(refname:strip=2)' refs/heads)" = main
grep -Fq 'api --method GET repos/dcchuck/homebrew-tap' "$tap_success/gh.log"
grep -Fq 'api --method GET repos/dcchuck/homebrew-tap/contents' "$tap_success/gh.log"
grep -Fq 'api --method GET repos/dcchuck/homebrew-tap/branches' "$tap_success/gh.log"
grep -Fq "api --method GET repos/dcchuck/homebrew-tap/git/trees/$tap_main_before?recursive=1" \
    "$tap_success/gh.log"
grep -Fq 'api --method GET repos/dcchuck/homebrew-tap/git/ref/heads/main' \
    "$tap_success/gh.log"
grep -Fq \
    'api --method GET repos/dcchuck/homebrew-tap/git/matching-refs/heads/rehearsal/car-go-clean-4242-3' \
    "$tap_success/gh.log"
grep -Fq 'api --method POST repos/dcchuck/homebrew-tap/pulls' \
    "$tap_success/gh.log"
grep -Fq -- '-F draft=true' "$tap_success/gh.log"
grep -Fq 'api --method PATCH repos/dcchuck/homebrew-tap/pulls/73' \
    "$tap_success/gh.log"
grep -Fq 'api --method DELETE repos/dcchuck/homebrew-tap/git/refs/heads/rehearsal/car-go-clean-4242-3' \
    "$tap_success/gh.log"
if printf '%s\n' "$tap_output" | grep -Fq 'tap-secret-must-not-appear' ||
   grep -Fq 'tap-secret-must-not-appear' "$tap_success/gh.log"
then
    echo "tap rehearsal exposed the token" >&2
    exit 1
fi

tap_existing="$work/tap-existing"
mkdir -p "$tap_existing/runner-temp"
make_tap_origin "$tap_existing"
existing_sha=$(git --git-dir="$tap_existing/origin.git" rev-parse refs/heads/main)
git --git-dir="$tap_existing/origin.git" update-ref \
    refs/heads/rehearsal/car-go-clean-4242-3 "$existing_sha"
existing_output="$tap_existing/output"
if run_tap_rehearsal "$tap_existing" > "$existing_output" 2>&1
then
    echo "unexpected success: existing tap rehearsal branch" >&2
    exit 1
fi
test "$(git --git-dir="$tap_existing/origin.git" rev-parse \
    refs/heads/rehearsal/car-go-clean-4242-3)" = "$existing_sha"
test ! -e "$tap_existing/pr-state"

tap_default="$work/tap-default"
mkdir -p "$tap_default/runner-temp"
make_tap_origin "$tap_default"
default_output="$tap_default/output"
if run_tap_rehearsal \
    "$tap_default" \
    FAKE_DEFAULT_BRANCH=rehearsal/car-go-clean-4242-3 \
    > "$default_output" 2>&1
then
    echo "unexpected success: rehearsal branch equals tap default" >&2
    exit 1
fi
test ! -e "$tap_default/pr-state"

tap_workflows="$work/tap-workflows"
mkdir -p "$tap_workflows/runner-temp"
make_tap_origin "$tap_workflows"
printf '%s\n' .github/workflows/ci.yml > "$tap_workflows/workflows"
workflow_output="$tap_workflows/output"
if run_tap_rehearsal \
    "$tap_workflows" \
    FAKE_WORKFLOW_PATHS_FILE="$tap_workflows/workflows" \
    > "$workflow_output" 2>&1
then
    echo "unexpected success: tap workflow without validated ignore rule" >&2
    exit 1
fi
test -z "$(
    git --git-dir="$tap_workflows/origin.git" for-each-ref \
        --format='%(refname)' refs/heads/rehearsal
)"
test ! -e "$tap_workflows/pr-state"

tap_truncated_tree="$work/tap-truncated-tree"
mkdir -p "$tap_truncated_tree/runner-temp"
make_tap_origin "$tap_truncated_tree"
truncated_output="$tap_truncated_tree/output"
if run_tap_rehearsal \
    "$tap_truncated_tree" \
    FAKE_TREE_TRUNCATED=1 \
    > "$truncated_output" 2>&1
then
    echo "unexpected success: truncated tap tree inventory" >&2
    exit 1
fi
test -z "$(
    git --git-dir="$tap_truncated_tree/origin.git" for-each-ref \
        --format='%(refname)' refs/heads/rehearsal
)"
test ! -e "$tap_truncated_tree/pr-state"

tap_state_generation_failure="$work/tap-state-generation-failure"
mkdir -p "$tap_state_generation_failure/runner-temp"
make_tap_origin "$tap_state_generation_failure"
generation_fifo="$tap_state_generation_failure/runner-temp/car-go-clean-tap-rehearsal-state.tmp"
mkfifo "$generation_fifo"
(
    exec 3< "$generation_fifo"
    exec 3<&-
) &
generation_reader=$!
state_generation_output="$tap_state_generation_failure/output"
generation_status=0
if run_tap_rehearsal \
    "$tap_state_generation_failure" \
    FAKE_PUSH_ATTEMPT_MARKER="$tap_state_generation_failure/push-attempted" \
    > "$state_generation_output" 2>&1
then
    generation_status=0
else
    generation_status=$?
fi
kill "$generation_reader" 2>/dev/null || :
wait "$generation_reader" || :
if test "$generation_status" -eq 0
then
    echo "unexpected success: initial state record generation failure" >&2
    exit 1
fi
test ! -e "$tap_state_generation_failure/push-attempted"
test ! -e "$tap_state_generation_failure/runner-temp/car-go-clean-tap-rehearsal-state"
test ! -e "$tap_state_generation_failure/runner-temp/car-go-clean-tap-rehearsal-state.tmp"
test -z "$(
    git --git-dir="$tap_state_generation_failure/origin.git" for-each-ref \
        --format='%(refname)' refs/heads/rehearsal
)"

tap_state_chmod_failure="$work/tap-state-chmod-failure"
mkdir -p "$tap_state_chmod_failure/runner-temp"
make_tap_origin "$tap_state_chmod_failure"
state_chmod_output="$tap_state_chmod_failure/output"
if run_tap_rehearsal \
    "$tap_state_chmod_failure" \
    FAKE_FAIL_STATE_CHMOD_NUMBER=1 \
    FAKE_PUSH_ATTEMPT_MARKER="$tap_state_chmod_failure/push-attempted" \
    > "$state_chmod_output" 2>&1
then
    echo "unexpected success: initial state chmod failure" >&2
    exit 1
fi
test ! -e "$tap_state_chmod_failure/push-attempted"
test ! -e "$tap_state_chmod_failure/runner-temp/car-go-clean-tap-rehearsal-state"
test ! -e "$tap_state_chmod_failure/runner-temp/car-go-clean-tap-rehearsal-state.tmp"
test -z "$(
    git --git-dir="$tap_state_chmod_failure/origin.git" for-each-ref \
        --format='%(refname)' refs/heads/rehearsal
)"

tap_state_failure="$work/tap-state-failure"
mkdir -p "$tap_state_failure/runner-temp"
make_tap_origin "$tap_state_failure"
state_failure_output="$tap_state_failure/output"
if run_tap_rehearsal \
    "$tap_state_failure" \
    FAKE_FAIL_NEXT_STATE_AFTER_PUSH_MARKER="$tap_state_failure/fail-next-state" \
    > "$state_failure_output" 2>&1
then
    echo "unexpected success: state persistence failure after branch push" >&2
    exit 1
fi
test -z "$(
    git --git-dir="$tap_state_failure/origin.git" for-each-ref \
        --format='%(refname)' refs/heads/rehearsal
)"
test ! -e "$tap_state_failure/pr-state"

tap_verify_failure="$work/tap-verify-failure"
mkdir -p "$tap_verify_failure/runner-temp"
make_tap_origin "$tap_verify_failure"
verify_output="$tap_verify_failure/output"
if run_tap_rehearsal \
    "$tap_verify_failure" \
    FAIL_PR_VERIFY=1 \
    > "$verify_output" 2>&1
then
    echo "unexpected success: PR verification failure" >&2
    exit 1
fi
test "$(cat "$tap_verify_failure/pr-state")" = open-draft
test -n "$(
    git --git-dir="$tap_verify_failure/origin.git" for-each-ref \
        --format='%(refname)' refs/heads/rehearsal
)"
test -f "$tap_verify_failure/runner-temp/car-go-clean-tap-rehearsal-state"
test -x "$tap_verify_failure/runner-temp/car-go-clean-tap-rehearsal-cleanup.sh"
grep -Fq 'Manual inspection required' "$verify_output"
if grep -Eq 'api --method (PATCH|DELETE)' "$tap_verify_failure/gh.log"
then
    echo "unverified rehearsal resources were mutated" >&2
    exit 1
fi
if grep -Fq 'tap-secret-must-not-appear' "$verify_output"
then
    echo "tap failure output exposed the token" >&2
    exit 1
fi

tap_corrupt_branch="$work/tap-corrupt-state-branch"
mkdir -p "$tap_corrupt_branch/runner-temp"
make_tap_origin "$tap_corrupt_branch"
corrupt_setup_output="$tap_corrupt_branch/setup-output"
if run_tap_rehearsal \
    "$tap_corrupt_branch" \
    FAIL_PR_VERIFY=1 \
    > "$corrupt_setup_output" 2>&1
then
    echo "unexpected success: corrupt-state setup rehearsal" >&2
    exit 1
fi
corrupt_state="$tap_corrupt_branch/runner-temp/car-go-clean-tap-rehearsal-state"
cp "$corrupt_state" "$corrupt_state.valid"
corrupt_case=0
for corrupt_branch in \
    "rehearsal/car-go-clean-42:42-3" \
    "rehearsal/car-go-clean-4242-3:3" \
    "rehearsal/car-go-clean-4242-3-4" \
    "rehearsal/car-go-clean--3" \
    "rehearsal/car-go-clean-4242-" \
    "rehearsal/car-go-clean-4242-3';echo-corrupt"
do
    corrupt_case=$((corrupt_case + 1))
    sed "s|^branch=.*|branch=$corrupt_branch|" \
        "$corrupt_state.valid" > "$corrupt_state.corrupt"
    chmod 600 "$corrupt_state.corrupt"
    mv "$corrupt_state.corrupt" "$corrupt_state"
    corrupt_calls_before=$(wc -l < "$tap_corrupt_branch/gh.log" | tr -d ' ')
    corrupt_cleanup_output="$tap_corrupt_branch/cleanup-output-$corrupt_case"
    if run_tap_cleanup_hook \
        "$tap_corrupt_branch" \
        > "$corrupt_cleanup_output" 2>&1
    then
        echo "unexpected success: corrupt loaded rehearsal branch" >&2
        exit 1
    fi
    corrupt_calls_after=$(wc -l < "$tap_corrupt_branch/gh.log" | tr -d ' ')
    test "$corrupt_calls_before" -eq "$corrupt_calls_after"
    grep -Fq 'tap cleanup state contains an invalid rehearsal branch' \
        "$corrupt_cleanup_output"
    test "$(cat "$tap_corrupt_branch/pr-state")" = open-draft
    test -n "$(
        git --git-dir="$tap_corrupt_branch/origin.git" for-each-ref \
            --format='%(refname)' refs/heads/rehearsal
    )"
    test -f "$corrupt_state"
    test -x "$tap_corrupt_branch/runner-temp/car-go-clean-tap-rehearsal-cleanup.sh"
done

tap_pr_head_mismatch="$work/tap-pr-head-mismatch"
mkdir -p "$tap_pr_head_mismatch/runner-temp"
make_tap_origin "$tap_pr_head_mismatch"
wrong_pr_head=$(git --git-dir="$tap_pr_head_mismatch/origin.git" \
    rev-parse refs/heads/main)
pr_head_mismatch_output="$tap_pr_head_mismatch/output"
if run_tap_rehearsal \
    "$tap_pr_head_mismatch" \
    FAKE_PR_HEAD_SHA_OVERRIDE="$wrong_pr_head" \
    > "$pr_head_mismatch_output" 2>&1
then
    echo "unexpected success: draft PR head SHA mismatch" >&2
    exit 1
fi
test "$(cat "$tap_pr_head_mismatch/pr-state")" = open-draft
test -n "$(
    git --git-dir="$tap_pr_head_mismatch/origin.git" for-each-ref \
        --format='%(refname)' refs/heads/rehearsal
)"
test -f "$tap_pr_head_mismatch/runner-temp/car-go-clean-tap-rehearsal-state"
test -x "$tap_pr_head_mismatch/runner-temp/car-go-clean-tap-rehearsal-cleanup.sh"
grep -Fq 'Manual inspection required' "$pr_head_mismatch_output"
if grep -Eq 'api --method (PATCH|DELETE)' "$tap_pr_head_mismatch/gh.log"
then
    echo "mismatched PR authority triggered cleanup mutation" >&2
    exit 1
fi

tap_retry="$work/tap-cleanup-retry"
mkdir -p "$tap_retry/runner-temp"
make_tap_origin "$tap_retry"
retry_output="$tap_retry/output"
if run_tap_rehearsal \
    "$tap_retry" \
    FAIL_CLOSE_ONCE_FILE="$tap_retry/close-failed-once" \
    > "$retry_output" 2>&1
then
    echo "unexpected success: tap cleanup failure" >&2
    exit 1
fi
test "$(cat "$tap_retry/pr-state")" = open-draft
test -n "$(
    git --git-dir="$tap_retry/origin.git" for-each-ref \
        --format='%(refname)' refs/heads/rehearsal
)"
grep -Fq "gh api --method PATCH 'repos/dcchuck/homebrew-tap/pulls/73' -f state=closed" \
    "$retry_output"
test -x "$tap_retry/runner-temp/car-go-clean-tap-rehearsal-cleanup.sh"
run_tap_cleanup_hook \
    "$tap_retry" \
    FAIL_CLOSE_ONCE_FILE="$tap_retry/close-failed-once"
test "$(cat "$tap_retry/pr-state")" = closed
test ! -e "$tap_retry/runner-temp/car-go-clean-tap-rehearsal-cleanup.sh"
test "$(grep -F -c 'api --method DELETE repos/dcchuck/homebrew-tap/git/refs/heads/rehearsal/car-go-clean-4242-3' \
    "$tap_retry/gh.log")" -eq 1
if grep -Fq 'tap-secret-must-not-appear' "$retry_output"
then
    echo "tap cleanup failure exposed the token" >&2
    exit 1
fi

tap_clone_api_mismatch="$work/tap-clone-api-mismatch"
mkdir -p "$tap_clone_api_mismatch/runner-temp"
make_tap_origin "$tap_clone_api_mismatch"
clone_api_output="$tap_clone_api_mismatch/output"
if run_tap_rehearsal \
    "$tap_clone_api_mismatch" \
    FAKE_DEFAULT_SHA_FIRST=ffffffffffffffffffffffffffffffffffffffff \
    > "$clone_api_output" 2>&1
then
    echo "unexpected success: cloned parent differs from GitHub default ref" >&2
    exit 1
fi
test -z "$(
    git --git-dir="$tap_clone_api_mismatch/origin.git" for-each-ref \
        --format='%(refname)' refs/heads/rehearsal
)"
test ! -e "$tap_clone_api_mismatch/pr-state"

tap_default_changed="$work/tap-default-changed-before-pr"
mkdir -p "$tap_default_changed/runner-temp"
make_tap_origin "$tap_default_changed"
default_before=$(git --git-dir="$tap_default_changed/origin.git" rev-parse refs/heads/main)
workflow_sha=$(make_tap_workflow_commit "$tap_default_changed")
default_changed_output="$tap_default_changed/output"
if run_tap_rehearsal \
    "$tap_default_changed" \
    FAKE_DEFAULT_SHA_SECOND="$workflow_sha" \
    > "$default_changed_output" 2>&1
then
    echo "unexpected success: default ref changed before PR creation" >&2
    exit 1
fi
test "$default_before" != "$workflow_sha"
test "$(git --git-dir="$tap_default_changed/origin.git" rev-parse refs/heads/main)" = \
    "$workflow_sha"
test ! -e "$tap_default_changed/pr-state"
test -z "$(
    git --git-dir="$tap_default_changed/origin.git" for-each-ref \
        --format='%(refname)' refs/heads/rehearsal
)"

tap_branch_retarget="$work/tap-branch-retarget"
mkdir -p "$tap_branch_retarget/runner-temp"
make_tap_origin "$tap_branch_retarget"
retarget_sha=$(git --git-dir="$tap_branch_retarget/origin.git" rev-parse refs/heads/main)
retarget_output="$tap_branch_retarget/output"
if run_tap_rehearsal \
    "$tap_branch_retarget" \
    FAKE_RETARGET_BRANCH_SHA="$retarget_sha" \
    > "$retarget_output" 2>&1
then
    echo "unexpected success: rehearsal branch was retargeted before PR" >&2
    exit 1
fi
test "$(git --git-dir="$tap_branch_retarget/origin.git" rev-parse \
    refs/heads/rehearsal/car-go-clean-4242-3)" = "$retarget_sha"
test ! -e "$tap_branch_retarget/pr-state"
test -f "$tap_branch_retarget/runner-temp/car-go-clean-tap-rehearsal-state"
test -x "$tap_branch_retarget/runner-temp/car-go-clean-tap-rehearsal-cleanup.sh"
test "$(file_mode "$tap_branch_retarget/runner-temp/car-go-clean-tap-rehearsal-state")" = 600
test "$(file_mode "$tap_branch_retarget/runner-temp/car-go-clean-tap-rehearsal-cleanup.sh")" = 700
grep -Fq 'Manual inspection required' "$retarget_output"
grep -Fq \
    'repos/dcchuck/homebrew-tap/git/matching-refs/heads/rehearsal/car-go-clean-4242-3' \
    "$retarget_output"
if grep -Fq \
    'api --method DELETE repos/dcchuck/homebrew-tap/git/refs/heads/rehearsal/car-go-clean-4242-3' \
    "$tap_branch_retarget/gh.log"
then
    echo "retargeted rehearsal branch was deleted" >&2
    exit 1
fi

tap_branch_retarget_after_close="$work/tap-branch-retarget-after-close"
mkdir -p "$tap_branch_retarget_after_close/runner-temp"
make_tap_origin "$tap_branch_retarget_after_close"
retarget_after_close_sha=$(git \
    --git-dir="$tap_branch_retarget_after_close/origin.git" \
    rev-parse refs/heads/main)
retarget_after_close_output="$tap_branch_retarget_after_close/output"
if run_tap_rehearsal \
    "$tap_branch_retarget_after_close" \
    FAKE_RETARGET_BRANCH_AFTER_CLOSE_SHA="$retarget_after_close_sha" \
    > "$retarget_after_close_output" 2>&1
then
    echo "unexpected success: rehearsal branch was retargeted after PR close" >&2
    exit 1
fi
test "$(cat "$tap_branch_retarget_after_close/pr-state")" = closed
test "$(git --git-dir="$tap_branch_retarget_after_close/origin.git" rev-parse \
    refs/heads/rehearsal/car-go-clean-4242-3)" = "$retarget_after_close_sha"
test -f "$tap_branch_retarget_after_close/runner-temp/car-go-clean-tap-rehearsal-state"
test -x "$tap_branch_retarget_after_close/runner-temp/car-go-clean-tap-rehearsal-cleanup.sh"
grep -Fq 'Manual inspection required' "$retarget_after_close_output"
if grep -Fq \
    'api --method DELETE repos/dcchuck/homebrew-tap/git/refs/heads/rehearsal/car-go-clean-4242-3' \
    "$tap_branch_retarget_after_close/gh.log"
then
    echo "branch retargeted after PR close was deleted" >&2
    exit 1
fi

tap_branch_absent_after_close="$work/tap-branch-absent-after-close"
mkdir -p "$tap_branch_absent_after_close/runner-temp"
make_tap_origin "$tap_branch_absent_after_close"
absent_after_close_output="$tap_branch_absent_after_close/output"
run_tap_rehearsal \
    "$tap_branch_absent_after_close" \
    FAKE_REMOVE_BRANCH_AFTER_CLOSE=1 \
    > "$absent_after_close_output" 2>&1
test "$(cat "$tap_branch_absent_after_close/pr-state")" = closed
test -z "$(
    git --git-dir="$tap_branch_absent_after_close/origin.git" for-each-ref \
        --format='%(refname)' refs/heads/rehearsal
)"
test ! -e "$tap_branch_absent_after_close/runner-temp/car-go-clean-tap-rehearsal-state"
test ! -e "$tap_branch_absent_after_close/runner-temp/car-go-clean-tap-rehearsal-cleanup.sh"
if grep -Fq \
    'api --method DELETE repos/dcchuck/homebrew-tap/git/refs/heads/rehearsal/car-go-clean-4242-3' \
    "$tap_branch_absent_after_close/gh.log"
then
    echo "already absent rehearsal branch received a DELETE request" >&2
    exit 1
fi

for lost_push_case in lost-response signal-after-success
do
    tap_lost_push="$work/tap-push-$lost_push_case"
    mkdir -p "$tap_lost_push/runner-temp"
    make_tap_origin "$tap_lost_push"
    lost_push_output="$tap_lost_push/output"
    case "$lost_push_case" in
        lost-response)
            lost_push_flag="FAKE_PUSH_LOST_RESPONSE=1"
            ;;
        signal-after-success)
            lost_push_flag="FAKE_PUSH_SIGNAL_AFTER_SUCCESS=1"
            ;;
    esac
    if run_tap_rehearsal \
        "$tap_lost_push" \
        "$lost_push_flag" \
        > "$lost_push_output" 2>&1
    then
        echo "unexpected success: accepted push $lost_push_case" >&2
        exit 1
    fi
    test -z "$(
        git --git-dir="$tap_lost_push/origin.git" for-each-ref \
            --format='%(refname)' refs/heads/rehearsal
    )"
    test ! -e "$tap_lost_push/pr-state"
    test ! -e "$tap_lost_push/runner-temp/car-go-clean-tap-rehearsal-state"
    test ! -e "$tap_lost_push/runner-temp/car-go-clean-tap-rehearsal-cleanup.sh"
done

for lost_pr_case in lost-response signal-after-success
do
    tap_lost_pr="$work/tap-pr-$lost_pr_case"
    mkdir -p "$tap_lost_pr/runner-temp"
    make_tap_origin "$tap_lost_pr"
    lost_pr_output="$tap_lost_pr/output"
    case "$lost_pr_case" in
        lost-response)
            lost_pr_flag="FAKE_PR_LOST_RESPONSE=1"
            ;;
        signal-after-success)
            lost_pr_flag="FAKE_PR_SIGNAL_AFTER_SUCCESS=1"
            ;;
    esac
    if run_tap_rehearsal \
        "$tap_lost_pr" \
        "$lost_pr_flag" \
        > "$lost_pr_output" 2>&1
    then
        echo "unexpected success: accepted PR $lost_pr_case" >&2
        exit 1
    fi
    test "$(cat "$tap_lost_pr/pr-state")" = closed
    test -z "$(
        git --git-dir="$tap_lost_pr/origin.git" for-each-ref \
            --format='%(refname)' refs/heads/rehearsal
    )"
    test ! -e "$tap_lost_pr/runner-temp/car-go-clean-tap-rehearsal-state"
    test ! -e "$tap_lost_pr/runner-temp/car-go-clean-tap-rehearsal-cleanup.sh"
done

tap_ambiguous_pr="$work/tap-ambiguous-pr"
mkdir -p "$tap_ambiguous_pr/runner-temp"
make_tap_origin "$tap_ambiguous_pr"
ambiguous_pr_output="$tap_ambiguous_pr/output"
if run_tap_rehearsal \
    "$tap_ambiguous_pr" \
    FAKE_PR_LOST_RESPONSE=1 \
    FAKE_DUPLICATE_PR=1 \
    > "$ambiguous_pr_output" 2>&1
then
    echo "unexpected success: ambiguous matching rehearsal PRs" >&2
    exit 1
fi
test "$(cat "$tap_ambiguous_pr/pr-state")" = open-draft
test -n "$(
    git --git-dir="$tap_ambiguous_pr/origin.git" for-each-ref \
        --format='%(refname)' refs/heads/rehearsal
)"
test -f "$tap_ambiguous_pr/runner-temp/car-go-clean-tap-rehearsal-state"
test -x "$tap_ambiguous_pr/runner-temp/car-go-clean-tap-rehearsal-cleanup.sh"
test "$(file_mode "$tap_ambiguous_pr/runner-temp/car-go-clean-tap-rehearsal-state")" = 600
test "$(file_mode "$tap_ambiguous_pr/runner-temp/car-go-clean-tap-rehearsal-cleanup.sh")" = 700
grep -Fq 'Manual inspection required' "$ambiguous_pr_output"
if grep -Eq 'api --method (PATCH|DELETE)' "$tap_ambiguous_pr/gh.log"
then
    echo "ambiguous rehearsal resources were mutated" >&2
    exit 1
fi

tap_close_checkpoint="$work/tap-close-checkpoint"
mkdir -p "$tap_close_checkpoint/runner-temp"
make_tap_origin "$tap_close_checkpoint"
close_checkpoint_output="$tap_close_checkpoint/output"
if run_tap_rehearsal \
    "$tap_close_checkpoint" \
    FAKE_FAIL_NEXT_STATE_AFTER_CLOSE_MARKER="$tap_close_checkpoint/fail-next-state" \
    > "$close_checkpoint_output" 2>&1
then
    echo "unexpected success: state checkpoint failed after closing PR" >&2
    exit 1
fi
test "$(cat "$tap_close_checkpoint/pr-state")" = closed
test -n "$(
    git --git-dir="$tap_close_checkpoint/origin.git" for-each-ref \
        --format='%(refname)' refs/heads/rehearsal
)"
test -f "$tap_close_checkpoint/runner-temp/car-go-clean-tap-rehearsal-state"
test -x "$tap_close_checkpoint/runner-temp/car-go-clean-tap-rehearsal-cleanup.sh"
test "$(file_mode "$tap_close_checkpoint/runner-temp/car-go-clean-tap-rehearsal-state")" = 600
test "$(file_mode "$tap_close_checkpoint/runner-temp/car-go-clean-tap-rehearsal-cleanup.sh")" = 700
run_tap_cleanup_hook "$tap_close_checkpoint"
test ! -e "$tap_close_checkpoint/runner-temp/car-go-clean-tap-rehearsal-state"
test ! -e "$tap_close_checkpoint/runner-temp/car-go-clean-tap-rehearsal-cleanup.sh"
test -z "$(
    git --git-dir="$tap_close_checkpoint/origin.git" for-each-ref \
        --format='%(refname)' refs/heads/rehearsal
)"
test "$(grep -F -c 'api --method PATCH repos/dcchuck/homebrew-tap/pulls/73' \
    "$tap_close_checkpoint/gh.log")" -eq 1
test "$(grep -F -c 'api --method DELETE repos/dcchuck/homebrew-tap/git/refs/heads/rehearsal/car-go-clean-4242-3' \
    "$tap_close_checkpoint/gh.log")" -eq 1

tap_close_chmod_checkpoint="$work/tap-close-chmod-checkpoint"
mkdir -p "$tap_close_chmod_checkpoint/runner-temp"
make_tap_origin "$tap_close_chmod_checkpoint"
close_chmod_output="$tap_close_chmod_checkpoint/output"
if run_tap_rehearsal \
    "$tap_close_chmod_checkpoint" \
    FAKE_FAIL_STATE_CHMOD_NUMBER=5 \
    > "$close_chmod_output" 2>&1
then
    echo "unexpected success: state chmod checkpoint failed after closing PR" >&2
    exit 1
fi
test "$(cat "$tap_close_chmod_checkpoint/pr-state")" = closed
test -n "$(
    git --git-dir="$tap_close_chmod_checkpoint/origin.git" for-each-ref \
        --format='%(refname)' refs/heads/rehearsal
)"
test -f "$tap_close_chmod_checkpoint/runner-temp/car-go-clean-tap-rehearsal-state"
test ! -e "$tap_close_chmod_checkpoint/runner-temp/car-go-clean-tap-rehearsal-state.tmp"
test -x "$tap_close_chmod_checkpoint/runner-temp/car-go-clean-tap-rehearsal-cleanup.sh"
test "$(file_mode "$tap_close_chmod_checkpoint/runner-temp/car-go-clean-tap-rehearsal-state")" = 600
if grep -Fq \
    'api --method DELETE repos/dcchuck/homebrew-tap/git/refs/heads/rehearsal/car-go-clean-4242-3' \
    "$tap_close_chmod_checkpoint/gh.log"
then
    echo "failed PR-close checkpoint allowed branch deletion" >&2
    exit 1
fi
run_tap_cleanup_hook \
    "$tap_close_chmod_checkpoint" \
    FAKE_FAIL_STATE_CHMOD_NUMBER=5
test ! -e "$tap_close_chmod_checkpoint/runner-temp/car-go-clean-tap-rehearsal-state"
test ! -e "$tap_close_chmod_checkpoint/runner-temp/car-go-clean-tap-rehearsal-cleanup.sh"
test -z "$(
    git --git-dir="$tap_close_chmod_checkpoint/origin.git" for-each-ref \
        --format='%(refname)' refs/heads/rehearsal
)"
test "$(grep -F -c 'api --method PATCH repos/dcchuck/homebrew-tap/pulls/73' \
    "$tap_close_chmod_checkpoint/gh.log")" -eq 1
test "$(grep -F -c 'api --method DELETE repos/dcchuck/homebrew-tap/git/refs/heads/rehearsal/car-go-clean-4242-3' \
    "$tap_close_chmod_checkpoint/gh.log")" -eq 1

tap_delete_checkpoint="$work/tap-delete-checkpoint"
mkdir -p "$tap_delete_checkpoint/runner-temp"
make_tap_origin "$tap_delete_checkpoint"
delete_checkpoint_output="$tap_delete_checkpoint/output"
if run_tap_rehearsal \
    "$tap_delete_checkpoint" \
    FAKE_FAIL_NEXT_STATE_AFTER_DELETE_MARKER="$tap_delete_checkpoint/fail-next-state" \
    > "$delete_checkpoint_output" 2>&1
then
    echo "unexpected success: state checkpoint failed after deleting branch" >&2
    exit 1
fi
test "$(cat "$tap_delete_checkpoint/pr-state")" = closed
test -z "$(
    git --git-dir="$tap_delete_checkpoint/origin.git" for-each-ref \
        --format='%(refname)' refs/heads/rehearsal
)"
test -f "$tap_delete_checkpoint/runner-temp/car-go-clean-tap-rehearsal-state"
test -x "$tap_delete_checkpoint/runner-temp/car-go-clean-tap-rehearsal-cleanup.sh"
test "$(file_mode "$tap_delete_checkpoint/runner-temp/car-go-clean-tap-rehearsal-state")" = 600
test "$(file_mode "$tap_delete_checkpoint/runner-temp/car-go-clean-tap-rehearsal-cleanup.sh")" = 700
run_tap_cleanup_hook "$tap_delete_checkpoint"
test ! -e "$tap_delete_checkpoint/runner-temp/car-go-clean-tap-rehearsal-state"
test ! -e "$tap_delete_checkpoint/runner-temp/car-go-clean-tap-rehearsal-cleanup.sh"
test "$(grep -F -c 'api --method PATCH repos/dcchuck/homebrew-tap/pulls/73' \
    "$tap_delete_checkpoint/gh.log")" -eq 1
test "$(grep -F -c 'api --method DELETE repos/dcchuck/homebrew-tap/git/refs/heads/rehearsal/car-go-clean-4242-3' \
    "$tap_delete_checkpoint/gh.log")" -eq 1

draft_fake_bin="$work/draft-fake-bin"
mkdir -p "$draft_fake_bin"
cat > "$draft_fake_bin/gh" <<'EOF'
#!/bin/sh
set -eu

state=${FAKE_GH_STATE:?}
log="$state/gh.log"
{
    printf 'CALL'
    for argument in "$@"
    do
        printf '\t%s' "$argument"
    done
    printf '\n'
} >> "$log"

if test "$1" = api
then
    shift
    method=GET
    endpoint=
    input=
    while test "$#" -gt 0
    do
        case "$1" in
            --method) method=$2; shift 2 ;;
            --input) input=$2; shift 2 ;;
            --jq|-f|-F|--field|--raw-field) shift 2 ;;
            graphql)
                endpoint=graphql
                shift
                ;;
            *)
                if test -z "$endpoint"
                then
                    endpoint=$1
                fi
                shift
                ;;
        esac
    done

    case "$endpoint" in
        graphql)
            if test -n "${FAKE_DISCOVERY_AUTH_FAILURE:-}"
            then
                echo 'gh: authentication failed (HTTP 401)' >&2
                exit 1
            fi
            if test -n "${FAKE_DISCOVERY_QUERY_FAILURE:-}"
            then
                printf '%s\n' '{"errors":[{"message":"query failed"}]}'
                exit 0
            fi
            if test -n "${FAKE_DISCOVERY_MISSING_RELEASE:-}"
            then
                printf '%s\n' '{"data":{"repository":{}}}'
                exit 0
            fi
            if test ! -f "$state/release.json"
            then
                printf '%s\n' '{"data":{"repository":{"release":null}}}'
            else
                jq '{
                    data: {
                        repository: {
                            release: {
                                databaseId: .databaseId,
                                isDraft: .isDraft
                            }
                        }
                    }
                }' "$state/release.json"
            fi
            ;;
        repos/dcchuck/car-go-clean)
            printf '%s\n' dcchuck/car-go-clean
            ;;
        repos/dcchuck/car-go-clean/git/ref/tags/v0.4.0)
            count=0
            if test -f "$state/tag-ref-count"
            then
                count=$(cat "$state/tag-ref-count")
            fi
            count=$((count + 1))
            printf '%s\n' "$count" > "$state/tag-ref-count"
            if test "${FAKE_DELETE_TAG_ON_REF_CALL:-0}" -eq "$count"
            then
                echo 'gh: Not Found (HTTP 404)' >&2
                exit 1
            fi
            if test "${FAKE_REPLACE_TAG_ON_REF_CALL:-0}" -eq "$count"
            then
                printf '%s\t%s\n' tag bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb
            else
                printf '%s\t%s\n' tag aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa
            fi
            ;;
        repos/dcchuck/car-go-clean/git/tags/aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa)
            printf '%s\t%s\n' \
                commit \
                0123456789abcdef0123456789abcdef01234567
            ;;
        repos/dcchuck/car-go-clean/git/tags/bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb)
            printf '%s\t%s\n' \
                commit \
                0123456789abcdef0123456789abcdef01234567
            ;;
        repos/dcchuck/car-go-clean/releases/tags/v0.4.0)
            # GitHub's REST-by-tag endpoint is published-only. A draft must be
            # found through GraphQL and fetched by numeric database ID.
            if test ! -f "$state/release.json" ||
               test "$(jq -r '.isDraft' "$state/release.json")" = true
            then
                echo 'gh: Not Found (HTTP 404)' >&2
                exit 1
            fi
            cat "$state/release.json"
            ;;
        repos/dcchuck/car-go-clean/releases/assets/*)
            test "$method" = DELETE
            asset_id=${endpoint##*/}
            if test -n "${FAKE_REPLACE_ASSET_BEFORE_DELETE:-}" &&
               test ! -f "$state/asset-replaced"
            then
                tmp="$state/release.json.tmp"
                jq \
                    --argjson old_id "$asset_id" \
                    '.assets |= map(
                        if .id == $old_id then .id = 999 else . end
                    )' \
                    "$state/release.json" > "$tmp"
                mv "$tmp" "$state/release.json"
                : > "$state/asset-replaced"
                echo 'gh: Not Found (HTTP 404)' >&2
                exit 1
            fi
            test "$(
                jq --argjson id "$asset_id" \
                    '[.assets[] | select(.id == $id)] | length' \
                    "$state/release.json"
            )" -eq 1 || {
                echo 'gh: Not Found (HTTP 404)' >&2
                exit 1
            }
            tmp="$state/release.json.tmp"
            jq --argjson id "$asset_id" \
                '.assets |= map(select(.id != $id))' \
                "$state/release.json" > "$tmp"
            mv "$tmp" "$state/release.json"
            ;;
        repos/dcchuck/car-go-clean/releases/*)
            release_id=${endpoint##*/}
            test -f "$state/release.json" || {
                echo 'gh: Not Found (HTTP 404)' >&2
                exit 1
            }
            test "$(jq -r '.databaseId' "$state/release.json")" = "$release_id" || {
                echo 'gh: Not Found (HTTP 404)' >&2
                exit 1
            }
            case "$method" in
                GET)
                    cat "$state/release.json"
                    ;;
                PATCH)
                    test -f "$input"
                    tmp="$state/release.json.tmp"
                    jq \
                        --slurpfile patch "$input" \
                        '.tagName = $patch[0].tag_name |
                         .targetCommitish = $patch[0].target_commitish |
                         .name = $patch[0].name |
                         .body = $patch[0].body |
                         .isDraft = $patch[0].draft' \
                        "$state/release.json" > "$tmp"
                    mv "$tmp" "$state/release.json"
                    if test -n "${FAKE_REPLACE_RELEASE_AFTER_PATCH:-}"
                    then
                        tmp="$state/release.json.tmp"
                        jq '.databaseId = 74' "$state/release.json" > "$tmp"
                        mv "$tmp" "$state/release.json"
                    fi
                    cat "$state/release.json"
                    ;;
                *)
                    echo "unexpected release API method: $method" >&2
                    exit 2
                    ;;
            esac
            ;;
        repos/dcchuck/car-go-clean/commits/*)
            target=${endpoint##*/}
            printf '%s\n' "${FAKE_RESOLVED_SHA:-$target}"
            ;;
        *)
            echo "unexpected gh api path: $endpoint" >&2
            exit 2
            ;;
    esac
    exit 0
fi

test "$1" = release
command=$2
case "$command" in
    view)
        echo 'release lookup must use GraphQL and numeric release IDs' >&2
        exit 2
        ;;
    create)
        tag=$3
        shift 3
        target=
        title=
        notes_file=
        draft=false
        verify_tag=false
        while test "$#" -gt 0
        do
            case "$1" in
                --target) target=$2; shift 2 ;;
                --title) title=$2; shift 2 ;;
                --notes-file) notes_file=$2; shift 2 ;;
                --repo) shift 2 ;;
                --draft) draft=true; shift ;;
                --verify-tag) verify_tag=true; shift ;;
                *) echo "unexpected release create argument: $1" >&2; exit 2 ;;
            esac
        done
        test "$draft" = true
        test "$verify_tag" = true
        test -f "$notes_file"
        test ! -f "$state/release.json"
        if test -n "${FAKE_DELETE_TAG_BEFORE_CREATE:-}"
        then
            echo 'tag v0.4.0 not found' >&2
            exit 1
        fi
        jq -n \
            --arg tag "$tag" \
            --arg target "$target" \
            --arg title "$title" \
            --rawfile body "$notes_file" \
            '{
                databaseId: 73,
                tagName: $tag,
                isDraft: true,
                isPrerelease: false,
                targetCommitish: $target,
                name: $title,
                body: $body,
                assets: []
            }' > "$state/release.json"
        ;;
    edit)
        echo 'release edits must use the verified numeric release ID' >&2
        exit 2
        ;;
    delete-asset)
        echo 'asset deletion must use the verified numeric asset ID' >&2
        exit 2
        ;;
    upload)
        test -f "$state/release.json"
        tag=$3
        shift 3
        test "$(jq -r '.tagName' "$state/release.json")" = "$tag"
        while test "$#" -gt 0
        do
            case "$1" in
                --repo) shift 2; continue ;;
                --clobber)
                    echo 'unguarded --clobber is forbidden' >&2
                    exit 2
                    ;;
            esac
            test -f "$1"
            count=0
            if test -f "$state/upload-count"
            then
                count=$(cat "$state/upload-count")
            fi
            count=$((count + 1))
            printf '%s\n' "$count" > "$state/upload-count"
            if test -n "${FAKE_UPLOAD_FAIL_AFTER:-}" &&
               test "$count" -gt "$FAKE_UPLOAD_FAIL_AFTER"
            then
                exit 1
            fi
            name=$(basename "$1")
            tmp="$state/release.json.tmp"
            jq \
                --arg name "$name" \
                --argjson id "$count" \
                '.assets += [{name: $name, id: $id}]' \
                "$state/release.json" > "$tmp"
            mv "$tmp" "$state/release.json"
            shift
        done
        ;;
    *)
        echo "unexpected gh release command: $command" >&2
        exit 2
        ;;
esac
EOF
chmod +x "$draft_fake_bin/gh"

draft_notes="$work/draft-notes.md"
printf '%s\n' '# reviewed release notes' > "$draft_notes"
draft_sha=0123456789abcdef0123456789abcdef01234567
draft_assets="$work/draft-assets"
mkdir -p "$draft_assets"
for archive in $archives
do
    printf 'draft archive fixture: %s\n' "$archive" > "$draft_assets/$archive"
    printf '%s  %s\n' \
        "$(hash_file "$draft_assets/$archive")" \
        "$archive" \
        > "$draft_assets/$archive.sha256"
done
printf '%s\n' 'class CarGoClean < Formula' > "$draft_assets/car-go-clean.rb"
printf '%s\n' 'aggregate checksums' > "$draft_assets/sha256.sum"
printf '%s\n' 'source archive fixture' > "$draft_assets/source.tar.gz"
printf '%s  %s\n' \
    "$(hash_file "$draft_assets/source.tar.gz")" \
    source.tar.gz \
    > "$draft_assets/source.tar.gz.sha256"

draft_expected_assets='
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
draft_expected_json=$(
    printf '%s\n' "$draft_expected_assets" |
        sed '/^$/d' |
        jq -Rsc 'split("\n") | map(select(length > 0))'
)
draft_plan_manifest="$work/draft-plan-dist-manifest.json"
jq -n \
    --arg tag v0.4.0 \
    --argjson names "$draft_expected_json" \
    '{
        dist_version: "0.32.0",
        announcement_tag: $tag,
        artifacts: ($names | map({key: ., value: {name: .}}) | from_entries),
        upload_files: []
    }' > "$draft_plan_manifest"
draft_global_manifest="$work/draft-global-dist-manifest.json"
jq -n \
    '[
        "car-go-clean.rb",
        "sha256.sum",
        "source.tar.gz",
        "source.tar.gz.sha256"
    ]' > "$work/draft-global-assets.json"
jq -n \
    --arg tag v0.4.0 \
    --slurpfile global_names "$work/draft-global-assets.json" \
    '{
        dist_version: "0.32.0",
        announcement_tag: $tag,
        artifacts: ($global_names[0] |
            map({key: ., value: {name: .}}) | from_entries),
        upload_files: ($global_names[0] | map("/target/distrib/" + .))
    }' > "$draft_global_manifest"

run_draft_upsert() {
    draft_state=$1
    shift
    env \
        PATH="$draft_fake_bin:$PATH" \
        FAKE_GH_STATE="$draft_state" \
        GITHUB_REPOSITORY=dcchuck/car-go-clean \
        CARGO_DIST_PLAN_MANIFEST="$draft_plan_manifest" \
        CARGO_DIST_GLOBAL_MANIFEST="$draft_global_manifest" \
        "$@" \
        "$draft_upserter" \
        v0.4.0 \
        "$draft_sha" \
        'car-go-clean v0.4.0' \
        "$draft_notes" \
        "$draft_assets"
}

assert_draft_inventory() {
    state=$1
    test "$(jq -r '.tagName' "$state/release.json")" = v0.4.0
    test "$(jq -r '.isDraft' "$state/release.json")" = true
    test "$(jq -r '.targetCommitish' "$state/release.json")" = "$draft_sha"
    test "$(jq -r '.name' "$state/release.json")" = 'car-go-clean v0.4.0'
    test "$(jq '[.assets[].name] | unique | length' "$state/release.json")" -eq 12
    test "$(jq '[.assets[].name] | length' "$state/release.json")" -eq 12
    for asset in $draft_expected_assets
    do
        test "$(jq --arg name "$asset" \
            '[.assets[] | select(.name == $name)] | length' \
            "$state/release.json")" -eq 1
    done
}

draft_absent="$work/draft-absent"
mkdir -p "$draft_absent"
if ! run_draft_upsert "$draft_absent" > "$draft_absent/output" 2>&1
then
    cat "$draft_absent/output" >&2
    sed -n '1,80p' "$draft_absent/gh.log" >&2
    exit 1
fi
assert_draft_inventory "$draft_absent"
test "$(grep -c "$(printf 'CALL\trelease\tcreate\tv0.4.0')" \
    "$draft_absent/gh.log")" -eq 1
grep -Fq "$(printf '\t--verify-tag')" "$draft_absent/gh.log"
test "$(grep -c "$(printf 'CALL\tapi\trepos/dcchuck/car-go-clean\t--jq\t.full_name')" \
    "$draft_absent/gh.log")" -eq 1
grep -Fq "$(printf 'CALL\tapi\tgraphql')" "$draft_absent/gh.log"
grep -Fq "$(printf 'CALL\tapi\trepos/dcchuck/car-go-clean/releases/73')" \
    "$draft_absent/gh.log"
if grep -Fq 'releases/tags/v0.4.0' "$draft_absent/gh.log"
then
    echo "draft discovery incorrectly used the published-only REST endpoint" >&2
    exit 1
fi

draft_outage="$work/draft-outage"
mkdir -p "$draft_outage"
expect_failure "release lookup authentication outage" \
    run_draft_upsert "$draft_outage" FAKE_DISCOVERY_AUTH_FAILURE=1
test ! -e "$draft_outage/release.json"
if grep -Fq "$(printf 'CALL\trelease\tcreate')" "$draft_outage/gh.log"
then
    echo "draft upsert treated a non-404 lookup failure as absence" >&2
    exit 1
fi
draft_query_failure="$work/draft-query-failure"
mkdir -p "$draft_query_failure"
expect_failure "release GraphQL query failure" \
    run_draft_upsert "$draft_query_failure" FAKE_DISCOVERY_QUERY_FAILURE=1
test ! -e "$draft_query_failure/release.json"
draft_schema_failure="$work/draft-schema-failure"
mkdir -p "$draft_schema_failure"
expect_failure "release GraphQL response without explicit release null" \
    run_draft_upsert "$draft_schema_failure" FAKE_DISCOVERY_MISSING_RELEASE=1
test ! -e "$draft_schema_failure/release.json"

for bad_manifest in plan global
do
    bad_state="$work/draft-bad-$bad_manifest"
    mkdir -p "$bad_state"
    bad_file="$bad_state/$bad_manifest.json"
    case "$bad_manifest" in
        plan)
            jq 'del(.artifacts["source.tar.gz"])' \
                "$draft_plan_manifest" > "$bad_file"
            bad_env="CARGO_DIST_PLAN_MANIFEST=$bad_file"
            ;;
        global)
            jq '.upload_files += ["/target/distrib/unexpected.zip"]' \
                "$draft_global_manifest" > "$bad_file"
            bad_env="CARGO_DIST_GLOBAL_MANIFEST=$bad_file"
            ;;
    esac
    expect_failure "invalid $bad_manifest cargo-dist inventory" \
        run_draft_upsert "$bad_state" "$bad_env"
    test ! -s "$bad_state/gh.log"
done

printf '%s\n' unexpected > "$draft_assets/unexpected.zip"
draft_extra="$work/draft-extra-direct-artifact"
mkdir -p "$draft_extra"
expect_failure "unexpected direct release artifact" \
    run_draft_upsert "$draft_extra"
test ! -s "$draft_extra/gh.log"
rm "$draft_assets/unexpected.zip"

draft_matching="$work/draft-matching"
mkdir -p "$draft_matching"
jq -n \
    --arg sha "$draft_sha" \
    '{
        databaseId: 73,
        tagName: "v0.4.0",
        isDraft: true,
        isPrerelease: false,
        targetCommitish: $sha,
        name: "old title",
        assets: [
            {name: "keep-me.txt", id: 9},
            {name: "car-go-clean-aarch64-apple-darwin.tar.xz", id: 10}
        ]
    }' > "$draft_matching/release.json"
run_draft_upsert "$draft_matching"
test "$(jq '[.assets[] | select(.name == "keep-me.txt")] | length' \
    "$draft_matching/release.json")" -eq 1
test "$(jq '[.assets[].name] | unique | length' \
    "$draft_matching/release.json")" -eq 13
test "$(jq '[.assets[].name] | length' \
    "$draft_matching/release.json")" -eq 13
test "$(grep -c "$(printf 'CALL\tapi\t--method\tDELETE\trepos/dcchuck/car-go-clean/releases/assets/10')" \
    "$draft_matching/gh.log")" -eq 1
if grep -Fq 'releases/assets/9' \
    "$draft_matching/gh.log"
then
    echo "draft upsert deleted an unexpected release asset" >&2
    exit 1
fi

draft_resolved_target="$work/draft-resolved-target"
mkdir -p "$draft_resolved_target"
jq -n \
    '{
        databaseId: 73,
        tagName: "v0.4.0",
        isDraft: true,
        isPrerelease: false,
        targetCommitish: "main",
        name: "branch target",
        assets: []
    }' > "$draft_resolved_target/release.json"
run_draft_upsert "$draft_resolved_target" FAKE_RESOLVED_SHA="$draft_sha"
assert_draft_inventory "$draft_resolved_target"
test "$(grep -c "$(printf 'CALL\tapi\trepos/dcchuck/car-go-clean/commits/main')" \
    "$draft_resolved_target/gh.log")" -eq 1

for rejected_state in published mismatched
do
    rejected="$work/draft-$rejected_state"
    mkdir -p "$rejected"
    is_draft=true
    target=$draft_sha
    case "$rejected_state" in
        published) is_draft=false ;;
        mismatched) target=ffffffffffffffffffffffffffffffffffffffff ;;
    esac
    jq -n \
        --argjson draft "$is_draft" \
        --arg target "$target" \
        '{
            databaseId: 73,
            tagName: "v0.4.0",
            isDraft: $draft,
            isPrerelease: false,
            targetCommitish: $target,
            name: "existing",
            assets: []
        }' > "$rejected/release.json"
    expect_failure "$rejected_state release" run_draft_upsert "$rejected"
    if grep -Eq "$(printf 'CALL\trelease\t(upload|create)|CALL\tapi\t--method\t(PATCH|DELETE)')" \
        "$rejected/gh.log"
    then
        echo "$rejected_state release was mutated" >&2
        exit 1
    fi
done

draft_partial="$work/draft-partial"
mkdir -p "$draft_partial"
expect_failure "partial draft asset upload" \
    run_draft_upsert "$draft_partial" FAKE_UPLOAD_FAIL_AFTER=3
test "$(jq '.assets | length' "$draft_partial/release.json")" -eq 3
rm "$draft_partial/upload-count"
run_draft_upsert "$draft_partial"
assert_draft_inventory "$draft_partial"

draft_deleted_before_create="$work/draft-deleted-before-create"
mkdir -p "$draft_deleted_before_create"
expect_failure "tag deleted before verified release creation" \
    run_draft_upsert "$draft_deleted_before_create" FAKE_DELETE_TAG_BEFORE_CREATE=1
test ! -e "$draft_deleted_before_create/release.json"
if grep -Fq "$(printf 'CALL\trelease\tupload')" \
    "$draft_deleted_before_create/gh.log"
then
    echo "assets uploaded after tag deletion" >&2
    exit 1
fi

for tag_race in deleted-after-edit replaced-after-edit replaced-at-final-boundary
do
    race="$work/draft-$tag_race"
    mkdir -p "$race"
    cp "$draft_matching/release.json" "$race/release.json"
    case "$tag_race" in
        deleted-after-edit) race_env=FAKE_DELETE_TAG_ON_REF_CALL=2 ;;
        replaced-after-edit) race_env=FAKE_REPLACE_TAG_ON_REF_CALL=2 ;;
        replaced-at-final-boundary) race_env=FAKE_REPLACE_TAG_ON_REF_CALL=3 ;;
    esac
    expect_failure "$tag_race" \
        run_draft_upsert "$race" "$race_env"
    if grep -Eq "$(printf 'CALL\trelease\tupload|CALL\tapi\t--method\tDELETE')" \
        "$race/gh.log"
    then
        echo "assets mutated after a tag replacement race" >&2
        exit 1
    fi
done

draft_release_replaced="$work/draft-release-replaced"
mkdir -p "$draft_release_replaced"
cp "$draft_matching/release.json" "$draft_release_replaced/release.json"
expect_failure "release replaced after metadata patch" \
    run_draft_upsert "$draft_release_replaced" FAKE_REPLACE_RELEASE_AFTER_PATCH=1
if grep -Eq "$(printf 'CALL\trelease\tupload|CALL\tapi\t--method\tDELETE')" \
    "$draft_release_replaced/gh.log"
then
    echo "assets mutated after release identity changed" >&2
    exit 1
fi

draft_asset_replaced="$work/draft-asset-replaced"
mkdir -p "$draft_asset_replaced"
cp "$draft_matching/release.json" "$draft_asset_replaced/release.json"
expect_failure "asset replaced before ID-addressed deletion" \
    run_draft_upsert "$draft_asset_replaced" FAKE_REPLACE_ASSET_BEFORE_DELETE=1
test "$(jq '[.assets[] | select(.id == 999)] | length' \
    "$draft_asset_replaced/release.json")" -eq 1
if grep -Fq "$(printf 'CALL\trelease\tupload')" \
    "$draft_asset_replaced/gh.log"
then
    echo "assets uploaded after ID-addressed deletion lost its race" >&2
    exit 1
fi
