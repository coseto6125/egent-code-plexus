#!/usr/bin/env bash
# tests/skill/test-aggregate.sh
set -euo pipefail
source "$(dirname "$0")/test-helpers.sh"

ROOT="$(cd "$(dirname "$0")/../.." && pwd)"
AGG="$ROOT/tools/aggregate.sh"
tmp=$(mktemp_test_dir)

mkdir -p "$tmp/docs/skills/cgn-onboard/guides"
cat > "$tmp/docs/skills/cgn-onboard/SKILL.md" <<'EOF'
---
name: cgn-onboard
description: x
when-to-use: y
---

# Entry
some directive body.
EOF
cat > "$tmp/docs/skills/cgn-onboard/guides/01-install.md" <<'EOF'
# Phase 01 — install
install body.
EOF
cat > "$tmp/docs/skills/cgn-onboard/guides/02-first-index.md" <<'EOF'
# Phase 02 — first-index
index body.
EOF

bash "$AGG" "$tmp/docs/skills/cgn-onboard" > "$tmp/ONBOARDING.md"

# Assertions: aggregator output contains all source contents and is in guide-number order.
assert_grep '^# Entry' "$tmp/ONBOARDING.md"
assert_grep '^install body\.' "$tmp/ONBOARDING.md"
assert_grep '^# Phase 02 — first-index' "$tmp/ONBOARDING.md"
assert_grep '^index body\.' "$tmp/ONBOARDING.md"
# Frontmatter must be stripped — recipient's ShareOnboardingGuide doesn't need it
assert_no_grep '^name: cgn-onboard' "$tmp/ONBOARDING.md"
# Section divider present between SKILL.md and first guide
assert_grep '<!-- guide: 01-install -->' "$tmp/ONBOARDING.md"
assert_grep '<!-- guide: 02-first-index -->' "$tmp/ONBOARDING.md"
# Ordering: 01 must come before 02
line1=$(grep -n '<!-- guide: 01-install -->' "$tmp/ONBOARDING.md" | cut -d: -f1)
line2=$(grep -n '<!-- guide: 02-first-index -->' "$tmp/ONBOARDING.md" | cut -d: -f1)
[[ "$line1" -lt "$line2" ]] || { echo "FAIL: 01 should appear before 02"; exit 1; }

pass
