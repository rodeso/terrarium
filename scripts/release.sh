#!/usr/bin/env bash
# Cuts a release: scripts/release.sh 0.2.0
#
# Moves the "Unreleased" changelog notes under the new version, sets the
# version in Cargo.toml, runs the tests and a release build, then commits
# "Release vX.Y.Z" and creates an annotated tag. Nothing is pushed.
set -euo pipefail

cd "$(dirname "$0")/.."

die() { echo "release: $*" >&2; exit 1; }

version="${1:-}"
[[ "$version" =~ ^[0-9]+\.[0-9]+\.[0-9]+(-[0-9A-Za-z.-]+)?$ ]] \
    || die "usage: scripts/release.sh X.Y.Z (got '${version}')"
tag="v$version"

[[ -z "$(git status --porcelain)" ]] || die "the working tree has uncommitted changes; commit or stash them first"
git rev-parse -q --verify "refs/tags/$tag" >/dev/null && die "tag $tag already exists"
grep -q '^## \[Unreleased\]' CHANGELOG.md || die "CHANGELOG.md has no '## [Unreleased]' section"

# The Unreleased notes: everything between its heading and the next "## [".
notes="$(awk '/^## \[Unreleased\]/{on=1; next} /^## \[/{on=0} on' CHANGELOG.md)"
[[ -n "$(tr -d '[:space:]' <<<"$notes")" ]] || die "the Unreleased section of CHANGELOG.md is empty"

restore() {
    echo "release: failed; restoring Cargo.toml, Cargo.lock and CHANGELOG.md" >&2
    git checkout -- Cargo.toml Cargo.lock CHANGELOG.md
}
trap restore ERR

today="$(date +%Y-%m-%d)"
awk -v heading="## [$version] - $today" '
    /^## \[Unreleased\]/ && !done { print; print ""; print heading; done=1; next }
    { print }
' CHANGELOG.md >CHANGELOG.md.tmp && mv CHANGELOG.md.tmp CHANGELOG.md

# Only the [package] version line starts with `version =`.
awk -v v="$version" '
    /^version = "/ && !done { print "version = \"" v "\""; done=1; next }
    { print }
' Cargo.toml >Cargo.toml.tmp && mv Cargo.toml.tmp Cargo.toml

echo "release: running tests"
cargo test --features ssr
echo "release: building release"
cargo leptos build --release

trap - ERR
git add Cargo.toml Cargo.lock CHANGELOG.md
git commit -q -m "Release $tag"
printf 'Terrarium %s\n%s\n' "$tag" "$notes" | git tag -a --cleanup=whitespace "$tag" -F -  # keep "### Added" headings

echo
echo "Released $tag. To publish it:"
echo "  git push origin $(git rev-parse --abbrev-ref HEAD) --follow-tags"
