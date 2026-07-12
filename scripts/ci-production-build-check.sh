#!/usr/bin/env bash
# Keep every operator/release path on the production `pt` feature graph.
# A plain default-feature build succeeds but silently removes semantic dedup.
set -euo pipefail

cd "$(dirname "$0")/.."

require_line() {
    local file=$1
    local line=$2
    if ! grep -Fqx -- "$line" "$file"; then
        echo "production-build-check: $file is missing exact line: $line" >&2
        exit 1
    fi
}

# GitHub expands this expression in YAML; it must stay literal here.
# shellcheck disable=SC2016
require_line .github/workflows/release.yml \
    '        run: cargo build --release --bin pt --features native-ml --target ${{ matrix.target }} --locked'
require_line .github/workflows/ci.yml \
    '      - run: cargo build --release --bin pt --features native-ml --locked'
require_line .gitea/workflows/ci.yml \
    '        run: cargo build --release --bin pt --features native-ml --locked'
require_line scripts/release.sh \
    'cargo build --release --bin pt --features native-ml --locked'
require_line scripts/ansible/ptask.yml \
    '            cargo build --release --bin pt --features native-ml --target {{ ptask_target_triple }} --locked'

echo "production-build-check: release and fallback builds enable native-ml"
