#!/usr/bin/env bash
# Tag + push helper. Runs the standard release verification then tags the
# current commit and pushes the tag to the canonical GitHub remote. The tag
# drives both forges' release workflows: GitHub runs
# `.github/workflows/release.yml` (self-hosted tensor-core runner) and
# publishes the GitHub Release; the Gitea mirror syncs the tag from GitHub
# and runs `.gitea/workflows/release.yml`, publishing the same per-arch
# assets (each with a .sha256) to the Gitea release, the sovereign copy.
#
# Usage: scripts/release.sh v0.10.0
set -euo pipefail

if [[ "${1:-}" == "" ]]; then
    echo "usage: $0 v<X>.<Y>.<Z>" >&2
    exit 64
fi
TAG=$1

# 1. Sanity: working tree clean, on main.
if [[ -n "$(git status --porcelain)" ]]; then
    echo "release: working tree dirty, aborting" >&2
    git status --short >&2
    exit 1
fi
BRANCH=$(git rev-parse --abbrev-ref HEAD)
if [[ "$BRANCH" != "main" ]]; then
    echo "release: not on main (on $BRANCH), aborting" >&2
    exit 1
fi

# 2. Version + Cargo gates.
echo "release: version + lockfile coherence"
PTASK_RELEASE_TAG="$TAG" bash scripts/ci-version-check.sh
echo "release: generated CLI artifacts"
bash scripts/generated-artifacts.sh --check
echo "release: cargo fmt --check"
cargo fmt --all -- --check
echo "release: cargo clippy"
cargo clippy --workspace --all-targets -- -D warnings
echo "release: cargo clippy (production native-ml feature)"
cargo clippy -p ptask-cli --all-targets --features native-ml -- -D warnings
echo "release: cargo test"
cargo test --workspace --locked --quiet
echo "release: cargo test (native-ml)"
cargo test -p ptask-distill --features native-ml --locked --quiet
echo "release: production feature wiring"
bash scripts/ci-production-build-check.sh
echo "release: production binary build"
cargo build --release --bin pt --features native-ml --locked

# 3. Tag + push to the canonical forge. The mirror syncs from GitHub.
echo "release: tagging $TAG"
git tag -a "$TAG" -m "Release $TAG"
echo "release: push origin"
git push origin "$TAG"

cat <<EOF

Release $TAG tagged + pushed. Both forges build and publish it:
  GitHub (.github/workflows/release.yml):
    https://github.com/puretensor/ptask/releases/tag/$TAG
  Gitea mirror (.gitea/workflows/release.yml, once the mirror syncs the tag):
    http://100.92.245.5:3002/puretensor/ptask/releases/tag/$TAG

EOF
