#!/usr/bin/env bash
# scripts/skill/aggregate.sh
# Concatenate SKILL.md (frontmatter stripped) + guides/*.md (in lexical order)
# → stdout. Builds docs/skills/ecp-onboard/ONBOARDING.md, the agent-facing
# onboarding guide README links to.
#
# --check compares the committed ONBOARDING.md against a fresh build and exits
# 1 on drift. Nothing regenerated the file between PR #442 (2026-05-25) and
# this check, so it missed PR #459's @ECP.md guidance-import block and PR
# #646's whole SKILL.md rewrite — 119 lines of drift in a published artifact.

set -euo pipefail

CHECK=0
if [[ "${1:-}" == "--check" ]]; then
    CHECK=1
    shift
fi

ROOT="${1:-docs/skills/ecp-onboard}"

if (( CHECK )); then
    committed="$ROOT/ONBOARDING.md"
    [[ -f "$committed" ]] || { echo "aggregate --check: $committed missing" >&2; exit 1; }
    if diff -u "$committed" <("$0" "$ROOT"); then
        echo "aggregate --check: $committed is in sync"
        exit 0
    fi
    echo "aggregate --check: $committed is stale — run 'bash $0 $ROOT > $committed'" >&2
    exit 1
fi

skill="$ROOT/SKILL.md"
[[ -f "$skill" ]] || { echo "aggregate: SKILL.md missing at $skill" >&2; exit 1; }

# Strip leading frontmatter (between first '---' and second '---' on their own lines).
# Tolerate CRLF line endings.
awk '
    BEGIN { in_fm = 0; done_fm = 0 }
    /^---\r?$/ {
        if (!done_fm) {
            if (in_fm) { in_fm = 0; done_fm = 1; next }
            else { in_fm = 1; next }
        }
    }
    { if (!in_fm) print }
' "$skill"

# Append each guide, in lexical filename order, with a divider comment.
if [[ -d "$ROOT/guides" ]]; then
    while IFS= read -r g; do
        slug=$(basename "$g" .md)
        printf '\n\n<!-- guide: %s -->\n\n' "$slug"
        cat "$g"
    done < <(find "$ROOT/guides" -maxdepth 1 -name '*.md' | sort)
fi
