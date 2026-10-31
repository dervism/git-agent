#!/usr/bin/env bash
# Releases git-agent: runs the checks, bumps the version, publishes to crates.io,
# tags the release commit, pushes it, and creates a GitHub release.
#
#   scripts/release.sh 0.3.0             release version 0.3.0
#   scripts/release.sh --dry-run 0.3.0   show what would happen, change nothing
#   scripts/release.sh --yes 0.3.0       don't ask before publishing
#
# Commits and tags are made with your own git identity. Running the script again
# after a failure is safe: steps that already happened are detected and skipped.
# Until the crate is published, a failure or a "no" undoes the version bump commit.

set -euo pipefail

usage() {
    echo "usage: scripts/release.sh [--dry-run] [--yes] <version>" >&2
    exit 2
}

dry_run=false
assume_yes=false
version=""
for arg in "$@"; do
    case "$arg" in
        --dry-run) dry_run=true ;;
        -y | --yes) assume_yes=true ;;
        -h | --help) usage ;;
        -*)
            echo "unknown option: $arg" >&2
            usage
            ;;
        *)
            [[ -z "$version" ]] || usage
            version="${arg#v}"
            ;;
    esac
done
[[ -n "$version" ]] || usage
if [[ ! "$version" =~ ^[0-9]+\.[0-9]+\.[0-9]+(-[0-9A-Za-z.-]+)?$ ]]; then
    echo "error: '$version' is not a version like 1.2.3" >&2
    exit 2
fi
tag="v$version"

step() { printf '\n==> %s\n' "$*"; }
die() {
    printf 'error: %s\n' "$*" >&2
    exit 1
}

# Runs a command that changes something; with --dry-run, only prints it.
run() {
    if $dry_run; then
        printf '    (dry run) %s\n' "$*"
    else
        "$@"
    fi
}

confirm() {
    if $assume_yes || $dry_run; then
        return 0
    fi
    local answer
    read -r -p "$1 [y/N] " answer
    [[ "$answer" =~ ^[Yy]([Ee][Ss])?$ ]]
}

cd "$(git rev-parse --show-toplevel)"

for tool in git cargo gh curl; do
    command -v "$tool" >/dev/null || die "$tool is required but not installed"
done
gh auth status >/dev/null 2>&1 || die "gh is not logged in; run: gh auth login"

crate=$(sed -n 's/^name = "\(.*\)"$/\1/p' Cargo.toml | head -n 1)
current=$(sed -n 's/^version = "\(.*\)"$/\1/p' Cargo.toml | head -n 1)

step "Checking that $crate $version can be released"
[[ -z "$(git status --porcelain)" ]] \
    || die "the working tree has uncommitted changes; commit or stash them first"
branch=$(git symbolic-ref --quiet --short HEAD) || die "HEAD is detached; check out a branch"
default_branch=$(gh repo view --json defaultBranchRef --jq .defaultBranchRef.name)
[[ "$branch" == "$default_branch" ]] \
    || die "releases are made from $default_branch, but you are on $branch"
git fetch --quiet origin
[[ -z "$(git rev-list "HEAD..origin/$branch")" ]] \
    || die "origin/$branch has commits you don't have; run: git pull --rebase"
newest=$(printf '%s\n%s\n' "$current" "$version" | sort -V | tail -n 1)
[[ "$newest" == "$version" ]] || die "$version is older than the current version $current"

step "Running the checks"
cargo fmt --check
cargo clippy --quiet --all-targets -- -D warnings
# The tests run git, which prints a lot; show the output only if they fail.
if ! test_output=$(cargo test --quiet 2>&1); then
    printf '%s\n' "$test_output"
    die "the tests failed"
fi

start_commit=$(git rev-parse HEAD)
undo_bump() {
    if [[ "$(git rev-parse HEAD)" != "$start_commit" ]]; then
        echo "Undoing the version bump commit." >&2
        git reset --quiet --hard "$start_commit"
    fi
}
trap undo_bump EXIT

if [[ "$current" == "$version" ]]; then
    step "Cargo.toml is already at $version"
else
    step "Bumping the version from $current to $version"
    run sed -i.bak "s/^version = \"$current\"$/version = \"$version\"/" Cargo.toml
    run rm Cargo.toml.bak
    run cargo update --quiet --workspace --offline
    run git commit --quiet --all --message "chore(release): $tag"
fi

published_status=$(curl --silent --output /dev/null --write-out '%{http_code}' \
    --user-agent "$crate-release-script" "https://crates.io/api/v1/crates/$crate/$version")
if [[ "$published_status" == 200 ]]; then
    step "$crate $version is already on crates.io"
else
    step "Verifying the package"
    cargo publish --dry-run --quiet
    confirm "Publish $crate $version to crates.io? This cannot be undone." \
        || die "cancelled; nothing was published"
    step "Publishing $crate $version to crates.io"
    run cargo publish --quiet
fi
trap - EXIT
# Package verification builds confuse Cargo's later builds of this crate; clear them.
cargo clean --quiet --package "$crate"

if git rev-parse --quiet --verify "refs/tags/$tag" >/dev/null; then
    [[ "$(git rev-parse "$tag^{commit}")" == "$(git rev-parse HEAD)" ]] \
        || die "tag $tag already exists on a different commit"
    step "Tag $tag already exists"
else
    step "Tagging $tag"
    run git tag --annotate "$tag" --message "$crate $version"
fi

step "Pushing $branch and $tag to origin"
run git push --quiet --atomic origin "$branch" "refs/tags/$tag"

if gh release view "$tag" >/dev/null 2>&1; then
    step "GitHub release $tag already exists"
else
    step "Creating the GitHub release"
    run gh release create "$tag" --title "$tag" --generate-notes --verify-tag
fi

if $dry_run; then
    step "Dry run finished; nothing was changed"
else
    step "Released $crate $version"
    echo "    https://crates.io/crates/$crate/$version"
    echo "    $(gh repo view --json url --jq .url)/releases/tag/$tag"
fi
