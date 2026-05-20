#!/usr/bin/env bash
# scripts/skill/aggregate.sh
# Concatenate SKILL.md (frontmatter stripped) + guides/*.md (in lexical order)
# → stdout. Used to build docs/skills/cgn-onboard/ONBOARDING.md for ShareOnboardingGuide.

set -euo pipefail

ROOT="${1:-docs/skills/cgn-onboard}"

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
