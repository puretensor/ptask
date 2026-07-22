#!/usr/bin/env bash
set -euo pipefail

cd "$(dirname "$0")/.."

mode=${1:---check}
if [[ "$mode" != "--check" && "$mode" != "--write" ]]; then
    echo "usage: $0 [--check|--write]" >&2
    exit 64
fi

generated_dir=$(mktemp -d)
trap 'rm -rf "$generated_dir"' EXIT

cargo run --locked --quiet --bin pt -- gen-manpage >"$generated_dir/pt.1"
sed -i 's/[[:space:]]\+$//' "$generated_dir/pt.1"
cargo run --locked --quiet --bin pt -- gen-completions bash >"$generated_dir/pt.bash"
cargo run --locked --quiet --bin pt -- gen-completions zsh >"$generated_dir/_pt"
cargo run --locked --quiet --bin pt -- gen-completions fish >"$generated_dir/pt.fish"

artifacts=(pt.1 pt.bash _pt pt.fish)
if [[ "$mode" == "--write" ]]; then
    for artifact in "${artifacts[@]}"; do
        cp "$generated_dir/$artifact" "docs/gen/$artifact"
    done
    echo "generated-artifacts: refreshed ${#artifacts[@]} files"
    exit 0
fi

stale=0
for artifact in "${artifacts[@]}"; do
    if ! diff -u "docs/gen/$artifact" "$generated_dir/$artifact"; then
        stale=1
    fi
done
if [[ "$stale" -ne 0 ]]; then
    echo "generated-artifacts: stale files; run bash scripts/generated-artifacts.sh --write" >&2
    exit 1
fi
echo "generated-artifacts: checked-in files match the CLI"
