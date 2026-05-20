#!/usr/bin/env bash
# scripts/skill/gen-cli-ref.sh
# Generate per-version CLI reference cards from `ecp --help`.
#
# Usage:
#   gen-cli-ref.sh [ecp-binary] [output-dir]
#
# Defaults:
#   ecp-binary   = ./target/release/ecp if it exists, else `ecp` from PATH
#   output-dir   = docs/skills/ecp-onboard/_shared/cli
#
# Output layout:
#   <output-dir>/<cmd>.md            (one per top-level + selected sub-commands)

set -euo pipefail

# Default ecp binary
default_ecp() {
    if [[ -x ./target/release/ecp ]]; then echo "./target/release/ecp"
    elif command -v ecp >/dev/null; then echo "ecp"
    else echo ""; fi
}

ECP="${1:-$(default_ecp)}"
OUT="${2:-docs/skills/ecp-onboard/_shared/cli}"

[[ -n "$ECP" ]] || { echo "gen-cli-ref: no ecp binary found" >&2; exit 1; }

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
    timeout 10 "$ECP" "$cmd" --help > "$out" 2>/dev/null || { echo "warn: $cmd has no --help; skipped" >&2; rm -f "$out"; }
done

for entry in "${SUB[@]}"; do
    parent="${entry%%:*}"
    child="${entry##*:}"
    out="$OUT/${parent}-${child}.md"
    timeout 10 "$ECP" "$parent" "$child" --help > "$out" 2>/dev/null || { echo "warn: $parent $child has no --help; skipped" >&2; rm -f "$out"; }
done

echo "gen-cli-ref: wrote updated references to $OUT/"
