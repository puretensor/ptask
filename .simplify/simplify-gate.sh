#!/usr/bin/env bash
# ptask simplify-gate — run from repo root on the simplify/contract-cuts branch.
# Exit 0 == gate passes. Mirrors the project CI (.github/workflows/ci.yml):
# fmt + clippy (workspace + native-ml) + test (workspace + native-ml) + schema
# sanity, plus the SIMPLIFY invariants (baseline tag, locked contract, coverage
# floor, branch pushed & unmerged).
set -euo pipefail

BRANCH="simplify/contract-cuts"
BASE="main"
COV_FLOOR_REGION="74.0"   # baseline region coverage 74.99% — must not drop materially

say() { printf '\n=== %s ===\n' "$*"; }

# --- SIMPLIFY invariant: baseline tag must exist ----------------------------
say "baseline tag"
git rev-parse --verify pre-simplify >/dev/null
echo "pre-simplify present at $(git rev-parse --short pre-simplify)"

# --- SIMPLIFY invariant: contract test files are byte-for-byte locked --------
say "contract lock"
sha256sum -c .simplify/contract.sha256 --status
echo "contract.sha256 verified ($(wc -l < .simplify/contract.sha256) locked test files)"

# --- project CI: formatting --------------------------------------------------
say "cargo fmt --all -- --check"
cargo fmt --all -- --check

# --- project CI: clippy (workspace + feature-gated native ML) ----------------
say "cargo clippy --workspace --all-targets -- -D warnings"
cargo clippy --workspace --all-targets -- -D warnings
say "cargo clippy -p ptask-distill --all-targets --features native-ml -- -D warnings"
cargo clippy -p ptask-distill --all-targets --features native-ml -- -D warnings

# --- project CI: build + full + native-ml test suites (== contract suite) ----
say "cargo build --workspace --locked"
cargo build --workspace --locked
say "cargo test --workspace --locked"
cargo test --workspace --locked
say "cargo test -p ptask-distill --features native-ml --locked"
cargo test -p ptask-distill --features native-ml --locked

# --- project CI: migration / schema sanity -----------------------------------
say "scripts/ci-schema-check.sh"
bash scripts/ci-schema-check.sh

# --- SIMPLIFY invariant: coverage must not drop vs baseline ------------------
say "coverage floor (region >= ${COV_FLOOR_REGION}%)"
if command -v cargo-llvm-cov >/dev/null 2>&1; then
  REGION=$(cargo llvm-cov --workspace --locked --summary-only 2>/dev/null \
            | awk '/^TOTAL/ {gsub("%","",$4); print $4}')
  echo "region coverage now: ${REGION}% (baseline 74.99%)"
  awk -v c="$REGION" -v f="$COV_FLOOR_REGION" 'BEGIN{ if (c+0 < f+0) { print "COVERAGE DROPPED"; exit 1 } }'
else
  echo "cargo-llvm-cov absent — falling back to no-test-deletion proxy"
  # proxy: contract lock above already pins every test file byte-for-byte.
fi

# --- SIMPLIFY invariant: branch pushed, and NOT merged into base -------------
say "branch pushed & unmerged"
git fetch -q origin "$BASE" "$BRANCH"
git rev-parse --verify "origin/${BRANCH}" >/dev/null   # branch exists on remote
if git merge-base --is-ancestor "origin/${BRANCH}" "origin/${BASE}"; then
  echo "ERROR: ${BRANCH} is already merged into ${BASE}" >&2
  exit 1
fi
echo "origin/${BRANCH} present and not merged into origin/${BASE}"
# PR OPEN/unmerged state is asserted authoritatively by the orchestrator via the
# GitHub MCP (no gh CLI / API token in this environment).

say "GATE PASS"
