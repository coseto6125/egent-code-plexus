#!/usr/bin/env python3
"""Per-language per-symbol parity diff: gnx-rs vs reference gitnexus.

Dumps `(kind, filePath, name)` tuples from both indices per language, then
emits `<lang>_rs_only.txt` / `<lang>_ref_only.txt` + `summary.md` showing
diff counts. Sampling those text files surfaces which side parsed wrong.

Assumes both binaries have already indexed `.sample_repo/<Lang>/...` —
gnx-rs registers each lang sub-dir separately (cd into it to query), while
ref-gitnexus uses a single `.sample_repo/` registration with `<Lang>/` path
prefix filtering.

Run from any cwd:
    python3 scripts/parity/dump_per_lang_symbols.py            # all 14 langs (ref cached)
    python3 scripts/parity/dump_per_lang_symbols.py Python     # one lang
    PARITY_REFRESH_REF=1 python3 .../dump_per_lang_symbols.py  # force re-dump ref

ref-gitnexus dumps are deterministic over `.sample_repo` (its index never
changes between iterations) and account for ~60% of the runtime due to ref-
gitnexus's paginated cypher API. By default this script re-uses the previously
written `<Lang>_ref_all.txt` when present; set PARITY_REFRESH_REF=1 to force a
re-dump (needed when `.sample_repo` corpus is rebuilt or ref-gitnexus is
upgraded).
"""
from __future__ import annotations

import json
import os
import re
import subprocess
import sys
from pathlib import Path

REPO = Path("/home/enor/gitnexus-rs/.sample_repo")
OUT_DIR = Path(__file__).parent / "symbol_diffs"
REFRESH_REF = os.environ.get("PARITY_REFRESH_REF", "").strip().lower() in {"1", "true", "yes"}
LANGS = [
    "TypeScript", "JavaScript", "Python", "Java", "Kotlin",
    "CSharp", "Go", "Rust", "PHP", "Ruby",
    "Swift", "C", "Cpp", "Dart",
]

# Per-lang file extensions for cypher scoping. The previous dir-prefix
# scheme (cwd=REPO/lang for rs; STARTS WITH 'lang/' for ref) had two
# defects:
#   1. cwd-based rs queries hit a per-lang partial index (gnx treated
#      sub-dirs as separate repos), returning only a fraction of the
#      true rs-side emissions and inflating ref_only counts.
#   2. dir-prefix collides (`Java/` ↔ `JavaScript/`), and Kotlin sample
#      = mixed-Kotlin/Java repo, so STARTS WITH 'Kotlin/' double-counts
#      Java files as Kotlin.
# Switch both sides to file-extension scoping against the root index.
LANG_EXTS: dict[str, list[str]] = {
    "TypeScript": [".ts", ".tsx"],
    "JavaScript": [".js", ".mjs", ".cjs", ".jsx"],
    "Python":     [".py", ".pyi"],
    "Java":       [".java"],
    "Kotlin":     [".kt", ".kts"],
    "CSharp":     [".cs"],
    "Go":         [".go"],
    "Rust":       [".rs"],
    "PHP":        [".php"],
    "Ruby":       [".rb"],
    "Swift":      [".swift"],
    "C":          [".c"],
    # `.h` belongs to Cpp here — both gnx-rs and ref-gitnexus route `.h` through
    # the C++ parser (gnx-rs `Language::from_normalized_path`, ref-gitnexus
    # `language-detection.ts` EXTENSION_MAP). Routing through C would silently
    # drop every class/template/method declaration in C++ headers that ship
    # with `.h`.
    "Cpp":        [".cpp", ".cc", ".cxx", ".hpp", ".hh", ".hxx", ".h"],
    "Dart":       [".dart"],
}


def _ext_clause(exts: list[str], var: str = "n") -> str:
    inner = " OR ".join(f"{var}.filePath ENDS WITH '{ext}'" for ext in exts)
    return f"({inner})"


# Auxiliary node types that don't represent user-authored symbols.
DROP_KINDS = {"Folder", "File", "CodeElement", "Community", "CodeEmbedding"}

ROW_RE = re.compile(r"\|\s*([^|]*?)\s*\|\s*([^|]*?)\s*\|\s*([^|]*?)\s*\|")


def is_anon(name: str) -> bool:
    if not name:
        return True
    return name.startswith(("__anon_", "anonymous_")) or name in {
        "<lambda>", "<anon>", "anonymous",
    }


def run(cmd: list[str], cwd: Path | None = None) -> str:
    r = subprocess.run(cmd, capture_output=True, text=True, cwd=cwd)
    if r.returncode != 0:
        print(f"!! {' '.join(cmd[:4])} ... rc={r.returncode}", file=sys.stderr)
        print(r.stderr[:300], file=sys.stderr)
    return r.stdout


def dump_rs(lang: str) -> set[tuple[str, str, str]]:
    """Cypher the root index with file-extension scoping for this lang.

    Returns paths with the `<Lang>/` corpus-dir prefix stripped so they
    align with ref-side paths (which also have the prefix stripped in
    `_parse_ref_md`).
    """
    exts = LANG_EXTS.get(lang, [])
    if not exts:
        return set()
    where = _ext_clause(exts, "n")
    q = f"MATCH (n) WHERE {where} RETURN n.kind, n.filePath, n.name"
    out = run(["gnx", "cypher", "--repo", str(REPO), q, "--format", "json"])
    try:
        obj = json.loads(out)
    except json.JSONDecodeError:
        return set()
    prefix = f"{lang}/"
    sink: set[tuple[str, str, str]] = set()
    for row in obj.get("rows", []):
        kind, fp, name = row[0], row[1], row[2]
        if kind in DROP_KINDS or is_anon(name):
            continue
        if fp.startswith(prefix):
            fp = fp[len(prefix):]
        sink.add((kind, fp, name))
    return sink


REF_PAGE = 200  # ref-gitnexus stdout truncates at 64 KB; keep pages well under.


def _parse_ref_md(md: str, prefix: str, sink: set[tuple[str, str, str]]) -> int:
    """Append parsed rows from one markdown page into `sink`; return data-row count."""
    md = md.replace("\\n", "\n")
    data_rows = 0
    for line in md.split("\n"):
        m = ROW_RE.match(line.strip())
        if not m:
            continue
        kind, fp, name = m.group(1), m.group(2), m.group(3)
        if kind.startswith("LABEL") or kind in {"---", "labels(n)"}:
            continue
        if fp == "n.filePath" or name == "n.name":
            continue
        data_rows += 1
        kind = kind.replace("[", "").replace("]", "").replace('"', "")
        kind = kind.split(",")[0].strip()
        if kind in DROP_KINDS or is_anon(name):
            continue
        if fp.startswith(prefix):
            fp = fp[len(prefix):]
        sink.add((kind, fp, name))
    return data_rows


def dump_ref(lang: str) -> set[tuple[str, str, str]]:
    """ref-gitnexus markdown table, paged via SKIP/LIMIT (64KB stdout cap).

    Scopes with file-extension WHERE clause to match `dump_rs` semantics —
    `STARTS WITH 'Java/'` would prefix-collide with `JavaScript/` paths.
    Path prefix `<Lang>/` is still stripped post-fetch so cross-side keys
    align (rs-side may include hits under `Kotlin/` for `.java` files in
    the mixed Kotlin+Java sample; we strip just the leading-corpus-dir
    segment matching the requested lang).

    ORDER BY is mandatory: cypher SKIP/LIMIT without ORDER BY is undefined,
    and ref-gitnexus's executor returns rows in a hash-table iteration
    order that changes between pages — that caused inconsistent ref totals
    across consecutive runs (observed 3032 vs 2625 vs 3323 for the same
    corpus). Ordering by `(filePath, name, labels)` makes pages stable.
    """
    exts = LANG_EXTS.get(lang, [])
    if not exts:
        return set()
    prefix = f"{lang}/"
    where = _ext_clause(exts, "n")
    sink: set[tuple[str, str, str]] = set()
    skip = 0
    while True:
        q = (
            f"MATCH (n) WHERE {where} "
            f"RETURN labels(n), n.filePath, n.name "
            f"ORDER BY n.filePath, n.name, labels(n) "
            f"SKIP {skip} LIMIT {REF_PAGE}"
        )
        out = run(["gitnexus", "cypher", "--repo", str(REPO), q])
        md = ""
        try:
            obj = json.loads(out)
            md = obj.get("markdown", "")
        except json.JSONDecodeError:
            m = re.search(r'markdown:\s*"(.*?)"\s*(?:row_count|$)', out, re.DOTALL)
            if m:
                md = m.group(1)
        if not md:
            break
        page_rows = _parse_ref_md(md, prefix, sink)
        if page_rows < REF_PAGE:
            break
        skip += REF_PAGE
    return sink


def _read_cached_ref(lang: str) -> set[tuple[str, str, str]] | None:
    """Re-read previously dumped `<Lang>_ref_all.txt` so we can skip the
    expensive paginated cypher round-trip. Returns None if the cache file
    doesn't exist or is empty; the caller falls back to `dump_ref`.
    """
    path = OUT_DIR / f"{lang}_ref_all.txt"
    if not path.exists():
        return None
    rows: set[tuple[str, str, str]] = set()
    for line in path.read_text(errors="replace").splitlines():
        parts = line.split("\t", 2)
        if len(parts) == 3 and parts[2]:
            rows.add((parts[0], parts[1], parts[2]))
    return rows if rows else None


def diff_lang(lang: str) -> tuple[int, int, int, int, int]:
    rs = dump_rs(lang)
    cached_ref = None if REFRESH_REF else _read_cached_ref(lang)
    ref = cached_ref if cached_ref is not None else dump_ref(lang)
    rs_only = rs - ref
    ref_only = ref - rs
    common = len(rs & ref)
    OUT_DIR.mkdir(parents=True, exist_ok=True)
    (OUT_DIR / f"{lang}_rs_only.txt").write_text(
        "\n".join(f"{k}\t{f}\t{n}" for k, f, n in sorted(rs_only)) + "\n"
    )
    (OUT_DIR / f"{lang}_ref_only.txt").write_text(
        "\n".join(f"{k}\t{f}\t{n}" for k, f, n in sorted(ref_only)) + "\n"
    )
    # Full sets enable aggregator cross-side pairing: a ref-only row at
    # `(Template, p, name)` should pair with a rs-side `(Function, p, name)`
    # that's in `common` (and therefore missing from `rs_only.txt`).
    (OUT_DIR / f"{lang}_rs_all.txt").write_text(
        "\n".join(f"{k}\t{f}\t{n}" for k, f, n in sorted(rs)) + "\n"
    )
    (OUT_DIR / f"{lang}_ref_all.txt").write_text(
        "\n".join(f"{k}\t{f}\t{n}" for k, f, n in sorted(ref)) + "\n"
    )
    return len(rs), len(ref), len(rs_only), len(ref_only), common


def main() -> int:
    target = sys.argv[1] if len(sys.argv) > 1 else None
    langs = [target] if target else LANGS
    if target and target not in LANGS:
        print(f"unknown lang: {target} (expected one of {LANGS})", file=sys.stderr)
        return 2
    rows: list[tuple[str, int, int, int, int, int]] = []
    for lang in langs:
        rs_n, ref_n, rs_only, ref_only, common = diff_lang(lang)
        rows.append((lang, rs_n, ref_n, rs_only, ref_only, common))
        print(
            f"{lang:<12} rs={rs_n:>6} ref={ref_n:>6}  "
            f"rs_only={rs_only:>6} ref_only={ref_only:>6} common={common:>6}"
        )
    md = [
        "# Per-lang per-symbol parity diff",
        "",
        "Tuple key = `(kind, filePath, name)`. Drops auxiliary kinds "
        f"({sorted(DROP_KINDS)}) and anonymous names.",
        "",
        "| Lang | rs | ref | rs_only | ref_only | common |",
        "| --- | ---: | ---: | ---: | ---: | ---: |",
    ]
    for r in rows:
        md.append("| {} | {} | {} | {} | {} | {} |".format(*r))
    (OUT_DIR / "summary.md").write_text("\n".join(md) + "\n")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
