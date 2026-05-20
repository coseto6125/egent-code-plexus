#!/usr/bin/env bash
# tests/skill/test-lint-orphan-guide.sh
set -euo pipefail
source "$(dirname "$0")/test-helpers.sh"

ROOT="$(cd "$(dirname "$0")/../.." && pwd)"
LINT="$ROOT/scripts/skill/lint-skill.sh"
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
EOF
cat > "$tmp/docs/skills/cgn-onboard/guides/01-install.md" <<'EOF'
# install
EOF

# Orphan guide 99-leftover.md → fail
cat > "$tmp/docs/skills/cgn-onboard/guides/99-leftover.md" <<'EOF'
# orphan
EOF
assert_exit 1 bash "$LINT" "$tmp/docs/skills/cgn-onboard"

# Remove orphan → pass
rm "$tmp/docs/skills/cgn-onboard/guides/99-leftover.md"
assert_exit 0 bash "$LINT" "$tmp/docs/skills/cgn-onboard"

pass
