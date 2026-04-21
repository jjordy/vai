#!/usr/bin/env bash
set -euo pipefail

# Cuts a vai release: bumps Cargo.toml/Cargo.lock, commits on main, tags, pushes.
# Usage: scripts/release.sh <X.Y.Z> [--no-push]

VERSION="${1:-}"
if [[ -z "$VERSION" ]]; then
    echo "usage: $0 <X.Y.Z> [--no-push]" >&2
    exit 1
fi
if ! [[ "$VERSION" =~ ^[0-9]+\.[0-9]+\.[0-9]+$ ]]; then
    echo "error: version must be X.Y.Z (got: $VERSION)" >&2
    exit 1
fi

PUSH=1
for arg in "${@:2}"; do
    case "$arg" in
        --no-push) PUSH=0 ;;
        *) echo "error: unknown arg: $arg" >&2; exit 1 ;;
    esac
done

TAG="v${VERSION}"

REPO_ROOT="$(git rev-parse --show-toplevel)"
cd "$REPO_ROOT"

BRANCH="$(git rev-parse --abbrev-ref HEAD)"
if [[ "$BRANCH" != "main" ]]; then
    echo "error: must be on main (currently on: $BRANCH)" >&2
    exit 1
fi

if [[ -n "$(git status --porcelain)" ]]; then
    echo "error: working tree not clean" >&2
    git status --short >&2
    exit 1
fi

if git rev-parse "$TAG" >/dev/null 2>&1; then
    echo "error: tag $TAG already exists locally" >&2
    exit 1
fi
if git ls-remote --tags origin "refs/tags/$TAG" | grep -q "$TAG"; then
    echo "error: tag $TAG already exists on origin" >&2
    exit 1
fi

echo "==> Pulling latest main"
git pull --ff-only origin main

CURRENT="$(grep -m1 '^version = ' Cargo.toml | cut -d'"' -f2)"
if [[ "$CURRENT" == "$VERSION" ]]; then
    echo "error: Cargo.toml is already at $VERSION — did you mean to re-tag?" >&2
    exit 1
fi

echo "==> Bumping Cargo.toml: $CURRENT -> $VERSION"
sed -i.bak "0,/^version = \"$CURRENT\"$/s//version = \"$VERSION\"/" Cargo.toml
rm -f Cargo.toml.bak

echo "==> Refreshing Cargo.lock"
cargo check --quiet >/dev/null

git add Cargo.toml Cargo.lock
git commit -m "chore: bump Cargo.toml to $VERSION for $TAG release"

echo "==> Tagging $TAG"
git tag "$TAG"

if [[ "$PUSH" == 1 ]]; then
    echo "==> Pushing main + $TAG to origin"
    git push origin main
    git push origin "$TAG"
    echo
    echo "Done. Release workflow should now be running:"
    echo "  gh run list --workflow=release.yml --limit 1"
else
    echo
    echo "Done (local only). To publish:"
    echo "  git push origin main && git push origin $TAG"
fi
