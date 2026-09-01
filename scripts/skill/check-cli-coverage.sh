#!/usr/bin/env bash
# scripts/skill/check-cli-coverage.sh
# Every skill pack caches the CLI verb list. This keeps the cache honest.
#
# Usage: check-cli-coverage.sh [pack ...]     # default: all three packs
#        check-cli-coverage.sh --list         # print the verb table and exit
#
# A pack is a directory (SKILL.md + ECP.md + guides/ + _shared/cli/) or a single
# markdown file. The authority is the clap enum in crates/ecp-cli/src/cli.rs, not
# a built binary: CI gates docs-only PRs, which never compile ecp, and a released
# binary would not know about a verb the PR itself adds.

set -euo pipefail

CLI_RS="crates/ecp-cli/src/cli.rs"
# `main.rs` maps every variant to its verb name in a match the compiler keeps
# exhaustive. Cross-checking the two closes this gate's one silent failure: an
# enum shape the awk below stops recognising would otherwise drop a verb and
# leave the gate vacuously green.
MAIN_RS="crates/ecp-cli/src/main.rs"

# Verbs that answer no structural question, so a routing doc may leave them out.
# `peers` is session collaboration, `usage` is a telemetry dashboard, and
# `uninstall` removes host integrations. None of them query the graph.
EXEMPT="uninstall peers usage"

DEFAULT_PACKS=(
    docs/skills/ecp
    skill_sample/codex/ecp
    skill_sample/gemini/GEMINI.md
)

[[ -f "$CLI_RS" ]] || { echo "coverage: run from the repo root ($CLI_RS not found)" >&2; exit 1; }

# Read the clap enum: one `<verb>\t<visible|hidden>\t<live|deprecated>` per line.
# A `#[command(hide = true)]` marks a verb hidden; a doc comment opening with
# `Deprecated:` marks it retired.
read_verbs() {
    awk '
/^pub enum Commands \{/ { in_enum = 1; next }
!in_enum { next }
/^\}/ { exit }

/^[[:space:]]*\/\/\// {
    if (doc == "") { line = $0; sub(/^[[:space:]]*\/\/\/[[:space:]]?/, "", line); doc = line }
    next
}
/^[[:space:]]*#\[/ { attr = attr $0; if ($0 ~ /\)\]|\]$/) next; in_attr = 1; next }
in_attr { attr = attr $0; if ($0 ~ /\)\]|^[[:space:]]*\]/) in_attr = 0; next }

/^[[:space:]]{4}[A-Z][A-Za-z0-9]*[[:space:]]*[({,]/ {
    name = $1
    sub(/[[:space:]]*[({,].*$/, "", name)
    kebab = ""
    for (i = 1; i <= length(name); i++) {
        c = substr(name, i, 1)
        if (c ~ /[A-Z]/) { if (i > 1) kebab = kebab "-"; c = tolower(c) }
        kebab = kebab c
    }
    hidden = (attr ~ /hide[[:space:]]*=[[:space:]]*true/) ? "hidden" : "visible"
    state = (doc ~ /^Deprecated:/) ? "deprecated" : "live"
    print kebab "\t" hidden "\t" state
    doc = ""; attr = ""
}
' "$CLI_RS"
}

VERBS=$(read_verbs)
[[ -n "$VERBS" ]] || { echo "coverage: parsed no verbs from $CLI_RS — the enum shape changed" >&2; exit 1; }

if [[ -f "$MAIN_RS" ]]; then
    declared=$(grep -oE '^[[:space:]]*Commands::[A-Za-z0-9]+[^=]*=> "[a-z-]+"' "$MAIN_RS" |
               grep -oE '"[a-z-]+"$' | tr -d '"' | sort -u)
    parsed=$(cut -f1 <<<"$VERBS" | sort -u)
    if [[ -n "$declared" && "$declared" != "$parsed" ]]; then
        echo "coverage: $CLI_RS and $MAIN_RS disagree on the verb list — the enum parse is stale" >&2
        diff <(echo "$parsed") <(echo "$declared") | sed 's/^/  /' >&2
        exit 1
    fi
fi

if [[ "${1:-}" == "--list" ]]; then
    printf '%s\n' "$VERBS"
    exit 0
fi

required=$(awk -F'\t' '$2 == "visible" && $3 == "live" { print $1 }' <<<"$VERBS")
for e in $EXEMPT; do required=$(grep -vx "$e" <<<"$required" || true); done
retired=$(awk -F'\t' '$3 == "deprecated" { print $1 }' <<<"$VERBS")

fail=0
note() { echo "coverage: $1" >&2; fail=1; }

# `ecp find` must not be satisfied by `ecp find-event-mirrors`, so every verb
# match ends on a word boundary rather than a bare substring.
named() { grep -qE "ecp $1([^a-z-]|\$)"; }

check_pack() {
    local pack="$1" docs
    if [[ -f "$pack" ]]; then
        docs="$pack"
    elif [[ -d "$pack" ]]; then
        docs=$(find "$pack" -name '*.md' -type f | sort)
    else
        note "$pack does not exist"
        return
    fi
    [[ -n "$docs" ]] || { note "$pack holds no markdown"; return; }

    local body
    # $docs is a newline-separated file list, so the split is the point.
    # shellcheck disable=SC2086
    body=$(cat $docs)

    for v in $required; do
        named "$v" <<<"$body" || note "$pack never names \`ecp $v\`"
    done

    for v in $retired; do
        local f
        for f in $docs; do
            named "$v" <"$f" || continue
            note "$f names \`ecp $v\`, which the CLI answers with a Deprecated notice"
            break
        done
    done

    # Per-command reference cards, where the pack ships them.
    local refs="$pack/_shared/cli"
    [[ -d "$refs" ]] || return 0
    for v in $required; do
        [[ -f "$refs/$v.md" ]] || note "$refs/$v.md missing for \`ecp $v\`"
    done
    for f in "$refs"/*.md; do
        local v
        v=$(basename "$f" .md)
        grep -qx "$v" <<<"$retired" && note "$f documents \`ecp $v\`, a deprecated verb"
    done
    # The loop above ends on a `grep` that misses for every non-deprecated ref,
    # and a non-zero return here would end the whole run under `set -e`.
    return 0
}

packs=("$@")
(( ${#packs[@]} )) || packs=("${DEFAULT_PACKS[@]}")
for p in "${packs[@]}"; do check_pack "$p"; done

(( fail )) && exit 1
echo "coverage OK: $(wc -l <<<"$required") verbs, $(( ${#packs[@]} )) packs"
