#!/usr/bin/env bash
# The cross-repo half of the vendored-core guarantee.
#
# Each repo's test suite pins the digest of its OWN copy of face-unlock.js, which catches an edit
# made without updating that pin. It does NOT prove the three copies agree: update one repo's copy
# and its pin together and every board stays green while the files diverge. This script is the
# comparison nothing else makes. Run it whenever face-unlock.js changes.
#
#   scripts/verify-vendored-core.sh [fleet-root]     (default: $HOME)
set -euo pipefail
root="${1:-$HOME}"
paths=(
  "$root/ptask/dashboard/www/face-unlock.js"
  "$root/ptve/ptve-ui/src/lib/face-unlock.js"
  "$root/specola/frontend/js/face-unlock.js"
)

missing=0
for p in "${paths[@]}"; do
  [[ -f "$p" ]] || { echo "MISSING: $p" >&2; missing=1; }
done
[[ $missing -eq 0 ]] || { echo "verify-vendored-core: cannot compare — checkout the missing repo(s)" >&2; exit 2; }

mapfile -t sums < <(sha256sum "${paths[@]}" | awk '{print $1}')
for i in "${!paths[@]}"; do printf '%s  %s\n' "${sums[$i]}" "${paths[$i]}"; done

uniq_count=$(printf '%s\n' "${sums[@]}" | sort -u | wc -l)
if [[ "$uniq_count" -ne 1 ]]; then
  echo "DIVERGED: the vendored core differs between repos. Re-vendor from one source and update all three pins." >&2
  exit 1
fi

# The pins must also agree with the file they claim to describe.
pin_files=(
  "$root/ptask/dashboard/tests/face_unlock.test.mjs"
  "$root/ptve/ptve-ui/src/lib/face-unlock.test.ts"
  "$root/specola/tests/face_unlock.test.js"
)
bad=0
for f in "${pin_files[@]}"; do
  [[ -f "$f" ]] || continue
  if ! grep -q "${sums[0]}" "$f"; then
    echo "PIN STALE: $f does not pin ${sums[0]}" >&2
    bad=1
  fi
done
[[ $bad -eq 0 ]] || exit 1

echo "OK: all three vendored cores and all three pins agree on ${sums[0]}"
