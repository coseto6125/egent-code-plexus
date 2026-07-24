#!/usr/bin/env bash
# tests/skill/test-gen-cli-ref.sh
set -euo pipefail
source "$(dirname "$0")/test-helpers.sh"

ROOT="$(cd "$(dirname "$0")/../.." && pwd)"
GEN="$ROOT/scripts/skill/gen-cli-ref.sh"
tmp=$(mktemp_test_dir)

# Mock ecp: prints version and stub --help output for a fixed set of subcommands.
mkdir -p "$tmp"
cat > "$tmp/mock-ecp" <<'EOF'
#!/usr/bin/env bash
case "$1" in
  --version) echo "ecp 9.9.9-test" ;;
  find)
    [[ "$2" == "--help" ]] && cat <<HELP
Usage: ecp find <pattern>

Find symbols by exact name (default) or BM25 mode.

Options:
  --mode <MODE>     exact | bm25 | fuzzy
  --repo <PATH>     repo root
HELP
    ;;
  impact)
    [[ "$2" == "--help" ]] && cat <<HELP
Usage: ecp impact [TARGET] [OPTIONS]

Blast radius for a symbol.

Options:
  --direction <DIR>   upstream | downstream | both
  --repo <PATH>       repo root
HELP
    ;;
  admin)
    if [[ "$2" == "index" && "$3" == "--help" ]]; then
      cat <<HELP
Usage: ecp admin index --repo <PATH>

Build the graph index for a repo.

Options:
  --repo <PATH>     repo root
  --force           re-index even if up-to-date
HELP
    fi
    ;;
esac
EOF
chmod +x "$tmp/mock-ecp"

OUT="$tmp/skill/_shared/cli"
mkdir -p "$OUT"
bash "$GEN" "$tmp/mock-ecp" "$OUT"

# Expect: flat per-command .md files directly under the output dir.
# Versioned subdirs + manifest.json were eliminated in PR #189 — only the
# latest references live under _shared/cli/ (what SKILL.md links point at).
assert_file_exists "$OUT/find.md"
assert_file_exists "$OUT/impact.md"
assert_file_exists "$OUT/admin-index.md"
assert_grep '^Usage: ecp find' "$OUT/find.md"
assert_grep '^Usage: ecp impact' "$OUT/impact.md"
assert_grep '^Usage: ecp admin index' "$OUT/admin-index.md"

# Pin the flat layout: no per-version directory, no manifest.
if [[ -d "$OUT/9.9.9-test" ]]; then
    echo "FAIL: unexpected versioned dir $OUT/9.9.9-test (flat layout since PR #189)" >&2; exit 1
fi
if [[ -f "$OUT/manifest.json" ]]; then
    echo "FAIL: unexpected manifest.json (removed in PR #189)" >&2; exit 1
fi

pass
