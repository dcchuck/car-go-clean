#!/bin/sh
set -eu

repo_root=$(CDPATH='' cd -- "$(dirname -- "$0")/.." && pwd)
validator="$repo_root/scripts/validate-release-inputs.sh"
dist_installer="$repo_root/scripts/install-cargo-dist.sh"
asset_verifier="$repo_root/scripts/verify-release-assets.sh"
formula_renderer="$repo_root/scripts/render-homebrew-formula.sh"
tap_rehearsal="$repo_root/scripts/rehearse-tap-capability.sh"
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
    "$tap_rehearsal"
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

tap_fake_bin="$work/tap-fake-bin"
mkdir -p "$tap_fake_bin"
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
            shift 2
            ;;
        -f|-F)
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
case "$method:$endpoint" in
    GET:repos/dcchuck/homebrew-tap)
        printf '%s\n' "${FAKE_DEFAULT_BRANCH:-main}"
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
    GET:repos/dcchuck/homebrew-tap/git/ref/heads/*)
        git --git-dir="$FAKE_TAP_ORIGIN" show-ref --verify --quiet \
            "refs/heads/$branch"
        printf 'refs/heads/%s\n' "$branch"
        ;;
    POST:repos/dcchuck/homebrew-tap/pulls)
        git --git-dir="$FAKE_TAP_ORIGIN" show-ref --verify --quiet \
            "refs/heads/$branch"
        git --git-dir="$FAKE_TAP_ORIGIN" diff-tree \
            --no-commit-id --name-only -r "refs/heads/$branch" \
            > "$FAKE_PR_FILES"
        printf '%s\n' open-draft > "$FAKE_PR_STATE"
        printf '%s\n' 73
        ;;
    GET:repos/dcchuck/homebrew-tap/pulls/73)
        if test -n "${FAIL_PR_VERIFY:-}"
        then
            exit 1
        fi
        printf '73\ttrue\topen\t%s\t%s\n' \
            "$branch" "${FAKE_DEFAULT_BRANCH:-main}"
        ;;
    PATCH:repos/dcchuck/homebrew-tap/pulls/73)
        if test -n "${FAIL_CLOSE_ONCE_FILE:-}" &&
           test ! -e "$FAIL_CLOSE_ONCE_FILE"
        then
            : > "$FAIL_CLOSE_ONCE_FILE"
            exit 1
        fi
        printf '%s\n' closed > "$FAKE_PR_STATE"
        ;;
    DELETE:repos/dcchuck/homebrew-tap/git/refs/heads/*)
        if test -n "${FAIL_STATE_DIR:-}"
        then
            chmod 700 "$FAIL_STATE_DIR"
        fi
        git --git-dir="$FAKE_TAP_ORIGIN" update-ref \
            -d "refs/heads/$branch"
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
        "$@" \
        "$tap_rehearsal"
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
grep -Fq 'api --method GET repos/dcchuck/homebrew-tap/git/trees/main?recursive=1' \
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

tap_state_failure="$work/tap-state-failure"
mkdir -p "$tap_state_failure/runner-temp"
make_tap_origin "$tap_state_failure"
cat > "$tap_state_failure/origin.git/hooks/post-receive" <<'EOF'
#!/bin/sh
set -eu
if test -n "${FAIL_STATE_DIR:-}"
then
    chmod 500 "$FAIL_STATE_DIR"
fi
EOF
chmod +x "$tap_state_failure/origin.git/hooks/post-receive"
state_failure_output="$tap_state_failure/output"
if run_tap_rehearsal \
    "$tap_state_failure" \
    FAIL_STATE_DIR="$tap_state_failure/runner-temp" \
    > "$state_failure_output" 2>&1
then
    echo "unexpected success: state persistence failure after branch push" >&2
    exit 1
fi
chmod 700 "$tap_state_failure/runner-temp"
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
test "$(cat "$tap_verify_failure/pr-state")" = closed
test -z "$(
    git --git-dir="$tap_verify_failure/origin.git" for-each-ref \
        --format='%(refname)' refs/heads/rehearsal
)"
if grep -Fq 'tap-secret-must-not-appear' "$verify_output"
then
    echo "tap failure output exposed the token" >&2
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
test -z "$(
    git --git-dir="$tap_retry/origin.git" for-each-ref \
        --format='%(refname)' refs/heads/rehearsal
)"
grep -Fq "gh api --method PATCH 'repos/dcchuck/homebrew-tap/pulls/73' -f state=closed" \
    "$retry_output"
test -x "$tap_retry/runner-temp/car-go-clean-tap-rehearsal-cleanup.sh"
env \
    PATH="$tap_fake_bin:$PATH" \
    HOMEBREW_TAP_TOKEN='tap-secret-must-not-appear' \
    EXPECTED_TOKEN='tap-secret-must-not-appear' \
    GITHUB_RUN_ID=4242 \
    GITHUB_RUN_ATTEMPT=3 \
    GITHUB_WORKSPACE="$repo_root" \
    RUNNER_TEMP="$tap_retry/runner-temp" \
    FAKE_TAP_ORIGIN="$tap_retry/origin.git" \
    GH_LOG="$tap_retry/gh.log" \
    FAKE_PR_FILES="$tap_retry/pr-files" \
    FAKE_PR_STATE="$tap_retry/pr-state" \
    FAIL_CLOSE_ONCE_FILE="$tap_retry/close-failed-once" \
    "$tap_retry/runner-temp/car-go-clean-tap-rehearsal-cleanup.sh"
test "$(cat "$tap_retry/pr-state")" = closed
test ! -e "$tap_retry/runner-temp/car-go-clean-tap-rehearsal-cleanup.sh"
test "$(grep -F -c 'api --method DELETE repos/dcchuck/homebrew-tap/git/refs/heads/rehearsal/car-go-clean-4242-3' \
    "$tap_retry/gh.log")" -eq 1
if grep -Fq 'tap-secret-must-not-appear' "$retry_output"
then
    echo "tap cleanup failure exposed the token" >&2
    exit 1
fi
