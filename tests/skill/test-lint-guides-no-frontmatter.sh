#!/usr/bin/env bash
# tests/skill/test-lint-guides-no-frontmatter.sh
set -euo pipefail
source "$(dirname "$0")/test-helpers.sh"

ROOT="$(cd "$(dirname "$0")/../.." && pwd)"
LINT="$ROOT/scripts/skill/lint-skill.sh"
tmp=$(mktemp_test_dir)

mkdir -p "$tmp/docs/skills/ecp-onboard/guides"
cat > "$tmp/docs/skills/ecp-onboard/SKILL.md" <<'EOF'
---
name: ecp-onboard
description: x
when-to-use: y
---
- Jump table:
  - install → guides/01-install.md
EOF

# A guide with frontmatter → lint must fail
cat > "$tmp/docs/skills/ecp-onboard/guides/01-install.md" <<'EOF'
---
name: install
---
# install
EOF
assert_exit 1 bash "$LINT" "$tmp/docs/skills/ecp-onboard"

# Remove frontmatter → passes
cat > "$tmp/docs/skills/ecp-onboard/guides/01-install.md" <<'EOF'
# install
EOF
assert_exit 0 bash "$LINT" "$tmp/docs/skills/ecp-onboard"

pass
