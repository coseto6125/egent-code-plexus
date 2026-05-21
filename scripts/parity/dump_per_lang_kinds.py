#!/usr/bin/env python3
"""Per-(language, kind) symbol count dump: ecp vs reference gitnexus.

Run from egent-code-plexus repo root. Assumes `.sample_repo/` contains 14 mainstream
language subdirectories already indexed by both binaries.
"""

from __future__ import annotations

import json
import re
import subprocess
import sys

REPO = "/home/enor/code-graph-nexus/.sample_repo"
REPO_REF = "/home/enor/gitnexus-rs/.sample_repo"  # gitnexus-indexed path (same corpus)
LANGS = [
    "TypeScript",
    "JavaScript",
    "Python",
    "Java",
    "Kotlin",
    "CSharp",
    "Go",
    "Rust",
    "PHP",
    "Ruby",
    "Swift",
    "C",
    "Cpp",
    "Dart",
]

# Per-lang source file extensions. Used to filter out cross-category
# pollution (e.g., GitHub Actions workflow `.yml` files inside `Rust/`
# add 62 Class nodes to the Rust count, inflating the apparent delta;
# Kotlin `build.gradle.kts` inside `Java/` adds Variable nodes).
# The filter is applied to BOTH ecp and ref-gitnexus queries so the
# per-lang totals reflect only nodes from real source files in that
# language's grammar.
EXTS: dict[str, list[str]] = {
    "TypeScript": [".ts", ".tsx"],
    "JavaScript": [".js", ".jsx", ".mjs", ".cjs"],
    "Python": [".py", ".pyi"],
    "Java": [".java"],
    "Kotlin": [".kt", ".kts"],
    "CSharp": [".cs", ".csx"],
    "Go": [".go"],
    "Rust": [".rs"],
    "PHP": [".php", ".phtml"],
    "Ruby": [".rb"],
    "Swift": [".swift"],
    "C": [".c", ".h"],
    "Cpp": [".cpp", ".cc", ".cxx", ".hpp", ".hh", ".hxx", ".cppm"],
    "Dart": [".dart"],
}


def _ext_filter_clause(lang: str) -> str:
    """Cypher `AND (...)` clause limiting nodes to real source-file
    extensions for `lang`. Empty string if lang has no entry."""
    exts = EXTS.get(lang)
    if not exts:
        return ""
    ored = " OR ".join(f"n.filePath ENDS WITH '{e}'" for e in exts)
    return f" AND ({ored})"


ROW_RE = re.compile(r"\|\s*([A-Za-z][A-Za-z0-9_]*)\s*\|\s*(\d+)\s*\|")


def run(cmd: list[str]) -> str:
    r = subprocess.run(cmd, capture_output=True, text=True)
    if r.returncode != 0:
        print(f"!! {' '.join(cmd[:3])} ... → rc={r.returncode}", file=sys.stderr)
        print(r.stderr[:400], file=sys.stderr)
    return r.stdout


def parse_rs(out: str) -> dict[str, int]:
    """ecp cypher --format json → {kind: count}."""
    try:
        obj = json.loads(out)
    except json.JSONDecodeError:
        return {}
    return {row[0]: row[1] for row in obj.get("rows", [])}


def parse_ref(out: str) -> dict[str, int]:
    """gitnexus cypher → either JSON {markdown:"...", row_count:N} or YAML-ish.

    Markdown table parsed via regex; labels appear as `["Function"]`-style
    array strings in some queries, plain string in others.
    """
    # Try JSON first
    md = ""
    try:
        obj = json.loads(out)
        md = obj.get("markdown", "")
    except json.JSONDecodeError:
        m = re.search(r'markdown:\s*"(.*?)"\s*(?:row_count|$)', out, re.DOTALL)
        if m:
            md = m.group(1)
    if not md:
        return {}
    # Convert escaped \n back to real newlines for matching, also handle bracketed labels
    md = md.replace("\\n", "\n")
    counts: dict[str, int] = {}
    for line in md.split("\n"):
        # Strip bracket/quote noise: ["Function"] → Function
        cleaned = line.replace("[", "").replace("]", "").replace('"', "")
        m = ROW_RE.match(cleaned.strip())
        if m:
            kind, cnt = m.group(1), int(m.group(2))
            if kind in ("kind", "l", "labels", "n", "c"):
                continue
            counts[kind] = counts.get(kind, 0) + cnt
    return counts


def cypher_rs_per_lang(lang: str) -> dict[str, int]:
    q = (
        f"MATCH (n) WHERE n.filePath STARTS WITH '{lang}/'"
        f"{_ext_filter_clause(lang)} "
        "RETURN n.kind AS kind, count(*) AS c ORDER BY c DESC"
    )
    return parse_rs(run(["ecp", "cypher", "--repo", REPO, q, "--format", "json"]))


def cypher_ref_per_lang(lang: str) -> dict[str, int]:
    q = (
        f"MATCH (n) WHERE n.filePath STARTS WITH '{lang}/'"
        f"{_ext_filter_clause(lang)} "
        "RETURN labels(n) AS l, count(*) AS c ORDER BY c DESC LIMIT 50"
    )
    return parse_ref(run(["gitnexus", "cypher", "--repo", REPO_REF, q]))


def print_lang_table(lang: str, rs: dict[str, int], ref: dict[str, int]) -> None:
    kinds = sorted(set(rs) | set(ref))
    rs_total = sum(rs.values())
    ref_total = sum(ref.values())
    print(
        f"\n=== {lang}  (rs total {rs_total}, ref total {ref_total}, delta {rs_total - ref_total:+}) ==="
    )
    print(f"  {'kind':<15} {'rs':>8} {'ref':>8} {'delta':>8}  flag")
    for k in kinds:
        r, x = rs.get(k, 0), ref.get(k, 0)
        d = r - x
        flag = ""
        if x == 0 and r > 0:
            flag = "[rs-only]"
        elif r == 0 and x > 0:
            flag = "[ref-only]  ← gap"
        elif x > 0 and (r / max(x, 1)) < 0.7:
            flag = "[under]     ← gap"
        elif r > 0 and (x / max(r, 1)) < 0.7:
            flag = "[over]      ← noise?"
        print(f"  {k:<15} {r:>8} {x:>8} {d:>+8}  {flag}")


def sample_kind(lang: str, kind: str, n: int = 8, skip: int = 0) -> list[tuple[str, str]]:
    """Return up to `n` (name, filePath) pairs for (lang, kind) from rs index."""
    q = (
        f"MATCH (n) WHERE n.kind='{kind}' AND n.filePath STARTS WITH '{lang}/' "
        f"RETURN n.name, n.filePath SKIP {skip} LIMIT {n}"
    )
    out = run(["ecp", "cypher", "--repo", REPO, q, "--format", "json"])
    try:
        return [(r[0], r[1]) for r in json.loads(out)["rows"]]
    except (json.JSONDecodeError, KeyError, IndexError):
        return []


def main() -> int:
    if len(sys.argv) > 1 and sys.argv[1] == "sample":
        # `dump_per_lang_kinds.py sample <Lang> <Kind> [skip=0] [n=8]`
        if len(sys.argv) < 4:
            print(
                "usage: dump_per_lang_kinds.py sample <Lang> <Kind> [skip=0] [n=8]",
                file=sys.stderr,
            )
            return 2
        lang = sys.argv[2]
        kind = sys.argv[3]
        skip = int(sys.argv[4]) if len(sys.argv) > 4 else 0
        n = int(sys.argv[5]) if len(sys.argv) > 5 else 8
        rows = sample_kind(lang, kind, n=n, skip=skip)
        print(f"# sample rs/{lang}/{kind} SKIP={skip} LIMIT={n}  rows={len(rows)}")
        for name, fp in rows:
            print(f"  {name:<30} {fp}")
        return 0

    target = sys.argv[1] if len(sys.argv) > 1 else None
    langs = [target] if target else LANGS
    if target and target not in LANGS:
        print(f"unknown lang: {target} (expected one of {LANGS})", file=sys.stderr)
        return 2
    for lang in langs:
        rs = cypher_rs_per_lang(lang)
        ref = cypher_ref_per_lang(lang)
        print_lang_table(lang, rs, ref)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
