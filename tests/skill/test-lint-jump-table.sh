#!/usr/bin/env bash
# tests/skill/test-lint-jump-table.sh
set -euo pipefail
source "$(dirname "$0")/test-helpers.sh"

ROOT="$(cd "$(dirname "$0")/../.." && pwd)"
LINT="$ROOT/tools/lint-skill.sh"
tmp=$(mktemp_test_dir)

mkdir -p "$tmp/docs/skills/cgn-onboard/guides"
cat > "$tmp/docs/skills/cgn-onboard/SKILL.md" <<'EOF'
---
name: cgn-onboard
description: x
when-to-use: y
---
Jump table:
- install → guides/01-install.md
- first-index → guides/02-first-index.md
EOF

# Only 01 exists; 02 missing → fail
cat > "$tmp/docs/skills/cgn-onboard/guides/01-install.md" <<'EOF'
# install
EOF
assert_exit 1 bash "$LINT" "$tmp/docs/skills/cgn-onboard"

# Add 02 → pass
cat > "$tmp/docs/skills/cgn-onboard/guides/02-first-index.md" <<'EOF'
# first-index
EOF
assert_exit 0 bash "$LINT" "$tmp/docs/skills/cgn-onboard"

pass
