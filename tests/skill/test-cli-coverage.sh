#!/usr/bin/env bash
# Tests scripts/skill/check-cli-coverage.sh against fixture trees, so the suite
# stays green when the real docs change and red when the gate's logic breaks.

source "$(dirname "$0")/test-helpers.sh"

SCRIPT="$(cd "$(dirname "$0")/../.." && pwd)/scripts/skill/check-cli-coverage.sh"
assert_file_exists "$SCRIPT"

tmp=$(mktemp_test_dir)

# A repo the script accepts: the clap enum it reads, and nothing else.
mkdir -p "$tmp/crates/ecp-cli/src"
cat > "$tmp/crates/ecp-cli/src/cli.rs" <<'RS'
pub enum Commands {
    /// Locate symbols by exact name
    Find(commands::find::FindArgs),
    /// Symbol blast radius
    Impact(commands::impact::ImpactArgs),
    /// Detect drift between consumers and routes
    ShapeCheck(commands::shape_check::ShapeCheckArgs),
    /// Heuristic detectors
    Heuristics(commands::heuristics::HeuristicsArgs),
    /// Deprecated: use `ecp heuristics event-mirrors`
    #[command(hide = true, long_about = "Deprecated: use `ecp heuristics event-mirrors`")]
    FindEventMirrors(commands::find_event_mirrors::FindEventMirrorsArgs),
    /// Multi-repo group contract extraction
    #[command(hide = true)]
    Group {
        #[command(subcommand)]
        cmd: GroupCmd,
    },
    /// Usage dashboard over telemetry
    Usage(commands::usage::UsageArgs),
}
RS

# --- The enum parse is the gate's ground truth, so pin it first ---
table=$(cd "$tmp" && bash "$SCRIPT" --list)
assert_equal "find	visible	live" "$(grep '^find	' <<<"$table")" "find is a live visible verb"
assert_equal "group	hidden	live" "$(grep '^group	' <<<"$table")" "hide = true on a brace variant"
assert_equal "shape-check	visible	live" "$(grep '^shape-check	' <<<"$table")" "PascalCase to kebab-case"
assert_equal "find-event-mirrors	hidden	deprecated" "$(grep '^find-event-mirrors	' <<<"$table")" "a Deprecated doc comment"

# The required set: visible, live, minus the operator-surface exemptions.
# `usage` is exempt, `group` is hidden, so neither may be demanded of a pack.
mkdir -p "$tmp/pack"
cat > "$tmp/pack/SKILL.md" <<'MD'
Use `ecp find` to locate a symbol, `ecp impact` for its callers,
`ecp shape-check` for response drift, and `ecp heuristics saga` for pairings.
MD
assert_exit 0 bash -c "cd '$tmp' && bash '$SCRIPT' pack"

# --- A missing verb goes red, and the message names it ---
cat > "$tmp/pack/SKILL.md" <<'MD'
Use `ecp find` to locate a symbol, `ecp impact` for its callers,
and `ecp shape-check` for response drift.
MD
out="$tmp/out.txt"
assert_exit 1 bash -c "cd '$tmp' && bash '$SCRIPT' pack 2>'$out'"
assert_grep 'never names .ecp heuristics' "$out"

# --- A deprecated verb goes red even when every required verb is present ---
cat > "$tmp/pack/SKILL.md" <<'MD'
Use `ecp find`, `ecp impact`, `ecp shape-check`, `ecp heuristics saga`,
and for event topics `ecp find-event-mirrors`.
MD
assert_exit 1 bash -c "cd '$tmp' && bash '$SCRIPT' pack 2>'$out'"
assert_grep 'names .ecp find-event-mirrors., which the CLI answers with a Deprecated' "$out"

# --- `ecp find` is not satisfied by `ecp find-event-mirrors` ---
# The substring form of this check credited the pack with documenting `ecp find`.
cat > "$tmp/pack/SKILL.md" <<'MD'
Use `ecp find-event-mirrors`, `ecp impact`, `ecp shape-check`, `ecp heuristics saga`.
MD
assert_exit 1 bash -c "cd '$tmp' && bash '$SCRIPT' pack 2>'$out'"
assert_grep 'never names .ecp find.$' "$out"

# --- A pack that ships reference cards must have one per verb, none retired ---
cat > "$tmp/pack/SKILL.md" <<'MD'
Use `ecp find`, `ecp impact`, `ecp shape-check`, `ecp heuristics saga`.
MD
mkdir -p "$tmp/pack/_shared/cli"
for v in find impact shape-check heuristics; do echo "# ecp $v" > "$tmp/pack/_shared/cli/$v.md"; done
assert_exit 0 bash -c "cd '$tmp' && bash '$SCRIPT' pack"

rm "$tmp/pack/_shared/cli/heuristics.md"
assert_exit 1 bash -c "cd '$tmp' && bash '$SCRIPT' pack 2>'$out'"
assert_grep 'heuristics.md missing' "$out"

echo "# ecp heuristics" > "$tmp/pack/_shared/cli/heuristics.md"
echo "# stale" > "$tmp/pack/_shared/cli/find-event-mirrors.md"
assert_exit 1 bash -c "cd '$tmp' && bash '$SCRIPT' pack 2>'$out'"
assert_grep 'a deprecated verb' "$out"

# --- A single markdown file is a pack too (the Gemini shape) ---
rm -rf "$tmp/pack"
cat > "$tmp/GEMINI.md" <<'MD'
| Find a symbol | `ecp find "name"` |
| Blast radius | `ecp impact X` |
| Response drift | `ecp shape-check` |
| Pairings | `ecp heuristics saga` |
MD
assert_exit 0 bash -c "cd '$tmp' && bash '$SCRIPT' GEMINI.md"

# --- main.rs cross-check: a verb the enum parse drops must not pass silently ---
cat > "$tmp/crates/ecp-cli/src/main.rs" <<'RS'
fn verb(cmd: &Commands) -> &'static str {
    match cmd {
        Commands::Find(_) => "find",
        Commands::Impact(_) => "impact",
        Commands::ShapeCheck(_) => "shape-check",
        Commands::Heuristics(_) => "heuristics",
        Commands::FindEventMirrors(_) => "find-event-mirrors",
        Commands::Group { .. } => "group",
        Commands::Usage(_) => "usage",
    }
}
RS
assert_exit 0 bash -c "cd '$tmp' && bash '$SCRIPT' GEMINI.md"

# main.rs knows a verb the enum parse did not produce.
echo '        Commands::Path(_) => "path",' >> "$tmp/crates/ecp-cli/src/main.rs"
assert_exit 1 bash -c "cd '$tmp' && bash '$SCRIPT' GEMINI.md 2>'$out'"
assert_grep 'disagree on the verb list' "$out"
assert_grep '^  > path$' "$out"
sed -i '$d' "$tmp/crates/ecp-cli/src/main.rs"
assert_exit 0 bash -c "cd '$tmp' && bash '$SCRIPT' GEMINI.md"

# --- Guardrails on the script's own preconditions ---
assert_exit 1 bash -c "cd '$(dirname "$tmp")' && bash '$SCRIPT' 2>/dev/null"
assert_exit 1 bash -c "cd '$tmp' && bash '$SCRIPT' no-such-pack 2>/dev/null"

pass
