#!/usr/bin/env bash
# tests/skill/test-persona-rules.sh
set -euo pipefail
source "$(dirname "$0")/test-helpers.sh"

ROOT="$(cd "$(dirname "$0")/../.." && pwd)"
TOOL="$ROOT/scripts/skill/test-persona-rules.sh"
tmp=$(mktemp_test_dir)

mkdir -p "$tmp/refs"

# Minimal rule table — one rule + one default
cat > "$tmp/refs/persona-inference.md" <<'EOF'
# Persona Inference

| Signal | Persona dimension | Default |
|---|---|---|
| CLAUDE.md contains "繁體中文" | lang_pref = zh-TW | (n/a) |
| (empty) | lang_pref = unknown | conservative |
EOF

# Fixtures: one matching the rule, one empty
cat > "$tmp/persona-fixtures.yaml" <<'EOF'
- signal: 'CLAUDE.md contains "繁體中文"'
  expected:
    lang_pref: zh-TW
- signal: '(empty)'
  expected:
    lang_pref: unknown
EOF

# Tool runs against rules + fixtures, asserts derived persona equals expected.
assert_exit 0 bash "$TOOL" "$tmp/refs/persona-inference.md" "$tmp/persona-fixtures.yaml"

# Negative: mutate fixture so expected mismatches → tool should fail
cat > "$tmp/persona-fixtures.yaml" <<'EOF'
- signal: 'CLAUDE.md contains "繁體中文"'
  expected:
    lang_pref: ja-JP
EOF
assert_exit 1 bash "$TOOL" "$tmp/refs/persona-inference.md" "$tmp/persona-fixtures.yaml"

pass
