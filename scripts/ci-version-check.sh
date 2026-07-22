#!/usr/bin/env bash
# Fail early when a version bump leaves Cargo.lock, workspace dependency
# requirements, or the checked-in manpage behind. Run before any unlocked
# Cargo command can silently repair the lockfile in its disposable checkout.
set -euo pipefail

cd "$(dirname "$0")/.."

version=$(
    sed -n '/^\[workspace.package\]/,/^\[/ s/^version = "\([^"]*\)"/\1/p' Cargo.toml
)
if [[ -z "$version" ]]; then
    echo "version-check: could not read workspace.package.version" >&2
    exit 1
fi

# This is intentionally first: `cargo clippy` without --locked would otherwise
# rewrite stale workspace package stamps and hide the repository defect.
cargo metadata --locked --no-deps --format-version 1 >/dev/null

dependency_count=$(grep -Ec '^ptask-(core|tui|server|bot|distill|notify)[[:space:]]*=.*version = "' Cargo.toml)
matching_dependencies=$(grep -Ec "^ptask-(core|tui|server|bot|distill|notify)[[:space:]]*=.*version = \"${version}\"" Cargo.toml)
if [[ "$dependency_count" -ne 6 || "$matching_dependencies" -ne 6 ]]; then
    echo "version-check: internal dependency requirements do not all match ${version}" >&2
    exit 1
fi

if ! grep -Eq "^\\.TH pt 1  +\\\"pt ${version}\\\"[[:space:]]*$" docs/gen/pt.1; then
    echo "version-check: docs/gen/pt.1 header is not version ${version}" >&2
    exit 1
fi
if ! grep -Fqx "v${version}" docs/gen/pt.1; then
    echo "version-check: docs/gen/pt.1 VERSION is not v${version}" >&2
    exit 1
fi

if [[ -n "${PTASK_RELEASE_TAG:-}" && "$PTASK_RELEASE_TAG" != "v${version}" ]]; then
    echo "version-check: release tag ${PTASK_RELEASE_TAG} does not match v${version}" >&2
    exit 1
fi

echo "version-check: workspace, lockfile, internal requirements, and manpage agree on ${version}"
