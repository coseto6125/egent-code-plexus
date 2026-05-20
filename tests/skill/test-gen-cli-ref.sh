#!/usr/bin/env bash
# tests/skill/test-gen-cli-ref.sh
set -euo pipefail
source "$(dirname "$0")/test-helpers.sh"

ROOT="$(cd "$(dirname "$0")/../.." && pwd)"
GEN="$ROOT/tools/gen-cli-ref.sh"
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

# Expect: 9.9.9-test version directory with per-command .md files
assert_file_exists "$OUT/9.9.9-test/find.md"
assert_file_exists "$OUT/9.9.9-test/impact.md"
assert_file_exists "$OUT/9.9.9-test/admin-index.md"
assert_grep '^Usage: ecp find' "$OUT/9.9.9-test/find.md"
assert_grep '^Usage: ecp impact' "$OUT/9.9.9-test/impact.md"
assert_grep '^Usage: ecp admin index' "$OUT/9.9.9-test/admin-index.md"

# Manifest.json present and lists the version
assert_file_exists "$OUT/manifest.json"
v=$(jq -r '.latest' "$OUT/manifest.json")
assert_equal "9.9.9-test" "$v" "manifest latest"
n=$(jq -r '.versions | length' "$OUT/manifest.json")
assert_equal "1" "$n" "manifest versions count"

pass
