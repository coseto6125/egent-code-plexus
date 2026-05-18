#!/usr/bin/env bash
# Run all test-*.sh in this directory. Stop on first failure.

set -euo pipefail

cd "$(dirname "$0")"

count=0
for t in test-*.sh; do
    [[ -f "$t" ]] || continue
    [[ "$t" == "test-helpers.sh" ]] && continue
    [[ -x "$t" ]] || chmod +x "$t"
    bash "$t"
    count=$((count + 1))
done

echo "---"
echo "all $count SKILL tests passed"
