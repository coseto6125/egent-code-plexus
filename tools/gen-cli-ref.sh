#!/usr/bin/env bash
# tools/gen-cli-ref.sh
# Generate per-version CLI reference cards from `cgn --help`.
#
# Usage:
#   gen-cli-ref.sh [cgn-binary] [output-dir]
#
# Defaults:
#   cgn-binary   = ./target/release/cgn if it exists, else `cgn` from PATH
#   output-dir   = docs/skills/cgn-onboard/_shared/cli
#
# Output layout:
#   <output-dir>/<cmd>.md            (one per top-level + selected sub-commands)

set -euo pipefail

# Default cgn binary
default_cgn() {
    if [[ -x ./target/release/cgn ]]; then echo "./target/release/cgn"
    elif command -v cgn >/dev/null; then echo "cgn"
    else echo ""; fi
}

CGN="${1:-$(default_cgn)}"
OUT="${2:-docs/skills/cgn-onboard/_shared/cli}"

[[ -n "$CGN" ]] || { echo "gen-cli-ref: no cgn binary found" >&2; exit 1; }

mkdir -p "$OUT"

# Commands to capture: top-level + curated sub-commands actually used in guides.
# When new guides reference a new command, add it here.
declare -a TOPLEVEL=(find impact inspect cypher routes coverage diff rename)
declare -a SUB=(
    "admin:index"
    "admin:group"
    "admin:mcp"
    "group:find"
    "group:contracts"
    "group:impact"
)

for cmd in "${TOPLEVEL[@]}"; do
    out="$OUT/$cmd.md"
    timeout 10 "$CGN" "$cmd" --help > "$out" 2>/dev/null || { echo "warn: $cmd has no --help; skipped" >&2; rm -f "$out"; }
done

for entry in "${SUB[@]}"; do
    parent="${entry%%:*}"
    child="${entry##*:}"
    out="$OUT/${parent}-${child}.md"
    timeout 10 "$CGN" "$parent" "$child" --help > "$out" 2>/dev/null || { echo "warn: $parent $child has no --help; skipped" >&2; rm -f "$out"; }
done

echo "gen-cli-ref: wrote updated references to $OUT/"
