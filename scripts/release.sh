#!/usr/bin/env bash
# Tag + push helper. Runs the standard release verification then tags the
# current commit and pushes the tag to the canonical GitHub remote. GitHub
# runs `.github/workflows/release.yml` (self-hosted tensor-core runner) and
# publishes the GitHub Release. `.gitea/workflows/release.yml` publishes the
# same per-arch asset names (each with a .sha256) to the Gitea release, but
# it is UNVERIFIED that a tag arriving by mirror sync triggers it (Gitea may
# not fire push events for mirror updates): check the Gitea release page
# and, if it is missing, push the tag to the Gitea remote directly. The
# binaries differ: GitHub's x86_64 build runs on tensor-core (glibc 2.39
# floor), Gitea's on ubuntu-22.04 (glibc 2.35 floor).
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

Release $TAG tagged + pushed to GitHub, which builds and publishes it:
    https://github.com/puretensor/ptask/releases/tag/$TAG
  Gitea mirror (.gitea/workflows/release.yml). Whether a mirror-synced tag
  triggers it is unverified: check this page, and push the tag to Gitea
  directly if no release appears:
    http://100.92.245.5:3002/puretensor/ptask/releases/tag/$TAG

EOF
