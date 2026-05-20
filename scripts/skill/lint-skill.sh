#!/usr/bin/env bash
# scripts/skill/lint-skill.sh
# T1 structural lint for the ecp-onboard SKILL pack.
# Usage: lint-skill.sh [path-to-skill-root]   # default: docs/skills/ecp-onboard

set -euo pipefail

ROOT="${1:-docs/skills/ecp-onboard}"

fail() { echo "lint FAIL: $1" >&2; exit 1; }

# --- Check 1: SKILL.md exists with valid frontmatter (name, description, when-to-use) ---
skill="$ROOT/SKILL.md"
[[ -f "$skill" ]] || fail "SKILL.md missing at $skill"

# Extract frontmatter block (between leading --- and next ---). Tolerate CRLF.
fm=$(awk '/^---\r?$/{c++; next} c==1' "$skill")
[[ -n "$fm" ]] || fail "SKILL.md has no frontmatter"

for key in name description when-to-use; do
    grep -qE "^${key}:" <<<"$fm" || fail "SKILL.md frontmatter missing '$key'"
done

# --- Check 2: guides/*.md have NO frontmatter ---
if [[ -d "$ROOT/guides" ]]; then
    while IFS= read -r -d '' g; do
        if head -1 "$g" | grep -qE "^---\r?$"; then
            fail "$g has frontmatter (only SKILL.md should)"
        fi
    done < <(find "$ROOT/guides" -maxdepth 1 -name '*.md' -print0)
fi

# --- Check 3: every guides/*.md referenced in SKILL.md's jump table actually exists ---
# Match patterns like 'guides/01-install.md' anywhere in SKILL.md.
while IFS= read -r ref; do
    target="$ROOT/$ref"
    [[ -f "$target" ]] || fail "jump-table reference '$ref' resolves to missing file $target"
done < <(grep -oE 'guides/[A-Za-z0-9._-]+\.md' "$skill" | sort -u)

# --- Check 4: every guides/*.md is referenced from SKILL.md (no orphans) ---
if [[ -d "$ROOT/guides" ]]; then
    while IFS= read -r -d '' g; do
        rel="guides/$(basename "$g")"
        grep -qF "$rel" "$skill" || fail "orphan guide: $rel not referenced in SKILL.md"
    done < <(find "$ROOT/guides" -maxdepth 1 -name '*.md' -print0)
fi

echo "lint OK: $ROOT"
