#!/bin/sh
set -eu

repo_root=$(CDPATH='' cd -- "$(dirname -- "$0")/.." && pwd)
validator="$repo_root/scripts/validate-release-inputs.sh"
dist_installer="$repo_root/scripts/install-cargo-dist.sh"
asset_verifier="$repo_root/scripts/verify-release-assets.sh"
formula_renderer="$repo_root/scripts/render-homebrew-formula.sh"
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

for script in "$validator" "$dist_installer" "$asset_verifier" "$formula_renderer"
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
    printf '%s  %s\n' "$(hash_file "$artifacts/$archive")" "$archive" \
        > "$artifacts/$archive.sha256"
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
