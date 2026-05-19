# cgn-onboard SKILL Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Build the cgn-onboard SKILL pack — a personalized installation + configuration wizard for `code-graph-nexus` distributed as a layered markdown SKILL — entirely inside the existing `code-graph-nexus` repo, ready for the 4 distribution outlets defined in the spec.

**Architecture:** Three-layer SKILL pack under `docs/skills/cgn-onboard/` (SKILL.md + guides/ + _shared/), bash tooling under `tools/`, tests + smoke playbook under `tests/skill/`, CI workflows under `.github/workflows/`. The pack reuses the existing `cgn` CLI release cycle (single repo, zero drift). Implementation TDDs all bash tools (each tool ships with a `tests/skill/test-<tool>.sh` integration test); markdown content is validated by the lint tool + aggregator round-trip.

**Tech Stack:** bash 5.x, jq, awk, sed, diff, GitHub Actions, markdown. No language runtime required beyond standard *nix tools. `cgn` (built from this same workspace) for CLI-ref generation.

**Spec:** `docs/superpowers/specs/2026-05-18-cgn-onboard-skill-design.md`

---

## File Structure

This plan creates the following files (all paths relative to `code-graph-nexus/` repo root):

```
docs/skills/cgn-onboard/                            ← Layer 1 + 2 + 3 SKILL pack
├── ONBOARDING.md                                    ← (Task 22) aggregator output, committed
├── SKILL.md                                         ← (Task 14)
├── guides/
│   ├── 01-install.md                                ← (Task 15)
│   ├── 02-first-index.md                            ← (Task 16)
│   ├── 03-group.md                                  ← (Task 17)
│   ├── 04-mcp.md                                    ← (Task 18)
│   └── 05-summary.md                                ← (Task 19)
└── _shared/
    ├── cli/
    │   ├── manifest.json                            ← (Task 21) generated
    │   └── 0.1.5/                                   ← (Task 21) generated; one .md per cgn subcmd
    └── refs/
        ├── env-detect.md                            ← (Task 12)
        ├── persona-inference.md                     ← (Task 10)
        └── recommendation-templates.md              ← (Task 13)

tools/                                              ← Bash tooling (no Cargo crate)
├── lint-skill.sh                                    ← (Tasks 3–6)
├── aggregate.sh                                     ← (Task 7)
├── gen-cli-ref.sh                                   ← (Task 8)
└── test-persona-rules.sh                            ← (Task 9)

tests/skill/                                        ← Test fixtures + harness
├── test-helpers.sh                                  ← (Task 2)
├── test-lint-frontmatter.sh                         ← (Task 3)
├── test-lint-guides-no-frontmatter.sh               ← (Task 4)
├── test-lint-jump-table.sh                          ← (Task 5)
├── test-lint-orphan-guide.sh                        ← (Task 6)
├── test-aggregate.sh                                ← (Task 7)
├── test-gen-cli-ref.sh                              ← (Task 8)
├── test-persona-rules.sh                            ← (Task 9)
├── persona-fixtures.yaml                            ← (Task 11)
├── smoke-playbook.md                                ← (Task 25)
└── run-all.sh                                       ← (Task 2)

.github/workflows/
├── skill-aggregate.yml                              ← (Task 23)
└── skill-cli-ref.yml                                ← (Task 24)

README.md                                           ← (Task 26) — modify existing
```

**Important constraints:**

- Every bash tool ships with a corresponding integration test before/alongside the tool. TDD applies.
- Markdown content files (SKILL.md, guides, refs) are NOT TDD'd by unit tests — they are validated by `lint-skill.sh` (structure) and the smoke playbook (behavior).
- Use **traditional Chinese (繁體中文)** in any chat-facing example text within the SKILL pack; **English** in all script comments, error messages, and YAML config (consistent with the repo's existing convention).
- Bash scripts: `#!/usr/bin/env bash` shebang + `set -euo pipefail` at top of every script. No `set +e` blocks without explicit comment.

---

## Task 1: Bootstrap directory skeleton

**Files:**
- Create: `docs/skills/cgn-onboard/.gitkeep`
- Create: `docs/skills/cgn-onboard/guides/.gitkeep`
- Create: `docs/skills/cgn-onboard/_shared/cli/.gitkeep`
- Create: `docs/skills/cgn-onboard/_shared/refs/.gitkeep`
- Create: `tools/.gitkeep` (if `tools/` doesn't yet exist)
- Create: `tests/skill/.gitkeep`

- [ ] **Step 1: Create directories**

```bash
mkdir -p docs/skills/cgn-onboard/guides
mkdir -p docs/skills/cgn-onboard/_shared/cli
mkdir -p docs/skills/cgn-onboard/_shared/refs
mkdir -p tools
mkdir -p tests/skill
```

- [ ] **Step 2: Add .gitkeep so empty dirs are tracked**

```bash
touch docs/skills/cgn-onboard/.gitkeep \
      docs/skills/cgn-onboard/guides/.gitkeep \
      docs/skills/cgn-onboard/_shared/cli/.gitkeep \
      docs/skills/cgn-onboard/_shared/refs/.gitkeep \
      tools/.gitkeep \
      tests/skill/.gitkeep
```

- [ ] **Step 3: Verify**

Run: `find docs/skills/cgn-onboard tools tests/skill -type d`
Expected: shows all 7 directories.

- [ ] **Step 4: Commit**

```bash
git add docs/skills/cgn-onboard tools/.gitkeep tests/skill/.gitkeep
git commit -m "chore(onboard): bootstrap directory skeleton"
```

---

## Task 2: Test harness — helpers + runner

**Files:**
- Create: `tests/skill/test-helpers.sh`
- Create: `tests/skill/run-all.sh`

- [ ] **Step 1: Write `tests/skill/test-helpers.sh`**

```bash
#!/usr/bin/env bash
# Source-only file. Provides assert_* helpers + tmpdir management for SKILL tests.
# Use:
#   source "$(dirname "$0")/test-helpers.sh"
#   tmp=$(mktemp_test_dir)
#   ... run things ...
#   pass

set -euo pipefail

mktemp_test_dir() {
    local dir
    dir=$(mktemp -d -t "skill-test-XXXXXX")
    # shellcheck disable=SC2064
    trap "rm -rf '$dir'" EXIT
    echo "$dir"
}

assert_equal() {
    local expected="$1" actual="$2" label="${3:-values}"
    if [[ "$expected" != "$actual" ]]; then
        echo "FAIL: $label" >&2
        echo "  expected: $expected" >&2
        echo "  actual:   $actual" >&2
        exit 1
    fi
}

assert_file_exists() {
    local path="$1"
    if [[ ! -f "$path" ]]; then
        echo "FAIL: file not found: $path" >&2
        exit 1
    fi
}

assert_grep() {
    local pattern="$1" file="$2"
    if ! grep -qE "$pattern" "$file"; then
        echo "FAIL: pattern '$pattern' not found in $file" >&2
        exit 1
    fi
}

assert_no_grep() {
    local pattern="$1" file="$2"
    if grep -qE "$pattern" "$file"; then
        echo "FAIL: pattern '$pattern' unexpectedly found in $file" >&2
        exit 1
    fi
}

assert_exit() {
    local expected_code="$1"; shift
    local actual_code=0
    "$@" || actual_code=$?
    assert_equal "$expected_code" "$actual_code" "exit code of: $*"
}

pass() {
    echo "PASS: ${BASH_SOURCE[1]##*/}"
}
```

- [ ] **Step 2: Write `tests/skill/run-all.sh`**

```bash
#!/usr/bin/env bash
# Run all test-*.sh in this directory. Stop on first failure.

set -euo pipefail

cd "$(dirname "$0")"

count=0
for t in test-*.sh; do
    [[ -f "$t" ]] || continue
    [[ -x "$t" ]] || chmod +x "$t"
    bash "$t"
    count=$((count + 1))
done

echo "---"
echo "all $count SKILL tests passed"
```

- [ ] **Step 3: Make executable + smoke-run (should print "all 0 tests passed")**

```bash
chmod +x tests/skill/run-all.sh tests/skill/test-helpers.sh
bash tests/skill/run-all.sh
```
Expected: `all 0 SKILL tests passed`

- [ ] **Step 4: Commit**

```bash
git add tests/skill/test-helpers.sh tests/skill/run-all.sh
git commit -m "test(skill): add bash test harness for SKILL tooling"
```

---

## Task 3: lint-skill.sh — frontmatter check (TDD)

**Files:**
- Create: `tests/skill/test-lint-frontmatter.sh`
- Create: `tools/lint-skill.sh`

- [ ] **Step 1: Write the failing test**

```bash
#!/usr/bin/env bash
# tests/skill/test-lint-frontmatter.sh
set -euo pipefail
source "$(dirname "$0")/test-helpers.sh"

ROOT="$(cd "$(dirname "$0")/../.." && pwd)"
LINT="$ROOT/tools/lint-skill.sh"
tmp=$(mktemp_test_dir)

# Case 1: SKILL.md missing → lint fails with exit 1
mkdir -p "$tmp/docs/skills/cgn-onboard"
assert_exit 1 bash "$LINT" "$tmp/docs/skills/cgn-onboard"

# Case 2: SKILL.md missing frontmatter → fails
cat > "$tmp/docs/skills/cgn-onboard/SKILL.md" <<'EOF'
# cgn-onboard
no frontmatter here
EOF
assert_exit 1 bash "$LINT" "$tmp/docs/skills/cgn-onboard"

# Case 3: SKILL.md missing required key (description) → fails
cat > "$tmp/docs/skills/cgn-onboard/SKILL.md" <<'EOF'
---
name: cgn-onboard
when-to-use: User says install cgn
---
body
EOF
assert_exit 1 bash "$LINT" "$tmp/docs/skills/cgn-onboard"

# Case 4: SKILL.md with all required keys → passes (this check only — orphan-guide check would still fail without guides, but at this step lint only runs frontmatter)
cat > "$tmp/docs/skills/cgn-onboard/SKILL.md" <<'EOF'
---
name: cgn-onboard
description: Personalized installation wizard for code-graph-nexus.
when-to-use: User says install cgn / set up code-graph-nexus.
---
- Jump table:
  - install → guides/01-install.md
EOF
mkdir -p "$tmp/docs/skills/cgn-onboard/guides"
cat > "$tmp/docs/skills/cgn-onboard/guides/01-install.md" <<'EOF'
# install
EOF
assert_exit 0 bash "$LINT" "$tmp/docs/skills/cgn-onboard"

pass
```

- [ ] **Step 2: Run test — expect FAIL (lint-skill.sh doesn't exist yet)**

Run: `bash tests/skill/test-lint-frontmatter.sh`
Expected: error (lint-skill.sh not found)

- [ ] **Step 3: Write minimal `tools/lint-skill.sh` to pass the test**

```bash
#!/usr/bin/env bash
# tools/lint-skill.sh
# T1 structural lint for the cgn-onboard SKILL pack.
# Usage: lint-skill.sh [path-to-skill-root]   # default: docs/skills/cgn-onboard

set -euo pipefail

ROOT="${1:-docs/skills/cgn-onboard}"

fail() { echo "lint FAIL: $1" >&2; exit 1; }

# --- Check 1: SKILL.md exists with valid frontmatter (name, description, when-to-use) ---
skill="$ROOT/SKILL.md"
[[ -f "$skill" ]] || fail "SKILL.md missing at $skill"

# Extract frontmatter block (between leading --- and next ---)
fm=$(awk '/^---$/{c++; next} c==1' "$skill")
[[ -n "$fm" ]] || fail "SKILL.md has no frontmatter"

for key in name description when-to-use; do
    grep -qE "^${key}:" <<<"$fm" || fail "SKILL.md frontmatter missing '$key'"
done

# --- Check 2: guides/*.md have NO frontmatter ---
if [[ -d "$ROOT/guides" ]]; then
    while IFS= read -r -d '' g; do
        if head -1 "$g" | grep -q "^---$"; then
            fail "$g has frontmatter (only SKILL.md should)"
        fi
    done < <(find "$ROOT/guides" -maxdepth 1 -name '*.md' -print0)
fi

echo "lint OK: $ROOT"
```

- [ ] **Step 4: Run test — expect PASS**

```bash
chmod +x tools/lint-skill.sh
bash tests/skill/test-lint-frontmatter.sh
```
Expected: `PASS: test-lint-frontmatter.sh`

- [ ] **Step 5: Commit**

```bash
git add tools/lint-skill.sh tests/skill/test-lint-frontmatter.sh
git commit -m "feat(tools): add lint-skill.sh — frontmatter validity check (T1)"
```

---

## Task 4: lint-skill.sh — guides-have-no-frontmatter check (extend)

The Task 3 implementation already covers this check, but we add a dedicated test to lock it in.

**Files:**
- Create: `tests/skill/test-lint-guides-no-frontmatter.sh`

- [ ] **Step 1: Write the test**

```bash
#!/usr/bin/env bash
# tests/skill/test-lint-guides-no-frontmatter.sh
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
- Jump table:
  - install → guides/01-install.md
EOF

# A guide with frontmatter → lint must fail
cat > "$tmp/docs/skills/cgn-onboard/guides/01-install.md" <<'EOF'
---
name: install
---
# install
EOF
assert_exit 1 bash "$LINT" "$tmp/docs/skills/cgn-onboard"

# Remove frontmatter → passes
cat > "$tmp/docs/skills/cgn-onboard/guides/01-install.md" <<'EOF'
# install
EOF
assert_exit 0 bash "$LINT" "$tmp/docs/skills/cgn-onboard"

pass
```

- [ ] **Step 2: Run — expect PASS (covered by Task 3 impl)**

```bash
bash tests/skill/test-lint-guides-no-frontmatter.sh
```
Expected: `PASS: test-lint-guides-no-frontmatter.sh`

- [ ] **Step 3: Commit**

```bash
git add tests/skill/test-lint-guides-no-frontmatter.sh
git commit -m "test(skill): pin lint behavior — guides forbid frontmatter"
```

---

## Task 5: lint-skill.sh — jump-table link resolution (TDD)

**Files:**
- Create: `tests/skill/test-lint-jump-table.sh`
- Modify: `tools/lint-skill.sh` (append Check 3)

- [ ] **Step 1: Write the failing test**

```bash
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
```

- [ ] **Step 2: Run test — expect FAIL (check not yet implemented)**

Run: `bash tests/skill/test-lint-jump-table.sh`
Expected: case 1 (missing 02) returns 0 instead of 1, test fails on `assert_exit 1`.

- [ ] **Step 3: Append jump-table check to `tools/lint-skill.sh`**

Add at end of `tools/lint-skill.sh`, before the final `echo "lint OK:"`:

```bash
# --- Check 3: every guides/*.md referenced in SKILL.md's jump table actually exists ---
# Match patterns like 'guides/01-install.md' anywhere in SKILL.md.
while IFS= read -r ref; do
    target="$ROOT/$ref"
    [[ -f "$target" ]] || fail "jump-table reference '$ref' resolves to missing file $target"
done < <(grep -oE 'guides/[A-Za-z0-9._-]+\.md' "$skill" | sort -u)
```

- [ ] **Step 4: Run test — expect PASS**

```bash
bash tests/skill/test-lint-jump-table.sh
```
Expected: `PASS: test-lint-jump-table.sh`

- [ ] **Step 5: Commit**

```bash
git add tools/lint-skill.sh tests/skill/test-lint-jump-table.sh
git commit -m "feat(tools): lint-skill — verify jump-table links resolve"
```

---

## Task 6: lint-skill.sh — orphan-guide detection (TDD)

**Files:**
- Create: `tests/skill/test-lint-orphan-guide.sh`
- Modify: `tools/lint-skill.sh` (append Check 4)

- [ ] **Step 1: Write the failing test**

```bash
#!/usr/bin/env bash
# tests/skill/test-lint-orphan-guide.sh
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
```

- [ ] **Step 2: Run test — expect FAIL**

Run: `bash tests/skill/test-lint-orphan-guide.sh`
Expected: orphan case still returns 0; assertion fails.

- [ ] **Step 3: Append orphan-guide check to `tools/lint-skill.sh`**

Add before `echo "lint OK:"`:

```bash
# --- Check 4: every guides/*.md is referenced from SKILL.md (no orphans) ---
if [[ -d "$ROOT/guides" ]]; then
    while IFS= read -r -d '' g; do
        rel="guides/$(basename "$g")"
        grep -qF "$rel" "$skill" || fail "orphan guide: $rel not referenced in SKILL.md"
    done < <(find "$ROOT/guides" -maxdepth 1 -name '*.md' -print0)
fi
```

- [ ] **Step 4: Run test — expect PASS**

```bash
bash tests/skill/test-lint-orphan-guide.sh
```
Expected: `PASS: test-lint-orphan-guide.sh`

- [ ] **Step 5: Commit**

```bash
git add tools/lint-skill.sh tests/skill/test-lint-orphan-guide.sh
git commit -m "feat(tools): lint-skill — detect orphan guides"
```

---

## Task 7: aggregate.sh — SKILL.md + guides → ONBOARDING.md (TDD)

**Files:**
- Create: `tests/skill/test-aggregate.sh`
- Create: `tools/aggregate.sh`

- [ ] **Step 1: Write the failing test**

```bash
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
```

- [ ] **Step 2: Run test — expect FAIL (aggregate.sh missing)**

Run: `bash tests/skill/test-aggregate.sh`
Expected: error (aggregate.sh not found)

- [ ] **Step 3: Write `tools/aggregate.sh`**

```bash
#!/usr/bin/env bash
# tools/aggregate.sh
# Concatenate SKILL.md (frontmatter stripped) + guides/*.md (in lexical order)
# → stdout. Used to build docs/skills/cgn-onboard/ONBOARDING.md for ShareOnboardingGuide.

set -euo pipefail

ROOT="${1:-docs/skills/cgn-onboard}"

skill="$ROOT/SKILL.md"
[[ -f "$skill" ]] || { echo "aggregate: SKILL.md missing at $skill" >&2; exit 1; }

# Strip leading frontmatter (between first '---' and second '---' on their own lines).
awk '
    BEGIN { in_fm = 0; done_fm = 0 }
    /^---$/ {
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
```

- [ ] **Step 4: Run test — expect PASS**

```bash
chmod +x tools/aggregate.sh
bash tests/skill/test-aggregate.sh
```
Expected: `PASS: test-aggregate.sh`

- [ ] **Step 5: Commit**

```bash
git add tools/aggregate.sh tests/skill/test-aggregate.sh
git commit -m "feat(tools): add aggregate.sh — SKILL.md + guides → ONBOARDING.md"
```

---

## Task 8: gen-cli-ref.sh — produces per-version CLI cards (TDD)

**Files:**
- Create: `tests/skill/test-gen-cli-ref.sh`
- Create: `tools/gen-cli-ref.sh`

- [ ] **Step 1: Write the failing test (mock cgn via shell stub)**

```bash
#!/usr/bin/env bash
# tests/skill/test-gen-cli-ref.sh
set -euo pipefail
source "$(dirname "$0")/test-helpers.sh"

ROOT="$(cd "$(dirname "$0")/../.." && pwd)"
GEN="$ROOT/tools/gen-cli-ref.sh"
tmp=$(mktemp_test_dir)

# Mock cgn: prints version and stub --help output for a fixed set of subcommands.
cat > "$tmp/mock-cgn" <<'EOF'
#!/usr/bin/env bash
case "$1" in
  --version) echo "cgn 9.9.9-test" ;;
  find)
    [[ "$2" == "--help" ]] && cat <<HELP
Usage: cgn find <pattern>

Find symbols by exact name (default) or BM25 mode.

Options:
  --mode <MODE>     exact | bm25 | fuzzy
  --repo <PATH>     repo root
HELP
    ;;
  impact)
    [[ "$2" == "--help" ]] && cat <<HELP
Usage: cgn impact [TARGET] [OPTIONS]

Blast radius for a symbol.

Options:
  --direction <DIR>   upstream | downstream | both
  --repo <PATH>       repo root
HELP
    ;;
  admin)
    if [[ "$2" == "index" && "$3" == "--help" ]]; then
      cat <<HELP
Usage: cgn admin index --repo <PATH>

Build the graph index for a repo.

Options:
  --repo <PATH>     repo root
  --force           re-index even if up-to-date
HELP
    fi
    ;;
esac
EOF
chmod +x "$tmp/mock-cgn"

OUT="$tmp/skill/_shared/cli"
mkdir -p "$OUT"
bash "$GEN" "$tmp/mock-cgn" "$OUT"

# Expect: 9.9.9-test version directory with per-command .md files
assert_file_exists "$OUT/9.9.9-test/find.md"
assert_file_exists "$OUT/9.9.9-test/impact.md"
assert_file_exists "$OUT/9.9.9-test/admin-index.md"
assert_grep '^Usage: cgn find' "$OUT/9.9.9-test/find.md"
assert_grep '^Usage: cgn impact' "$OUT/9.9.9-test/impact.md"
assert_grep '^Usage: cgn admin index' "$OUT/9.9.9-test/admin-index.md"

# Manifest.json present and lists the version
assert_file_exists "$OUT/manifest.json"
v=$(jq -r '.latest' "$OUT/manifest.json")
assert_equal "9.9.9-test" "$v" "manifest latest"
n=$(jq -r '.versions | length' "$OUT/manifest.json")
assert_equal "1" "$n" "manifest versions count"

pass
```

- [ ] **Step 2: Run test — expect FAIL**

Run: `bash tests/skill/test-gen-cli-ref.sh`
Expected: error (gen-cli-ref.sh not found)

- [ ] **Step 3: Write `tools/gen-cli-ref.sh`**

```bash
#!/usr/bin/env bash
# tools/gen-cli-ref.sh
# Generate per-version CLI reference cards from `cgn --help`.
#
# Usage:
#   gen-cli-ref.sh [cgn-binary] [output-dir]
#
# Defaults:
#   cgn-binary   = ./target/release/cgn if it exists, else `cgn` from PATH
#   output-dir   = docs/skills/cgn-onboard/_shared/cli
#
# Output layout:
#   <output-dir>/<version>/<cmd>.md            (one per top-level + selected sub-commands)
#   <output-dir>/manifest.json                  {"latest": "<ver>", "versions": [...]}

set -euo pipefail

# Default cgn binary
default_cgn() {
    if [[ -x ./target/release/cgn ]]; then echo "./target/release/cgn"
    elif command -v cgn >/dev/null; then echo "cgn"
    else echo ""; fi
}

CGN="${1:-$(default_cgn)}"
OUT="${2:-docs/skills/cgn-onboard/_shared/cli}"

[[ -n "$CGN" ]] || { echo "gen-cli-ref: no cgn binary found" >&2; exit 1; }

# Version: "cgn 0.1.5" → 0.1.5  (also matches "cgn 9.9.9-test")
VER=$("$CGN" --version | awk '{print $2}')
[[ -n "$VER" ]] || { echo "gen-cli-ref: could not determine cgn version" >&2; exit 1; }

mkdir -p "$OUT/$VER"

# Commands to capture: top-level + curated sub-commands actually used in guides.
# When new guides reference a new command, add it here.
declare -a TOPLEVEL=(find impact inspect cypher routes coverage diff rename)
declare -a SUB=(
    "admin:index"
    "admin:group"
    "admin:mcp"
    "group:find"
    "group:contracts"
    "group:impact"
)

for cmd in "${TOPLEVEL[@]}"; do
    out="$OUT/$VER/$cmd.md"
    "$CGN" "$cmd" --help > "$out" 2>/dev/null || { echo "warn: $cmd has no --help; skipped" >&2; rm -f "$out"; }
done

for entry in "${SUB[@]}"; do
    parent="${entry%%:*}"
    child="${entry##*:}"
    out="$OUT/$VER/${parent}-${child}.md"
    "$CGN" "$parent" "$child" --help > "$out" 2>/dev/null || { echo "warn: $parent $child has no --help; skipped" >&2; rm -f "$out"; }
done

# Build/update manifest.json
manifest="$OUT/manifest.json"
if [[ -f "$manifest" ]]; then
    versions=$(jq -r --arg v "$VER" '(.versions // []) + [$v] | unique' "$manifest")
else
    versions=$(jq -n --arg v "$VER" '[$v]')
fi
jq -n \
    --arg v "$VER" \
    --argjson vs "$versions" \
    --arg ts "$(date -u +%Y-%m-%dT%H:%M:%SZ)" \
    '{latest: $v, versions: $vs, generated_at: $ts}' \
    > "$manifest"

echo "gen-cli-ref: wrote $OUT/$VER/ + manifest"
```

- [ ] **Step 4: Run test — expect PASS**

```bash
chmod +x tools/gen-cli-ref.sh
bash tests/skill/test-gen-cli-ref.sh
```
Expected: `PASS: test-gen-cli-ref.sh`

- [ ] **Step 5: Commit**

```bash
git add tools/gen-cli-ref.sh tests/skill/test-gen-cli-ref.sh
git commit -m "feat(tools): add gen-cli-ref.sh — per-version CLI reference cards"
```

---

## Task 9: test-persona-rules.sh — persona-rule self-consistency tester (TDD on the tester itself)

This task TDDs the **tool** that will later be used in Task 11 to validate the persona rule table.

**Files:**
- Create: `tests/skill/test-persona-rules.sh`  (tests the tool)
- Create: `tools/test-persona-rules.sh`        (the tool)

- [ ] **Step 1: Write the failing test for the tool**

```bash
#!/usr/bin/env bash
# tests/skill/test-persona-rules.sh
set -euo pipefail
source "$(dirname "$0")/test-helpers.sh"

ROOT="$(cd "$(dirname "$0")/../.." && pwd)"
TOOL="$ROOT/tools/test-persona-rules.sh"
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
```

- [ ] **Step 2: Run test — expect FAIL (tool missing)**

Run: `bash tests/skill/test-persona-rules.sh`
Expected: error (test-persona-rules.sh not found)

- [ ] **Step 3: Write `tools/test-persona-rules.sh`**

```bash
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
    # Find rule row matching signal exactly
    if ! grep -qP "^${sig//\//\\/}\t" <<<"$parsed_rules"; then
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
```

- [ ] **Step 4: Run test — expect PASS**

```bash
chmod +x tools/test-persona-rules.sh
bash tests/skill/test-persona-rules.sh
```
Expected: `PASS: test-persona-rules.sh`

- [ ] **Step 5: Commit**

```bash
git add tools/test-persona-rules.sh tests/skill/test-persona-rules.sh
git commit -m "feat(tools): add test-persona-rules.sh — T4 rule/fixture consistency"
```

---

## Task 10: Write persona-inference.md rule table

**Files:**
- Create: `docs/skills/cgn-onboard/_shared/refs/persona-inference.md`

- [ ] **Step 1: Write the file**

```markdown
# Persona Inference

This file defines the rules the SKILL uses to derive a `persona` table from
already-loaded prompts + chat history. The agent **does not fish for additional
user files** — every rule's signal must be observable in the agent's existing
context window.

## Rule table

| Signal | Persona dimension | Default |
|---|---|---|
| CLAUDE.md contains "繁體中文" or "Traditional Chinese" | lang_pref = zh-TW | Wizard speaks 繁中 |
| CLAUDE.md contains "respond in" and "English" | lang_pref = en | Wizard speaks English |
| Chat contains "cargo" or "rust" or "Rust workspace" | install_pref = cargo-binstall | Recommend `cargo binstall code-graph-nexus` |
| Chat contains "brew" or "Homebrew" | install_pref = brew | Recommend `brew install` formula |
| Chat contains "monorepo" or "multi-repo" or "workspace" | scope_pref = group-heavy | Don't skip group phase |
| Chat contains "Cursor" or "cursor" | ide_pref = cursor | mcp phase writes Cursor config |
| Chat contains "Zed" | ide_pref = zed | mcp phase writes Zed config |
| Chat contains "VS Code" or "vscode" or "Continue" | ide_pref = vscode | mcp phase writes VS Code config |
| Chat shows existing Claude Code session | ide_pref = claude-code | mcp phase writes Claude Code config |
| (empty) | lang_pref = unknown | conservative |
| (empty) | install_pref = github-release-tarball | conservative |
| (empty) | scope_pref = single-repo | conservative |
| (empty) | ide_pref = unknown | conservative (ask user explicitly) |

## How the rules are applied

1. At the start of each phase, the agent re-runs the table top-down against
   its current context.
2. The first matching row for each persona dimension wins (specific signals
   beat the `(empty)` fallback).
3. If two specific rules conflict for the same dimension (e.g., chat mentions
   both `cargo` and `brew`), the agent asks the user to disambiguate.
4. The agent never persists this table — it only lives in the in-memory
   working state for this wizard session.

## Adding new rules

When adding a row, you must also add at least one matching fixture to
`tests/skill/persona-fixtures.yaml` and re-run
`tools/test-persona-rules.sh` to confirm consistency.
```

- [ ] **Step 2: Commit**

```bash
git add docs/skills/cgn-onboard/_shared/refs/persona-inference.md
git commit -m "feat(skill): add persona-inference rule table"
```

---

## Task 11: persona-fixtures.yaml + run T4 to gate

**Files:**
- Create: `tests/skill/persona-fixtures.yaml`

- [ ] **Step 1: Write fixtures**

```yaml
# Each fixture: a signal string that must match (case-sensitive) the
# 'Signal' column of a row in persona-inference.md, plus the expected
# persona dimensions for that row.

- signal: 'CLAUDE.md contains "繁體中文" or "Traditional Chinese"'
  expected:
    lang_pref: zh-TW
- signal: 'CLAUDE.md contains "respond in" and "English"'
  expected:
    lang_pref: en
- signal: 'Chat contains "cargo" or "rust" or "Rust workspace"'
  expected:
    install_pref: cargo-binstall
- signal: 'Chat contains "brew" or "Homebrew"'
  expected:
    install_pref: brew
- signal: 'Chat contains "monorepo" or "multi-repo" or "workspace"'
  expected:
    scope_pref: group-heavy
- signal: 'Chat contains "Cursor" or "cursor"'
  expected:
    ide_pref: cursor
- signal: 'Chat contains "Zed"'
  expected:
    ide_pref: zed
- signal: 'Chat contains "VS Code" or "vscode" or "Continue"'
  expected:
    ide_pref: vscode
- signal: 'Chat shows existing Claude Code session'
  expected:
    ide_pref: claude-code
- signal: '(empty)'
  expected:
    lang_pref: unknown
    install_pref: github-release-tarball
    scope_pref: single-repo
    ide_pref: unknown
```

- [ ] **Step 2: Run T4**

```bash
bash tools/test-persona-rules.sh \
    docs/skills/cgn-onboard/_shared/refs/persona-inference.md \
    tests/skill/persona-fixtures.yaml
```
Expected: `persona rules: consistent`

- [ ] **Step 3: Commit**

```bash
git add tests/skill/persona-fixtures.yaml
git commit -m "test(skill): pin persona-inference rules with fixtures"
```

---

## Task 12: env-detect.md — system probes + common-cause table

**Files:**
- Create: `docs/skills/cgn-onboard/_shared/refs/env-detect.md`

- [ ] **Step 1: Write the file**

```markdown
# Environment Detection

Shared snippets used by Phase 01 (install) and Phase 04 (mcp). The agent
runs these probes via its existing shell-execution tool — they are
plain `command -v` / `uname` invocations that produce no side effects.

## Probes

### OS + architecture

```bash
uname -sm
# → "Darwin arm64"  / "Linux x86_64"  / "Linux aarch64"
```

### Package managers (one line each, exit 0 = present)

```bash
command -v cargo
command -v cargo-binstall
command -v brew
command -v curl
command -v wget
```

### IDEs (configuration paths exist?)

```bash
# Claude Code
test -d "$HOME/.claude"

# Cursor (macOS / Linux)
test -d "$HOME/Library/Application Support/Cursor" || test -d "$HOME/.config/Cursor"

# Zed
test -d "$HOME/.config/zed"

# VS Code (with Continue.dev plugin convention)
test -d "$HOME/.vscode" || test -d "$HOME/.continue"
```

### Existing cgn state

```bash
command -v cgn && cgn --version
test -d "$HOME/.cgn"
test -f "$HOME/.cgn/registry.json"
```

## Common-cause table

When a phase's apply step fails, the agent maps the symptom to one of the
hypotheses below before offering retry / change-method / skip.

| Phase | Symptom | Hypotheses (priority order) |
|---|---|---|
| install | `cargo binstall` not found | (1) `cargo` not installed; (2) `cargo-binstall` subcommand missing — suggest `cargo install cargo-binstall` |
| install | binstall fails to fetch tarball | (1) no prebuilt for this target triple → fallback to source build (`cargo install code-graph-nexus`); (2) network / proxy; (3) GitHub release not yet propagated |
| install | `brew install` fails with "no such formula" | tap not added — suggest `brew tap <author>/cgn` |
| install | `curl` of GitHub release returns 404 | version tag mismatch — confirm latest release tag on `gh release list` |
| first-index | `cgn admin index ... → not a git repo` | wrong path / no `.git` directory at repo root |
| first-index | index runs >3 min | large repo / vendored deps not ignored — recommend a `.cgnignore` |
| first-index | `permission denied` writing to `~/.cgn` | recipient's HOME not writable (rare; container env) — suggest `CGN_HOME=$PWD/.cgn` env override |
| group | `cgn admin group add ... → repo not in registry` | repo not yet indexed — re-run phase 02 for that path first |
| mcp | IDE config written but IDE doesn't pick up new tool | (1) IDE not restarted; (2) wrong config path (Cursor has two: `~/.cursor/mcp.json` and per-project `.cursor/mcp.json`); (3) IDE version too old |

## When probes fail

If a probe itself errors (e.g., `uname` not available, `command -v` returns
non-zero unexpectedly), switch to **manual mode**: ask the user directly
for OS / installed package managers, mark `system_probe = manual` in the
persona, and stop attempting silent detection for the rest of the session.
```

- [ ] **Step 2: Commit**

```bash
git add docs/skills/cgn-onboard/_shared/refs/env-detect.md
git commit -m "feat(skill): add env-detect — probes + common-cause table"
```

---

## Task 13: recommendation-templates.md — next-step sentence library

**Files:**
- Create: `docs/skills/cgn-onboard/_shared/refs/recommendation-templates.md`

- [ ] **Step 1: Write the file**

```markdown
# Recommendation Templates

Phase 05 (summary) emits a "next steps" list tailored to the user's
persona. This file is the source library. The agent picks 3–5 lines
matching the persona, never invents new ones outside this list.

## How to read this file

Each section is keyed by persona dimension + value. Within a section,
each `- ` bullet is one recommendation candidate. Use `{<placeholder>}`
for inputs the agent fills in (e.g., `{repo_name}` = the first repo the
user indexed).

## By scope_pref

### scope_pref = group-heavy

- Run `cgn group find <group> "<symbol>" --merge rrf` to do cross-repo BM25 search with RRF fusion.
- Run `cgn group contracts <group>` to inventory routes / queue / RPC contracts across the group.
- Run `cgn group impact <group> --baseline origin/main` to see the full blast radius of a multi-repo change before merging.

### scope_pref = single-repo

- Run `cgn find "<symbol>" --repo .` to look up the canonical definition.
- Run `cgn impact <symbol> --direction upstream --repo .` to see callers.
- Run `cgn routes --repo .` to list HTTP routes mapped to handlers.

## By ide_pref

### ide_pref = claude-code

- Open a Claude Code session in `{repo_name}` and ask "summarize the auth module"; the cgn MCP tools should appear automatically.
- Type `/cgn` in Claude Code to see the cheatsheet skill loaded.

### ide_pref = cursor

- Restart Cursor after the MCP config was written so it picks up the new server.
- Cursor's MCP servers appear in Settings → Features → MCP.

### ide_pref = zed

- Zed's assistant panel will list `cgn_*` tools once the config is reloaded.

### ide_pref = vscode / continue

- Continue.dev reads `~/.continue/config.json`. Restart VS Code to pick up the new MCP server.

## By install_pref (post-install hygiene)

### install_pref = cargo-binstall

- `cargo binstall --self-update` keeps cargo-binstall current so future cgn upgrades stay fast.
- Run `cgn --version` periodically; cargo-binstall does NOT auto-upgrade cgn itself.

### install_pref = brew

- `brew upgrade code-graph-nexus` will pull the latest tagged release.

### install_pref = github-release-tarball

- Bookmark `gh release view --repo <owner>/code-graph-nexus` to spot new releases.

## Universal (always offer 1)

- Bookmark this summary file (`~/.cgn/onboarding-summary.md`) — a future agent session can read it to know what was set up.
- Run `cgn coverage --repo @all --detailed` to inspect registry health.
- Run `cgn admin mcp tools` to list the MCP tools currently exposed.

## When persona is fully `unknown`

Pick 2 from the **Universal** list + the 3 lines under `scope_pref = single-repo`.
```

- [ ] **Step 2: Commit**

```bash
git add docs/skills/cgn-onboard/_shared/refs/recommendation-templates.md
git commit -m "feat(skill): add recommendation-templates — phase 05 sentence library"
```

---

## Task 14: SKILL.md (Layer 1 entry point)

**Files:**
- Create: `docs/skills/cgn-onboard/SKILL.md`

- [ ] **Step 1: Write the file**

```markdown
---
name: cgn-onboard
description: Personalized installation + configuration wizard for code-graph-nexus.
  Walks the user from "no cgn installed" → "cgn ready + indexed + MCP wired
  + recommended next steps".
when-to-use: User says "install cgn" / "set up code-graph-nexus" / "onboard me
  to cgn", OR opened an ONBOARDING share link / pasted a bootstrap URL.
---

# cgn-onboard

You are the cgn onboarding wizard. Your job is to walk a recipient from
"never used code-graph-nexus" to "cgn installed, indexed, grouped (if applicable),
MCP-wired, and with a tailored 'what to try next' list".

## Directives (non-negotiable)

1. **Recommend → user picks accept / change / skip.** Every choice point
   uses this format. Never auto-decide on the user's behalf.
2. **Only use already-loaded prompts + system probes.** Do not fish for
   user files beyond what is already in your context. Probes are limited
   to those listed in `_shared/refs/env-detect.md`.
3. **Never silently retry, never silently switch methods.** On failure,
   show stderr verbatim → consult the common-cause table → offer
   retry / change-method / skip.
4. **Never block on the install download.** When Phase 01 starts a
   background download, advance immediately to Phase 02 to collect
   later phases' choices in parallel. Apply choices in a batch at the
   T6 gate, after the binary is verified.
5. **Background = `cgn` CLI only.** Every applied action goes through
   the `cgn` command. Never write to user files outside of
   `~/.cgn/onboarding-summary.md` (and IDE MCP configs the user has
   explicitly approved in Phase 04).
6. **On new session start:** if `~/.cgn/onboarding-summary.md` exists,
   read it first and offer resume / redo-phase / start-over.

## Persona inference (summary)

Read `_shared/refs/persona-inference.md` for the full rule table. Apply
the rules top-down at the start of each phase to derive:

- `lang_pref` — the language to converse in
- `install_pref` — preferred installer (cargo-binstall / brew / tarball)
- `scope_pref` — `single-repo` vs `group-heavy`
- `ide_pref` — which IDE's MCP config to write

If a dimension stays `unknown` after rule application, fall back to the
`(empty)` row's conservative default and ask the user explicitly when
that dimension is needed by a phase.

## Jump table

Walk the phases in order. At each phase, load the corresponding guide
fully before interacting with the user.

| Intent / state | Next guide |
|---|---|
| Fresh session, no prior summary | guides/01-install.md |
| Install done, no `~/.cgn/registry.json` yet | guides/02-first-index.md |
| Indexed but no group registered | guides/03-group.md (skip if `scope_pref = single-repo`) |
| Indexed + grouped, no MCP config | guides/04-mcp.md |
| All previous phases complete | guides/05-summary.md |
| Resuming an interrupted session | Read summary, ask user which phase to resume |

## Ordering rules

- **Phases 01–04 are choice-collection only.** Each guide records the
  user's decision into an in-memory `config_inventory`. Do not invoke
  `cgn` apply commands inside Phases 02/03/04.
- **Phase 05 is the apply-and-summarize gate.** Wait for the Phase 01
  background download to complete + verify `cgn --version`, then drain
  `config_inventory` into a single batch of `cgn admin` calls in order:
  index → group → mcp. Verify each command succeeds before moving to
  the next.
- **If Phase 01 install failed**, do not proceed to Phase 05's apply
  step. Re-enter Phase 01 with the failure context surfaced from the
  common-cause table.

## CLI flag lookups

When you need exact `cgn <cmd>` flag syntax, read
`_shared/cli/manifest.json`, find the version closest to the user's
local `cgn --version`, and open the corresponding
`_shared/cli/<version>/<cmd>.md` card. If the user's version is not
in the manifest, fall back to running `cgn <cmd> --help` live and use
its output as ground truth — never invent flags.

## Hard "don't" list

- Do not silently retry a failed command.
- Do not switch install methods without user consent.
- Do not modify `~/.zshrc`, `~/.gitconfig`, or any user file not
  explicitly listed under Phase 04 (IDE MCP configs).
- Do not assume future cgn versions have a flag — always verify against
  the CLI reference cards or live `--help`.
```

- [ ] **Step 2: Lint pre-flight (expect fail because guides don't exist yet — that's OK, but let's verify the lint catches it correctly)**

```bash
bash tools/lint-skill.sh docs/skills/cgn-onboard
```
Expected: FAIL — "jump-table reference 'guides/01-install.md' resolves to missing file ...".

This confirms the lint is doing its job; the lint will pass after Tasks 16–20.

- [ ] **Step 3: Commit**

```bash
git add docs/skills/cgn-onboard/SKILL.md
git commit -m "feat(skill): add SKILL.md (Layer 1 entry point)"
```

---

## Task 15: guides/01-install.md (Phase 01)

**Files:**
- Create: `docs/skills/cgn-onboard/guides/01-install.md`

- [ ] **Step 1: Write the guide**

```markdown
# Phase 01 — Install

Goal: produce a verified `cgn` binary on PATH. Start the install in the
background and advance to Phase 02 without waiting.

## Step 1: Probe the system

Run the probes from `_shared/refs/env-detect.md`:

```bash
uname -sm
command -v cargo
command -v cargo-binstall
command -v brew
command -v curl
```

Record results in `config_inventory.install_probe`:

- `os`, `arch` from `uname -sm`
- `has_cargo_binstall`, `has_brew`, `has_curl` booleans
- `cgn_already_installed`: `command -v cgn && cgn --version`

## Step 2: Apply persona × probe → recommendation

| persona.install_pref | probes | Recommendation |
|---|---|---|
| `cargo-binstall` | `has_cargo_binstall = true` | `cargo binstall code-graph-nexus` |
| `cargo-binstall` | `has_cargo_binstall = false`, `has_cargo = true` | `cargo install code-graph-nexus` (slower; source build) + suggest installing cargo-binstall next time |
| `brew` | `has_brew = true` | `brew install <tap>/code-graph-nexus` (substitute the actual tap name from the README) |
| `github-release-tarball` (or fallback) | `has_curl = true` | `curl -L https://github.com/<owner>/code-graph-nexus/releases/latest/download/cgn-<target>.tar.gz \| tar -xz -C ~/bin/` |
| (cgn already installed) | `cgn_already_installed = true` | Verification only; skip download |

## Step 3: Present 3-choice menu

Format (translate to `lang_pref`):

```
[Phase: install / Step 1 of 5]

Based on your persona ({install_pref}, {os}-{arch}), recommendation:

  ✓ Recommended: {recommended_command}
     Why: {reason}

  Alternative A: {alt_a_command}
     Why: {reason_a}

  Alternative B: {alt_b_command}
     Why: {reason_b}

  Skip: I've already installed it (I'll jump to verification)

Reply: accept / a / b / skip
```

Wait for user choice.

## Step 4: Start background install

If choice ≠ skip:

- Spawn the chosen command in the background (use the agent's
  `run_in_background` shell execution mode).
- Do NOT wait for completion. Record the background task ID into
  `config_inventory.install_task_id`.
- Immediately tell the user: "Install running in background. Continuing
  to Phase 02 — your binary will be verified before any `cgn` commands
  are executed."

If choice == skip:

- Run `cgn --version` synchronously and record the output. If it fails,
  loop back to Step 3.

## Step 5: Advance to Phase 02 (do NOT block on install)

Jump to `guides/02-first-index.md`. The Phase 01 background install
keeps running while later phases collect their choices.

## Failure handling

If the install command fails (whether discovered at T6 verification or
earlier), do not auto-retry. Consult the **install** rows in the
common-cause table in `_shared/refs/env-detect.md` and offer the user:

- **Retry** the same command (verbatim)
- **Change method** — re-present the 3-choice menu, excluding the failed option
- **Skip** — mark `config_inventory.install_status = failed` and let
  Phase 05 surface the failure in the final summary

Never silently switch methods.
```

- [ ] **Step 2: Commit**

```bash
git add docs/skills/cgn-onboard/guides/01-install.md
git commit -m "feat(skill): add guides/01-install.md"
```

---

## Task 16: guides/02-first-index.md (Phase 02)

**Files:**
- Create: `docs/skills/cgn-onboard/guides/02-first-index.md`

- [ ] **Step 1: Write the guide**

```markdown
# Phase 02 — First-index

Goal: collect the user's choice of which repo(s) to index. **Do not run
`cgn admin index` here** — only record the choice into
`config_inventory.first_index`.

## Step 1: Detect candidate repos

The agent should NOT scan the filesystem broadly. Instead, infer candidates
from already-loaded context:

- Current working directory (if the chat is happening inside a repo)
- Any repo path the user mentioned in chat
- The repo containing this SKILL pack itself (if recipient is reading
  the file by absolute path)

If no candidate is obvious, ask the user directly: "Which repository
should I index first?"

## Step 2: Apply persona → recommendation

| persona.scope_pref | Recommendation |
|---|---|
| `group-heavy` | Index 2–3 sibling repos in a single batch (user lists them) |
| `single-repo` | Index the current repo only |
| `unknown` | Ask the user; default to "current directory" |

## Step 3: Present 3-choice menu

```
[Phase: first-index / Step 2 of 5]

Based on your persona ({scope_pref}), recommendation:

  ✓ Recommended: index {recommended_repo_list}
     Why: {reason}

  Alternative A: index only the current directory
  Alternative B: skip indexing for now (you can run `cgn admin index` later)

Reply: accept / a / b / skip
```

Wait for user choice.

## Step 4: Record choice (DO NOT execute)

Record into `config_inventory.first_index`:

```yaml
first_index:
  repos: [<chosen list>]
  status: queued     # NOT 'done' — apply happens in Phase 05
```

## Step 5: Advance to Phase 03

Jump to `guides/03-group.md`. If `persona.scope_pref = single-repo` AND
only one repo was selected, **skip directly to** `guides/04-mcp.md`
(no group needed).
```

- [ ] **Step 2: Commit**

```bash
git add docs/skills/cgn-onboard/guides/02-first-index.md
git commit -m "feat(skill): add guides/02-first-index.md"
```

---

## Task 17: guides/03-group.md (Phase 03)

**Files:**
- Create: `docs/skills/cgn-onboard/guides/03-group.md`

- [ ] **Step 1: Write the guide**

```markdown
# Phase 03 — Group

Goal: collect group definitions if the user has multiple repos. **Do not
run `cgn admin group add` here** — record into `config_inventory.groups`.

This phase is **skipped** when:

- `persona.scope_pref = single-repo` AND `first_index.repos` has length 1
- The user explicitly skipped Phase 02

## Step 1: Detect grouping signals

- Were multiple repos selected in Phase 02?
- Do their paths share a common parent (suggests a monorepo / workspace)?
- Did the chat mention "team", "monorepo", "service mesh", or similar?

If none of these → ask the user: "Do you have related repos you'd like
to query as a unit (e.g., a frontend + backend pair, or a microservices
suite)?"

## Step 2: Apply persona → group layout recommendation

| Pattern | Recommendation |
|---|---|
| 2–3 repos sharing parent dir | One group named after the parent dir |
| Frontend + backend mentioned | Two groups (`frontend`, `backend`), each with the relevant repo |
| User-named group | Take the user's name verbatim |

## Step 3: Present 3-choice menu

```
[Phase: group / Step 3 of 5]

Detected grouping signals: {summary}.

  ✓ Recommended: create group "{recommended_name}" with repos {repo_list}
     Why: {reason}

  Alternative A: separate groups per pair (e.g., A, B)
  Alternative B: no groups (you can `cgn admin group add` later)

Reply: accept / a / b / skip
```

Wait for user choice.

## Step 4: Record choice

```yaml
groups:
  - name: {chosen_name}
    repos: [{chosen_repos}]
    status: queued
```

## Step 5: Advance to Phase 04

Jump to `guides/04-mcp.md`.
```

- [ ] **Step 2: Commit**

```bash
git add docs/skills/cgn-onboard/guides/03-group.md
git commit -m "feat(skill): add guides/03-group.md"
```

---

## Task 18: guides/04-mcp.md (Phase 04)

**Files:**
- Create: `docs/skills/cgn-onboard/guides/04-mcp.md`

- [ ] **Step 1: Write the guide**

```markdown
# Phase 04 — MCP

Goal: collect the user's choice of which IDE(s) to wire the cgn MCP
server into. **Do not write the MCP config files here** — record into
`config_inventory.mcp_targets`.

## Step 1: Detect installed IDEs

Run probes from `_shared/refs/env-detect.md` (the IDEs section).
Record into `config_inventory.mcp_probe`:

```yaml
mcp_probe:
  claude_code: true|false
  cursor: true|false
  zed: true|false
  vscode_continue: true|false
```

## Step 2: Apply persona → recommendation

| persona.ide_pref | Recommendation |
|---|---|
| `claude-code` | Write Claude Code MCP config |
| `cursor` | Write Cursor MCP config |
| `zed` | Write Zed MCP config |
| `vscode` | Write Continue.dev config |
| `unknown` | Recommend all IDEs that the probe detected; let user opt out |

For **multiple detected IDEs**, recommend wiring all of them (an MCP
server can serve multiple clients simultaneously).

## Step 3: Present menu

```
[Phase: mcp / Step 4 of 5]

Detected IDEs: {list of detected IDEs}.

  ✓ Recommended: wire MCP into {ide_list}
     Why: {reason}

  Alternative A: only {persona.ide_pref}
  Alternative B: skip MCP setup (you can `cgn admin mcp` later)

Reply: accept / a / b / skip
```

Wait for user choice.

## Step 4: Record choice

```yaml
mcp_targets:
  - ide: claude-code
    config_path: ~/.claude/.mcp.json  # or the per-project equivalent
    status: queued
  - ide: cursor
    config_path: ~/.cursor/mcp.json
    status: queued
  # ... one entry per chosen IDE
```

## Step 5: Confirm explicit write consent

Per Directive 5 in SKILL.md, the wizard MUST NOT write to user files
outside `~/.cgn/onboarding-summary.md` without consent. Show the user
the exact paths the wizard will write to in Phase 05, and ask:

```
I'll write these files in Phase 05:
  - ~/.claude/.mcp.json   (Claude Code)
  - ~/.cursor/mcp.json    (Cursor)

Reply: yes / no / show-content
```

If `show-content`, display the JSON the wizard would write (template
below), then re-ask.

### MCP config template

```json
{
  "mcpServers": {
    "cgn": {
      "command": "cgn",
      "args": ["admin", "mcp", "serve"]
    }
  }
}
```

For IDEs that use a different schema (e.g., Continue.dev uses
`~/.continue/config.json` with a `models` + `mcpServers` mix), look up
the exact format in the IDE's docs at apply time — do not guess.

## Step 6: Advance to Phase 05

Jump to `guides/05-summary.md`.
```

- [ ] **Step 2: Commit**

```bash
git add docs/skills/cgn-onboard/guides/04-mcp.md
git commit -m "feat(skill): add guides/04-mcp.md"
```

---

## Task 19: guides/05-summary.md (Phase 05 — apply + summary)

**Files:**
- Create: `docs/skills/cgn-onboard/guides/05-summary.md`

- [ ] **Step 1: Write the guide**

```markdown
# Phase 05 — Apply + Summary

Goal: at the T6 gate, wait for the background install (Phase 01) to
finish + verify `cgn --version`, then drain `config_inventory` into a
single batch of `cgn admin` calls. Finally, persist the summary and
emit the recommendation list.

## Step 1: T6 gate — wait for install

```bash
# Wait for the background task started in Phase 01.
# Use the agent's mechanism (e.g., poll the task_id until status = done).
cgn --version
```

If `cgn --version` fails:

- Surface stderr to the user.
- Consult `_shared/refs/env-detect.md` common-cause table.
- Re-enter Phase 01's failure-handling branch.
- DO NOT proceed to Step 2 until install is verified.

If `cgn --version` succeeds, parse the version and stash it as
`config_inventory.installed_version`.

## Step 2: Apply first-index

For each repo in `config_inventory.first_index.repos`:

```bash
cgn admin index --repo <repo_path>
```

Use `_shared/cli/<version>/admin-index.md` for exact flag syntax. If
the version is missing, fall back to `cgn admin index --help`.

On success, mark `status: done` in the inventory. On failure, follow
the common-cause table → retry / change-method / skip.

## Step 3: Apply groups

For each group in `config_inventory.groups`:

```bash
cgn admin group add --repo <repo_path> <group_name>
```

(See `_shared/cli/<version>/admin-group.md` for the exact subcommand
shape — `add` vs `create` etc. depending on version.)

## Step 4: Write MCP configs

For each target in `config_inventory.mcp_targets` (user already
consented in Phase 04 Step 5):

- **Idempotency:** if the config file already exists, **merge** the
  `cgn` entry into the existing `mcpServers` object rather than
  overwriting the file. Use `jq` for JSON files.
- **Backup:** before any write, copy the existing file to
  `<path>.bak.<timestamp>`.

```bash
# Example: Claude Code
target=~/.claude/.mcp.json
if [[ -f "$target" ]]; then
    cp "$target" "$target.bak.$(date +%s)"
    jq '.mcpServers.cgn = {"command":"cgn","args":["admin","mcp","serve"]}' \
        "$target" > "$target.tmp" && mv "$target.tmp" "$target"
else
    mkdir -p "$(dirname "$target")"
    cat > "$target" <<'JSON'
{ "mcpServers": { "cgn": { "command": "cgn", "args": ["admin", "mcp", "serve"] } } }
JSON
fi
```

## Step 5: Persist summary

Write `~/.cgn/onboarding-summary.md`:

```markdown
---
wizard_version: 0.1.0
last_phase_completed: 05-summary
installed_version: {version}
persona_snapshot:
  lang_pref: {lang}
  install_pref: {install}
  scope_pref: {scope}
  ide_pref: {ide}
generated_at: {ISO 8601 timestamp}
---

## Phase 01 install
- [x] command run: {command}
- [x] verified: cgn --version → {version}

## Phase 02 first-index
- [x] indexed: {list of repos}

## Phase 03 group
- [x] group "{name}" created with repos: {list}
(or)
- [ ] skipped — single-repo workflow

## Phase 04 mcp
- [x] wrote ~/.claude/.mcp.json (Claude Code)
- [x] wrote ~/.cursor/mcp.json (Cursor)

## Phase 05 summary
- [x] this file
```

Each step from the inventory becomes a `- [x]` or `- [ ] skipped — <reason>`
line. The YAML frontmatter is machine-readable for future resume sessions.

## Step 6: Emit recommendations

Open `_shared/refs/recommendation-templates.md`. Pick 3–5 lines that
match the persona (see the file's own header for the selection rule).
Format as a final chat message:

```
🎉 Onboarding complete.

Indexed: {list}
Groups: {list or "none"}
MCP wired into: {list}
Summary saved to: ~/.cgn/onboarding-summary.md

Try next:
- {recommendation 1}
- {recommendation 2}
- {recommendation 3}

Re-run `cgn admin coverage` anytime to see graph health.
```

The wizard's job ends here.

## Resume case

If `~/.cgn/onboarding-summary.md` already exists at session start
(per SKILL.md directive 6), read its frontmatter. If
`last_phase_completed = 05-summary`, the user already finished —
greet them with the recommendation list only. Otherwise offer:

```
Last session got to Phase {N}. What would you like to do?
- Resume from Phase {N+1}
- Redo a specific phase (which?)
- Start over (this will overwrite the summary)
```
```

- [ ] **Step 2: Commit**

```bash
git add docs/skills/cgn-onboard/guides/05-summary.md
git commit -m "feat(skill): add guides/05-summary.md"
```

---

## Task 20: Run full lint — must pass

- [ ] **Step 1: Run T1 lint**

```bash
bash tools/lint-skill.sh docs/skills/cgn-onboard
```
Expected: `lint OK: docs/skills/cgn-onboard`

- [ ] **Step 2: Run all SKILL tests**

```bash
bash tests/skill/run-all.sh
```
Expected: `all N SKILL tests passed` (N = 7 at this point — Tasks 3, 4, 5, 6, 7, 8, 9).

- [ ] **Step 3: Run T4 against the real rules + fixtures**

```bash
bash tools/test-persona-rules.sh \
    docs/skills/cgn-onboard/_shared/refs/persona-inference.md \
    tests/skill/persona-fixtures.yaml
```
Expected: `persona rules: consistent`

- [ ] **Step 4: Commit if anything was tweaked to make lint pass**

```bash
git status
# If there are changes:
git add -u
git commit -m "fix(skill): lint adjustments after Layer 2 guides written"
```

If status is clean, skip the commit.

---

## Task 21: Generate Layer 3 CLI cards from local cgn

**Files:**
- Create: `docs/skills/cgn-onboard/_shared/cli/<cgn-version>/*.md`
- Create: `docs/skills/cgn-onboard/_shared/cli/manifest.json`

- [ ] **Step 1: Build cgn locally**

```bash
cargo build --release --bin cgn
./target/release/cgn --version
```
Expected: e.g. `cgn 0.1.5`

- [ ] **Step 2: Run the generator**

```bash
bash tools/gen-cli-ref.sh \
    ./target/release/cgn \
    docs/skills/cgn-onboard/_shared/cli
```
Expected: `gen-cli-ref: wrote docs/skills/cgn-onboard/_shared/cli/0.1.5/ + manifest`

- [ ] **Step 3: Inspect output**

```bash
ls docs/skills/cgn-onboard/_shared/cli/0.1.5/
cat docs/skills/cgn-onboard/_shared/cli/manifest.json
```
Expected: A directory with per-command `.md` files (find.md, impact.md, …, admin-index.md, admin-group.md, …), plus a manifest with the version listed.

- [ ] **Step 4: Commit**

```bash
git add docs/skills/cgn-onboard/_shared/cli/
git commit -m "feat(skill): generate CLI reference cards for cgn 0.1.5"
```

---

## Task 22: Run aggregator + commit ONBOARDING.md

**Files:**
- Create: `docs/skills/cgn-onboard/ONBOARDING.md`

- [ ] **Step 1: Run the aggregator**

```bash
bash tools/aggregate.sh docs/skills/cgn-onboard \
    > docs/skills/cgn-onboard/ONBOARDING.md
```

- [ ] **Step 2: Sanity-check the output**

```bash
wc -l docs/skills/cgn-onboard/ONBOARDING.md
# Expect 500–800 lines (SKILL.md ~80 + 5 guides at 100–150 each)

grep -c '<!-- guide:' docs/skills/cgn-onboard/ONBOARDING.md
# Expect 5

head -3 docs/skills/cgn-onboard/ONBOARDING.md
# Must NOT start with '---' (frontmatter stripped)
```

- [ ] **Step 3: T2 round-trip**

```bash
bash tools/aggregate.sh docs/skills/cgn-onboard > /tmp/ONBOARDING.gen.md
diff -u docs/skills/cgn-onboard/ONBOARDING.md /tmp/ONBOARDING.gen.md
```
Expected: no diff, exit 0.

- [ ] **Step 4: Commit**

```bash
git add docs/skills/cgn-onboard/ONBOARDING.md
git commit -m "feat(skill): generate ONBOARDING.md (aggregator build artifact)"
```

---

## Task 23: skill-aggregate.yml — CI for aggregator round-trip

**Files:**
- Create: `.github/workflows/skill-aggregate.yml`

- [ ] **Step 1: Write the workflow**

```yaml
name: SKILL aggregate

on:
  push:
    paths:
      - 'docs/skills/cgn-onboard/SKILL.md'
      - 'docs/skills/cgn-onboard/guides/**'
      - 'tools/aggregate.sh'
      - 'tools/lint-skill.sh'
  pull_request:
    paths:
      - 'docs/skills/cgn-onboard/**'
      - 'tools/aggregate.sh'
      - 'tools/lint-skill.sh'

jobs:
  lint-and-roundtrip:
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v4
      - name: T1 lint
        run: bash tools/lint-skill.sh docs/skills/cgn-onboard
      - name: T2 aggregator round-trip
        run: |
          bash tools/aggregate.sh docs/skills/cgn-onboard > /tmp/ONBOARDING.gen.md
          diff -u docs/skills/cgn-onboard/ONBOARDING.md /tmp/ONBOARDING.gen.md
      - name: T4 persona rules
        run: |
          bash tools/test-persona-rules.sh \
            docs/skills/cgn-onboard/_shared/refs/persona-inference.md \
            tests/skill/persona-fixtures.yaml
      - name: All SKILL tests
        run: bash tests/skill/run-all.sh
```

- [ ] **Step 2: Commit**

```bash
git add .github/workflows/skill-aggregate.yml
git commit -m "ci(skill): add aggregator round-trip + lint workflow"
```

---

## Task 24: skill-cli-ref.yml — CI for CLI-ref regen on release

**Files:**
- Create: `.github/workflows/skill-cli-ref.yml`

- [ ] **Step 1: Write the workflow**

```yaml
name: SKILL CLI ref regen

on:
  push:
    tags:
      - 'v*'
  workflow_dispatch:

jobs:
  regen:
    runs-on: ubuntu-latest
    permissions:
      contents: write
      pull-requests: write
    steps:
      - uses: actions/checkout@v4
      - uses: dtolnay/rust-toolchain@stable
      - name: Build cgn
        run: cargo build --release --bin cgn
      - name: Regenerate CLI ref cards
        run: |
          bash tools/gen-cli-ref.sh \
            ./target/release/cgn \
            docs/skills/cgn-onboard/_shared/cli
      - name: Open PR if anything changed
        uses: peter-evans/create-pull-request@v6
        with:
          branch: skill/cli-ref-regen-${{ github.ref_name }}
          title: "chore(skill): regenerate CLI ref for ${{ github.ref_name }}"
          commit-message: "chore(skill): regenerate CLI ref for ${{ github.ref_name }}"
          body: |
            Auto-regenerated CLI reference cards triggered by tag ${{ github.ref_name }}.
            Review and merge.
```

- [ ] **Step 2: Commit**

```bash
git add .github/workflows/skill-cli-ref.yml
git commit -m "ci(skill): add CLI-ref regen workflow on release tag"
```

---

## Task 25: smoke-playbook.md — manual end-to-end test

**Files:**
- Create: `tests/skill/smoke-playbook.md`

- [ ] **Step 1: Write the playbook**

```markdown
# SKILL Smoke Playbook (T5)

Manual end-to-end test. Run **before each release** that touches anything
under `docs/skills/cgn-onboard/`. Not run in CI (cross-platform install
matrix is out of scope).

## Setup

1. Fresh sandbox: a VM, container, or remote machine where:
   - `cgn` is NOT installed
   - `~/.cgn/` does NOT exist
   - The recipient's editor of choice is installed (Claude Code,
     Cursor, etc.)

## Test cases

### Case A: Cross-agent URL bootstrap (any LLM)

1. Paste into a fresh chat session of the target agent (Cursor / Aider / Gemini CLI / etc.):
   > "Fetch https://raw.githubusercontent.com/<owner>/code-graph-nexus/main/docs/skills/cgn-onboard/SKILL.md and follow it as my onboarding wizard for code-graph-nexus."
2. **Expect:** agent reads SKILL.md, runs probes, emits Phase 01 3-choice menu.
3. Pick `accept`.
4. **Expect:** download starts in background; agent advances to Phase 02 immediately (does not wait).
5. Answer Phase 02–04 prompts.
6. **Expect:** Phase 05 waits for download to verify before running `cgn admin index`.
7. **Verify:**
   - `which cgn` returns a path
   - `~/.cgn/registry.json` exists
   - `~/.cgn/onboarding-summary.md` exists
   - IDE MCP config file written (for the IDE chosen)
   - `cgn find . --repo <indexed-repo>` returns results

### Case B: ShareOnboardingGuide (Claude Code)

1. In Claude Code, from `docs/skills/cgn-onboard/` cwd:
   - Run the `ShareOnboardingGuide` tool with mode `check`.
2. **Expect:** short-code link returned.
3. Open that link in a fresh Claude Code session (different machine or `claude --reset`).
4. Repeat cases A.2 onward.

### Case C: Resume after interruption

1. Run Case A; at Phase 03 say `quit` or close terminal.
2. **Expect:** `~/.cgn/onboarding-summary.md` has frontmatter with
   `last_phase_completed: 02-first-index`.
3. Start a new agent session, paste URL bootstrap.
4. **Expect:** agent reads summary, offers "Resume from Phase 03? Redo a specific phase? Start over?"
5. Pick "Resume" — confirm Phase 03 starts correctly.

### Case D: Install failure path

1. Sabotage: in the test VM, place a `cargo-binstall` shim that exits 1.
2. Run Case A and pick `cargo binstall`.
3. **Expect:** Phase 05's T6 gate detects the failure, surfaces stderr,
   consults common-cause table, offers retry / change-method / skip.

## Pass criteria

- All 4 cases complete the listed "verify" steps.
- No file outside `~/.cgn/onboarding-summary.md` and the IDE MCP configs is modified.
- No silent retries observed.
- Persona inference picks the correct branch based on the test agent's CLAUDE.md (or equivalent).

## When to update this playbook

- A new phase is added → add corresponding verify steps.
- A new distribution outlet is added → new Case.
- A regression is found in production → pin it with a new Case before fixing.
```

- [ ] **Step 2: Commit**

```bash
git add tests/skill/smoke-playbook.md
git commit -m "test(skill): add T5 smoke playbook"
```

---

## Task 26: README — distribution paths section

**Files:**
- Modify: `README.md`

- [ ] **Step 1: Read the current README**

Run: `head -50 README.md`

(The new section goes before any existing "License" / "Contributing" sections, near the end of the main body. If a "Skills" or "Onboarding" section already exists, replace it.)

- [ ] **Step 2: Append the new section**

Append at the appropriate position (just before "License" if it exists, otherwise at the end):

```markdown
## Onboarding skill for AI agents

`docs/skills/cgn-onboard/` ships a layered SKILL pack that turns any LLM
agent into a personalized installation + configuration wizard for `cgn`.

Four ways to use it:

### (a) URL bootstrap — any LLM agent

Paste into your agent chat:

```
Fetch https://raw.githubusercontent.com/<owner>/code-graph-nexus/main/docs/skills/cgn-onboard/SKILL.md
and follow it as my onboarding wizard for code-graph-nexus.
```

The agent reads `SKILL.md`, probes your system, and walks you through
install + first-index + group + MCP setup, tailored to the prompts /
preferences already in its context.

### (b) ShareOnboardingGuide — Claude Code (lowest friction)

From this repo's checkout:

```bash
cd docs/skills/cgn-onboard
# In Claude Code, invoke the ShareOnboardingGuide tool
# It uploads ONBOARDING.md and returns a short link
```

Send that link to a teammate. They open it in their Claude Code session
and the wizard auto-loads.

### (c) Plugin install — Claude Code (advanced)

```bash
# Pull only the SKILL pack — avoids downloading the whole Rust workspace
git clone --depth=1 --filter=blob:none --sparse \
    https://github.com/<owner>/code-graph-nexus ~/.claude/plugins/cgn-onboard-src
cd ~/.claude/plugins/cgn-onboard-src
git sparse-checkout set docs/skills/cgn-onboard
ln -s docs/skills/cgn-onboard ~/.claude/skills/cgn-onboard
```

### (d) Manual git clone — any agent

Same `--depth=1 --filter=blob:none --sparse` + `git sparse-checkout set
docs/skills/cgn-onboard` recipe, dropped into your agent's skill / rule
directory.

### How the SKILL is structured (for SKILL authors)

- `SKILL.md` — Layer 1 entry, frontmatter + jump table + directives.
- `guides/01-…05-…md` — Layer 2 phase guides.
- `_shared/cli/<version>/<cmd>.md` — Layer 3 auto-generated CLI reference (one set per cgn version).
- `_shared/refs/{persona-inference,env-detect,recommendation-templates}.md` — Layer 3 hand-written rule tables.
- `ONBOARDING.md` — build artifact (aggregator output). Do not hand-edit.

Tooling lives at `tools/{lint-skill,aggregate,gen-cli-ref,test-persona-rules}.sh`.
CI in `.github/workflows/skill-{aggregate,cli-ref}.yml`.
Spec: `docs/superpowers/specs/2026-05-18-cgn-onboard-skill-design.md`.
```

- [ ] **Step 3: Commit**

```bash
git add README.md
git commit -m "docs(readme): document 4 distribution paths for cgn-onboard SKILL"
```

---

## Task 27: Final verification + push

- [ ] **Step 1: Final lint + tests + aggregator round-trip**

```bash
bash tools/lint-skill.sh docs/skills/cgn-onboard
bash tests/skill/run-all.sh
bash tools/test-persona-rules.sh \
    docs/skills/cgn-onboard/_shared/refs/persona-inference.md \
    tests/skill/persona-fixtures.yaml
bash tools/aggregate.sh docs/skills/cgn-onboard > /tmp/ONBOARDING.gen.md
diff -u docs/skills/cgn-onboard/ONBOARDING.md /tmp/ONBOARDING.gen.md
```
All must exit 0.

- [ ] **Step 2: Inspect file count + sizes**

```bash
find docs/skills/cgn-onboard tools tests/skill .github/workflows \
    -type f -name '*.md' -o -name '*.sh' -o -name '*.yml' -o -name '*.json' -o -name '*.yaml' \
    | xargs wc -l | tail -1
```
Expected: total roughly 1500–2500 lines (5 guides ~150 each, SKILL.md ~80, refs ~300 total, tools ~400 total, tests ~300 total, ONBOARDING.md ~800).

- [ ] **Step 3: Confirm no orphan / placeholder content**

```bash
grep -rE 'TODO|TBD|FIXME|XXX' docs/skills/cgn-onboard tools tests/skill .github/workflows/skill-*.yml
```
Expected: no output.

- [ ] **Step 4: Push branch + open PR**

```bash
git push -u origin HEAD
gh pr create --title "feat(skill): cgn-onboard SKILL pack — 3 layers + 4 distribution outlets" \
    --body "$(cat <<'EOF'
## Summary

Implementation of the cgn-onboard SKILL pack per spec
`docs/superpowers/specs/2026-05-18-cgn-onboard-skill-design.md`.

- Layered structure under `docs/skills/cgn-onboard/`: SKILL.md (Layer 1)
  + 5 phase guides (Layer 2) + `_shared/{cli,refs}` (Layer 3).
- Bash tooling under `tools/`: `lint-skill.sh`, `aggregate.sh`,
  `gen-cli-ref.sh`, `test-persona-rules.sh`.
- Tests under `tests/skill/` (8 bash integration tests + persona
  fixtures + manual smoke playbook).
- CI in `.github/workflows/skill-{aggregate,cli-ref}.yml`.

## Test plan

- [ ] CI green (skill-aggregate workflow)
- [ ] Manual smoke playbook Case A on a clean VM
- [ ] `gh pr ready` once green

🤖 Generated with [Claude Code](https://claude.com/claude-code)
EOF
)"
```

---

## Cross-task consistency notes

- `config_inventory` is the same in-memory structure across all phases.
  Schema (informal — agents track this in their working memory, not a file):

  ```yaml
  config_inventory:
    install_probe: { os, arch, has_cargo_binstall, has_brew, has_curl, cgn_already_installed }
    install_task_id: <background task id from agent's run_in_background>
    install_status: queued | running | done | failed
    first_index: { repos: [...], status }
    groups: [{ name, repos, status }]
    mcp_targets: [{ ide, config_path, status }]
    mcp_probe: { claude_code, cursor, zed, vscode_continue }
    installed_version: <semver>
  ```

- File-naming convention used throughout:
  - SKILL pack: `docs/skills/cgn-onboard/`
  - Tools: `tools/<verb>-<noun>.sh`
  - Tests: `tests/skill/test-<tool-or-feature>.sh`
  - CI: `.github/workflows/skill-<verb>.yml`

- The `<owner>` placeholder in README and smoke-playbook URLs is the
  GitHub org / user of the code-graph-nexus repo. Replace with the actual
  value at PR time (not parametric — just edit before merging).
