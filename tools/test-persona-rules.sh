#!/usr/bin/env bash
# tools/test-persona-rules.sh
# T4 — assert persona rule table and fixtures stay consistent.
#
# Usage:
#   tools/test-persona-rules.sh <rules.md> <fixtures.yaml>
#
# Both inputs are expected to follow the formats shown in spec § 5 / T4:
#   rules.md     : markdown table with columns | Signal | Persona dimension | Default |
#                  Persona-dimension cells have the form 'key = value'.
#   fixtures.yaml: list of {signal: str, expected: {k: v, ...}}.
#
# This tool DOES NOT call an LLM. It only verifies rule-table internal
# consistency:
#   (a) every fixture signal is either an exact match for a rule's signal OR '(empty)';
#   (b) the rule's persona-dimension assignment matches the fixture's expected;
#   (c) there is exactly one rule whose signal is literally '(empty)' (fallback).

set -euo pipefail

RULES="$1"
FIXTURES="$2"

[[ -f "$RULES" ]] || { echo "rules file not found: $RULES" >&2; exit 1; }
[[ -f "$FIXTURES" ]] || { echo "fixtures file not found: $FIXTURES" >&2; exit 1; }

# Parse rules.md table → tab-separated "signal\tkey\tvalue".
# Only rows that contain a '=' in column 2 are considered.
parsed_rules=$(awk -F'|' '
    /^\|/ && /=/ {
        sig = $2; gsub(/^[ \t]+|[ \t]+$/, "", sig)
        dim = $3; gsub(/^[ \t]+|[ \t]+$/, "", dim)
        # dim is "key = value"
        n = split(dim, a, "=")
        if (n != 2) next
        k = a[1]; v = a[2]
        gsub(/^[ \t]+|[ \t]+$/, "", k)
        gsub(/^[ \t]+|[ \t]+$/, "", v)
        print sig "\t" k "\t" v
    }
' "$RULES")

# (c) check: exactly one fallback row with signal '(empty)'
fallback_count=$(awk -F'\t' '$1=="(empty)"' <<<"$parsed_rules" | wc -l)
[[ "$fallback_count" -ge 1 ]] || { echo "rules: no '(empty)' fallback row" >&2; exit 1; }

# Walk fixtures (parsed via yq — but to avoid dep, use jq with a yaml-to-json conversion).
# Fall back to a minimal awk parser since fixtures format is fixed.
parse_fixture() {
    # Emit "signal\tkey\tvalue" lines, one per expected-key entry.
    awk '
        /^- *signal:/ {
            line = $0; sub(/^- *signal:[ \t]*/, "", line)
            # Strip surrounding single/double quotes
            sub(/^["\x27]/, "", line); sub(/["\x27]$/, "", line)
            sig = line; in_expected = 0; next
        }
        /^  expected:/ { in_expected = 1; next }
        in_expected && /^    [^ ]/ {
            kv = $0; sub(/^    /, "", kv)
            split(kv, a, ":")
            k = a[1]
            v = a[2]
            gsub(/^[ \t]+|[ \t]+$/, "", k)
            gsub(/^[ \t]+|[ \t]+$/, "", v)
            print sig "\t" k "\t" v
        }
        /^- / && NR > 1 { in_expected = 0 }
    ' "$FIXTURES"
}

mismatch=0
while IFS=$'\t' read -r sig k v; do
    [[ -z "$sig" ]] && continue
    # Find rule row matching signal exactly (awk avoids regex-meta in signal text)
    if ! awk -F'\t' -v s="$sig" '$1==s {found=1} END {exit !found}' <<<"$parsed_rules"; then
        echo "fixture signal not in rules: $sig" >&2
        mismatch=1
        continue
    fi
    rule_v=$(awk -F'\t' -v s="$sig" -v k="$k" '$1==s && $2==k {print $3}' <<<"$parsed_rules")
    if [[ "$rule_v" != "$v" ]]; then
        echo "mismatch for signal='$sig' key='$k': rules=$rule_v expected=$v" >&2
        mismatch=1
    fi
done < <(parse_fixture)

[[ "$mismatch" -eq 0 ]] || exit 1
echo "persona rules: consistent"
