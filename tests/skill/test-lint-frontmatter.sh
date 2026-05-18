#!/usr/bin/env bash
# tests/skill/test-lint-frontmatter.sh
set -euo pipefail
source "$(dirname "$0")/test-helpers.sh"

ROOT="$(cd "$(dirname "$0")/../.." && pwd)"
LINT="$ROOT/tools/lint-skill.sh"
tmp=$(mktemp_test_dir)

# Case 1: SKILL.md missing → lint fails with exit 1
mkdir -p "$tmp/docs/skills/gnx-onboard"
assert_exit 1 bash "$LINT" "$tmp/docs/skills/gnx-onboard"

# Case 2: SKILL.md missing frontmatter → fails
cat > "$tmp/docs/skills/gnx-onboard/SKILL.md" <<'EOF'
# gnx-onboard
no frontmatter here
EOF
assert_exit 1 bash "$LINT" "$tmp/docs/skills/gnx-onboard"

# Case 3: SKILL.md missing required key (description) → fails
cat > "$tmp/docs/skills/gnx-onboard/SKILL.md" <<'EOF'
---
name: gnx-onboard
when-to-use: User says install gnx
---
body
EOF
assert_exit 1 bash "$LINT" "$tmp/docs/skills/gnx-onboard"

# Case 4: SKILL.md with all required keys → passes
cat > "$tmp/docs/skills/gnx-onboard/SKILL.md" <<'EOF'
---
name: gnx-onboard
description: Personalized installation wizard for graph-nexus.
when-to-use: User says install gnx / set up graph-nexus.
---
- Jump table:
  - install → guides/01-install.md
EOF
mkdir -p "$tmp/docs/skills/gnx-onboard/guides"
cat > "$tmp/docs/skills/gnx-onboard/guides/01-install.md" <<'EOF'
# install
EOF
assert_exit 0 bash "$LINT" "$tmp/docs/skills/gnx-onboard"

pass
