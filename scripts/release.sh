#!/usr/bin/env bash
# Tag + push helper. Runs the standard release verification then tags the
# current commit and pushes the tag to the canonical GitHub remote. The tag
# push triggers `.github/workflows/release.yml` which builds the binary and
# attaches it to the GitHub Release; the Gitea mirror syncs automatically.
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

Release $TAG tagged + pushed. GitHub Actions will build and publish at:
  https://github.com/puretensor/ptask/releases/tag/$TAG

EOF
