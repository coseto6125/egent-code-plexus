#!/usr/bin/env python3
"""Per-symbol parity review packet generator.

Walks the aggregator's cross-side `(path, name)` pairing on
`<Lang>_{rs,ref}_all.txt` and, for every diff entry that survives EQUIV
collapse, emits a markdown block with:

  * which side(s) emitted what (rs kind list, ref kind list)
  * the source declaration site at `.sample_repo/<Lang>/<path>` with
    ±N lines of context (grep `\\b<name>\\b`, first match wins)
  * a blank verdict line for the reviewer to fill in
    (real_bug / label_diff / design / defensive)

The aim is to collapse the manual loop "open _only.txt → eyeball name →
grep file → open file → scroll → check both sides" into a single
greppable markdown packet per language.

Usage
-----
    # All 14 langs, default buckets (real_rs + real_ref), 50/kind cap
    python3 scripts/parity/review_diffs.py

    # One language, drop the cap so everything is dumped
    python3 scripts/parity/review_diffs.py --lang PHP --limit 0

    # Narrow to a single kind on the rs over-emission side
    python3 scripts/parity/review_diffs.py --lang TypeScript --kind Function

    # Include label_diff entries (useful when verifying EQUIV class)
    python3 scripts/parity/review_diffs.py --bucket real_rs,real_ref,label

    # Widen the context window
    python3 scripts/parity/review_diffs.py --context 15

Outputs land at ``scripts/parity/review/<Lang>_review.md``. Re-running
overwrites — these are derived artefacts, not source-of-truth.
"""
from __future__ import annotations

import argparse
import datetime as dt
import re
from collections import defaultdict
from pathlib import Path

REPO = Path("/home/enor/code-graph-nexus/.sample_repo")
SCRIPT_DIR = Path(__file__).parent
DIFF_DIR = SCRIPT_DIR / "symbol_diffs"
OUT_DIR = SCRIPT_DIR / "review"

LANGS = [
    "TypeScript", "JavaScript", "Python", "Java", "Kotlin",
    "CSharp", "Go", "Rust", "PHP", "Ruby",
    "Swift", "C", "Cpp", "Dart",
]

# Mirror parity_aggregate.py — keep in sync. Anything here is a
# label-equivalence (cross-side same declaration, different kind label).
MODEL_RS_ONLY = {"EntryPoint", "Process", "Annotation", "Trait", "Impl"}
MODEL_REF_ONLY = {"Section", "Folder", "File", "Document"}
_EQUIV_CLASSES: list[set[str]] = [
    {"Method", "Function", "Template", "Constructor"},
    {"Typedef", "TypeAlias"},
    {"Const", "Variable", "Property", "Static"},
    {"Interface", "Struct", "Enum", "Annotation", "Class", "Trait", "Union"},
    {"Delegate", "Function"},
]

# Markdown fence languages — best-effort syntax tag. Falls back to "" when
# the file extension isn't in the table (still produces a fenced block).
EXT_TO_FENCE: dict[str, str] = {
    ".ts": "typescript", ".tsx": "tsx", ".js": "javascript", ".mjs": "javascript",
    ".cjs": "javascript", ".jsx": "jsx", ".py": "python", ".pyi": "python",
    ".java": "java", ".kt": "kotlin", ".kts": "kotlin", ".cs": "csharp",
    ".go": "go", ".rs": "rust", ".php": "php", ".rb": "ruby",
    ".swift": "swift", ".c": "c", ".h": "c", ".cpp": "cpp", ".cc": "cpp",
    ".cxx": "cpp", ".hpp": "cpp", ".hh": "cpp", ".hxx": "cpp",
    ".dart": "dart",
}


def _build_equiv_map() -> dict[str, frozenset[str]]:
    parent: dict[str, str] = {}

    def find(x: str) -> str:
        while parent.setdefault(x, x) != x:
            parent[x] = parent[parent[x]]
            x = parent[x]
        return x

    def union(a: str, b: str) -> None:
        ra, rb = find(a), find(b)
        if ra != rb:
            parent[ra] = rb

    for cls in _EQUIV_CLASSES:
        members = list(cls)
        for m in members:
            find(m)
        for m in members[1:]:
            union(members[0], m)
    classes: dict[str, set[str]] = defaultdict(set)
    for k in list(parent):
        classes[find(k)].add(k)
    return {k: frozenset(classes[find(k)]) for k in parent}


EQUIV = _build_equiv_map()


# Mirror parity_aggregate.py — keep in sync. Pairs ref `Route /<path>` with
# rs `EntryPoint route@<func>` per file (Python Blueprint / FastAPI shorthand
# `@bp.get("/path") def func():` captured as different kind+name on each side,
# so EQUIV `(path, name)` lookup can't pair them). Per-file count match is the
# deterministic alias; scope is intentionally narrow (`route@` prefix only).
#
# Return type differs from parity_aggregate.py BY DESIGN: aggregate only needs
# the count for bucket tallies, so it returns `int`. This variant returns the
# matched `(rs_row, ref_row)` pairs so `classify_lang` can synthesize label
# entries that show the cross-side mapping in the review markdown. Keep the
# `defaultdict` + `min(...)` body identical otherwise.
def _pair_route_aliases(
    rs_only: set[tuple[str, str, str]],
    ref_only: set[tuple[str, str, str]],
) -> tuple[
    set[tuple[str, str, str]],
    set[tuple[str, str, str]],
    list[tuple[tuple[str, str, str], tuple[str, str, str]]],
]:
    rs_by_file: dict[str, list[tuple[str, str, str]]] = defaultdict(list)
    ref_by_file: dict[str, list[tuple[str, str, str]]] = defaultdict(list)
    for row in rs_only:
        if row[0] == "EntryPoint" and row[2].startswith("route@"):
            rs_by_file[row[1]].append(row)
    for row in ref_only:
        if row[0] == "Route":
            ref_by_file[row[1]].append(row)
    pairs: list[tuple[tuple[str, str, str], tuple[str, str, str]]] = []
    drop_rs: set[tuple[str, str, str]] = set()
    drop_ref: set[tuple[str, str, str]] = set()
    for fp, ref_rows in ref_by_file.items():
        rs_rows = rs_by_file.get(fp, [])
        n = min(len(ref_rows), len(rs_rows))
        if n == 0:
            continue
        for ref_row, rs_row in zip(ref_rows[:n], rs_rows[:n], strict=True):
            pairs.append((rs_row, ref_row))
        drop_rs.update(rs_rows[:n])
        drop_ref.update(ref_rows[:n])
    return rs_only - drop_rs, ref_only - drop_ref, pairs


def read_rows(path: Path) -> list[tuple[str, str, str]]:
    if not path.exists():
        return []
    out: list[tuple[str, str, str]] = []
    for line in path.read_text(errors="replace").splitlines():
        parts = line.split("\t", 2)
        if len(parts) == 3:
            out.append((parts[0], parts[1], parts[2]))
    return out


# -- pairing -----------------------------------------------------------------


def classify_lang(lang: str) -> dict[str, list[dict]]:
    """Return per-bucket list of diff entries for one language.

    Buckets: ``label`` | ``model`` | ``real_rs`` | ``real_ref``. Each entry
    is a dict ``{kind, path, name, rs_kinds, ref_kinds}`` where
    ``rs_kinds`` / ``ref_kinds`` list every kind label that the respective
    side emits at this ``(path, name)``.
    """
    rs_all = read_rows(DIFF_DIR / f"{lang}_rs_all.txt")
    ref_all = read_rows(DIFF_DIR / f"{lang}_ref_all.txt")
    if not rs_all and not ref_all:
        return {"label": [], "model": [], "real_rs": [], "real_ref": []}

    rs_by_pn: dict[tuple[str, str], list[str]] = defaultdict(list)
    ref_by_pn: dict[tuple[str, str], list[str]] = defaultdict(list)
    for k, p, n in rs_all:
        rs_by_pn[(p, n)].append(k)
    for k, p, n in ref_all:
        ref_by_pn[(p, n)].append(k)

    rs_set = set(rs_all)
    ref_set = set(ref_all)
    rs_only = rs_set - ref_set
    ref_only = ref_set - rs_set
    rs_only, ref_only, route_pairs = _pair_route_aliases(rs_only, ref_only)
    # Mirror of parity_aggregate `_pair_route_method_prefix`: cgn emits
    # Route names as `"METHOD path"` (e.g., `"GET /users"`) while ref emits
    # the bare path. Strip method prefix to surface as label_diff instead of
    # appearing as both rs_over (METHOD-prefixed) and ref_over (bare path).
    HTTP_METHOD_PREFIXES = (
        "GET ", "POST ", "PUT ", "DELETE ", "PATCH ",
        "OPTIONS ", "HEAD ", "CONNECT ", "TRACE ", "ALL ", "USE ",
    )

    def _strip_method(n: str) -> str:
        for m in HTTP_METHOD_PREFIXES:
            if n.startswith(m):
                return n[len(m):]
        return n

    ref_route_pn: set[tuple[str, str]] = set()
    for fk, p, n in ref_only:
        if fk == "Route":
            ref_route_pn.add((p, n))
    drop_rs_route: set[tuple[str, str, str]] = set()
    method_prefix_pairs: list[tuple[tuple[str, str, str], tuple[str, str, str]]] = []
    paired_ref_route_pn: set[tuple[str, str]] = set()
    for row in rs_only:
        if row[0] != "Route":
            continue
        normalized = _strip_method(row[2])
        if normalized == row[2]:
            continue
        key = (row[1], normalized)
        if key in ref_route_pn and key not in paired_ref_route_pn:
            drop_rs_route.add(row)
            paired_ref_route_pn.add(key)
            method_prefix_pairs.append((row, ("Route", key[0], key[1])))
    drop_ref_route: set[tuple[str, str, str]] = set()
    for row in ref_only:
        if row[0] == "Route" and (row[1], row[2]) in paired_ref_route_pn:
            drop_ref_route.add(row)
    rs_only -= drop_rs_route
    ref_only -= drop_ref_route
    # Mirror of parity_aggregate `_pair_ref_const_function_double_emit`:
    # ref-gitnexus double-emits `Const` + `Function` for TS/JS arrow-fn
    # bindings (`export const fn = (...) => ...`); cgn emits only
    # `Function`. The Const-side leftover is a label mismatch, not a
    # missing symbol. Narrow scope — requires ref-side AND rs-side
    # Function at same `(p, n)` so plain `const x = 42; function x() {}`
    # collisions don't pair.
    const_fn_pairs: list[tuple[str, str]] = []
    drop_ref_const: set[tuple[str, str, str]] = set()
    for row in ref_only:
        if row[0] != "Const":
            continue
        ref_kinds = ref_by_pn.get((row[1], row[2]), [])
        rs_kinds = rs_by_pn.get((row[1], row[2]), [])
        if "Function" in ref_kinds and "Function" in rs_kinds:
            drop_ref_const.add(row)
            const_fn_pairs.append((row[1], row[2]))
    ref_only -= drop_ref_const

    # Mirror of parity_aggregate `_pair_ref_template_class_double_emit`:
    # ref-gitnexus double-emits `Class` + `Template` for Cpp
    # `template<typename T> class Foo`; cgn emits only `Class`. The
    # Template-side leftover is a label mismatch, not a missing symbol.
    # Same shape as Const/Function: require ref-side type-family kind AND
    # rs-side type-family kind at same `(p, n)` so we only pair true
    # double-emits.
    TEMPLATE_TYPE_PAIR_KINDS = {
        "Class", "Struct", "Interface", "Enum", "Trait", "Union",
    }
    template_class_pairs: list[tuple[str, str]] = []
    drop_ref_template: set[tuple[str, str, str]] = set()
    for row in ref_only:
        if row[0] != "Template":
            continue
        ref_kinds = ref_by_pn.get((row[1], row[2]), [])
        rs_kinds = rs_by_pn.get((row[1], row[2]), [])
        if any(rk in TEMPLATE_TYPE_PAIR_KINDS for rk in ref_kinds) and any(
            rk in TEMPLATE_TYPE_PAIR_KINDS for rk in rs_kinds
        ):
            drop_ref_template.add(row)
            template_class_pairs.append((row[1], row[2]))
    ref_only -= drop_ref_template

    buckets: dict[str, list[dict]] = {
        "label": [], "model": [], "real_rs": [], "real_ref": [],
    }
    # Surface METHOD-prefix route pairings so the markdown shows both sides.
    for rs_row, ref_row in method_prefix_pairs:
        _, p, rs_name = rs_row
        _, _, ref_name = ref_row
        buckets["label"].append({
            "kind": "Route",
            "path": p,
            "name": ref_name,
            "rs_kinds": [f"Route[{rs_name}]"],
            "ref_kinds": ["Route"],
        })
    # Surface paired Template↔Class double-emits as label entries.
    for p, n in template_class_pairs:
        rs_kinds = sorted(set(rs_by_pn.get((p, n), [])))
        ref_kinds = sorted(set(ref_by_pn.get((p, n), [])))
        buckets["label"].append({
            "kind": "Template",
            "path": p,
            "name": n,
            "rs_kinds": rs_kinds,
            "ref_kinds": ref_kinds,
        })
    # Surface paired Const↔Function double-emits as label entries so the
    # review markdown still shows the cross-side mapping the reviewer
    # would otherwise have to discover manually.
    for p, n in const_fn_pairs:
        buckets["label"].append({
            "kind": "Const",
            "path": p,
            "name": n,
            "rs_kinds": ["Function"],
            "ref_kinds": ["Const", "Function"],
        })

    # Render route aliases as label entries keyed on the ref-side Route row
    # (URL path is what the reviewer wants to verify); annotate `rs_kinds`
    # with the paired EntryPoint name so the cross-side mapping is visible
    # without a separate report section.
    for rs_row, ref_row in route_pairs:
        _, p, ref_name = ref_row
        _, _, rs_name = rs_row
        buckets["label"].append({
            "kind": "Route",
            "path": p,
            "name": ref_name,
            "rs_kinds": [f"EntryPoint[{rs_name}]"],
            "ref_kinds": ["Route"],
        })

    def entry(kind: str, path: str, name: str) -> dict:
        return {
            "kind": kind, "path": path, "name": name,
            "rs_kinds": sorted(set(rs_by_pn.get((path, name), []))),
            "ref_kinds": sorted(set(ref_by_pn.get((path, name), []))),
        }

    for rk, p, n in sorted(rs_only):
        ref_kinds = ref_by_pn.get((p, n), [])
        if rk in EQUIV and any(fk in EQUIV.get(rk, set()) for fk in ref_kinds):
            buckets["label"].append(entry(rk, p, n))
            continue
        if rk in MODEL_RS_ONLY:
            buckets["model"].append(entry(rk, p, n))
        else:
            buckets["real_rs"].append(entry(rk, p, n))
    for fk, p, n in sorted(ref_only):
        rs_kinds = rs_by_pn.get((p, n), [])
        if fk in EQUIV and any(rk in EQUIV.get(fk, set()) for rk in rs_kinds):
            # Already accounted on rs side iteration — avoid double-count.
            continue
        if fk in MODEL_REF_ONLY:
            buckets["model"].append(entry(fk, p, n))
        else:
            buckets["real_ref"].append(entry(fk, p, n))
    return buckets


# -- source snippet ----------------------------------------------------------


def resolve_file(lang: str, rel_path: str) -> Path | None:
    """Probe the two layouts dump_per_lang_symbols.py may produce.

    Paths starting with ``<Lang>/`` get stripped in the dump; for those we
    prepend ``<REPO>/<Lang>/``. A few cross-lang spillovers (eg `.h` files
    under `Cpp/` showing up in C lang dump because of file-ext scoping)
    keep the original prefix — try ``<REPO>/<path>`` as a fallback.
    """
    cands = [REPO / lang / rel_path, REPO / rel_path]
    for c in cands:
        if c.is_file():
            return c
    return None


_WORD_BOUNDARY_CACHE: dict[str, re.Pattern[str]] = {}


def _name_re(name: str) -> re.Pattern[str]:
    if name not in _WORD_BOUNDARY_CACHE:
        _WORD_BOUNDARY_CACHE[name] = re.compile(rf"\b{re.escape(name)}\b")
    return _WORD_BOUNDARY_CACHE[name]


def find_declaration(
    file_path: Path, name: str, context: int,
) -> tuple[int, list[str]] | None:
    """Grep ``\\bname\\b`` in the file, return (1-indexed line, snippet)
    centred on the FIRST match. ``None`` when the name doesn't appear.

    Heuristic, not parser-grade: prefers the first occurrence under the
    assumption that the declaration usually precedes references. Where
    that's wrong, the reviewer still sees the file and can spot the right
    line nearby — much faster than running 14 lang-specific regexes.
    """
    try:
        text = file_path.read_text(errors="replace")
    except OSError:
        return None
    lines = text.splitlines()
    rx = _name_re(name)
    for i, line in enumerate(lines, start=1):
        if rx.search(line):
            lo = max(1, i - context)
            hi = min(len(lines), i + context)
            snippet = []
            for j in range(lo, hi + 1):
                marker = ">>" if j == i else "  "
                snippet.append(f"{marker} {j:>4} │ {lines[j - 1]}")
            return i, snippet
    return None


# -- rendering ---------------------------------------------------------------


def fence_for(path: str) -> str:
    suffix = Path(path).suffix.lower()
    return EXT_TO_FENCE.get(suffix, "")


def render_entry(lang: str, bucket: str, e: dict, context: int) -> list[str]:
    """Render one diff entry — code-first, metadata trimmed.

    Layout puts the source snippet within the first ~6 lines of the
    entry so the reviewer sees the actual divergent declaration without
    scrolling past bullets. The diff itself is one line; the verdict
    prompt is one line. Everything else is the code.
    """
    path, name, kind = e["path"], e["name"], e["kind"]
    rs_label = ", ".join(e["rs_kinds"]) or "—"
    ref_label = ", ".join(e["ref_kinds"]) or "—"

    file_path = resolve_file(lang, path)
    located = find_declaration(file_path, name, context) if file_path else None
    line_marker = f":{located[0]}" if located else ""

    out = [
        f"### `{name}` @ {path}{line_marker}",
        "",
        f"`{bucket}` · cgn **{rs_label}** vs ref **{ref_label}**",
        "",
    ]
    if file_path is None:
        out += [
            f"> source file not found at `{REPO}/{lang}/{path}` or `{REPO}/{path}`",
            "",
            "verdict: _____  (real_bug / label_diff / design / defensive)",
            "",
        ]
        return out
    if located is None:
        out += [
            f"> name `{name}` not found in `{file_path.relative_to(REPO)}` "
            f"(stale dump, renamed symbol, or non-identifier capture)",
            "",
            "verdict: _____  (real_bug / label_diff / design / defensive)",
            "",
        ]
        return out
    _, snippet = located
    out.append(f"```{fence_for(path)}")
    out.extend(snippet)
    out.append("```")
    out.append("")
    out.append("verdict: _____  (real_bug / label_diff / design / defensive)")
    out.append("")
    return out


def filter_entries(
    entries: list[dict], kind_filter: str | None, limit_per_kind: int,
) -> list[dict]:
    if kind_filter:
        entries = [e for e in entries if e["kind"] == kind_filter]
    if limit_per_kind <= 0:
        return entries
    by_kind: dict[str, list[dict]] = defaultdict(list)
    for e in entries:
        by_kind[e["kind"]].append(e)
    out: list[dict] = []
    for k in sorted(by_kind):
        out.extend(by_kind[k][:limit_per_kind])
    return out


def render_lang(
    lang: str, buckets_wanted: list[str],
    kind_filter: str | None, limit_per_kind: int, context: int,
) -> str | None:
    classified = classify_lang(lang)
    sections: list[str] = []
    summary_counts: list[str] = []
    total = 0
    for b in ("real_rs", "real_ref", "label", "model"):
        summary_counts.append(f"{b}={len(classified[b])}")
    for bucket in buckets_wanted:
        entries = filter_entries(classified.get(bucket, []), kind_filter,
                                 limit_per_kind)
        if not entries:
            continue
        by_kind: dict[str, list[dict]] = defaultdict(list)
        for e in entries:
            by_kind[e["kind"]].append(e)
        sections.append(f"## Bucket: `{bucket}`")
        sections.append("")
        for k in sorted(by_kind):
            sections.append(f"Kind: **{k}** ({len(by_kind[k])} entries shown)")
            sections.append("")
            for e in by_kind[k]:
                sections.extend(render_entry(lang, bucket, e, context))
                total += 1
            sections.append("---")
            sections.append("")
    if total == 0:
        return None
    header = [
        f"# {lang} parity review packet",
        "",
        f"_Generated {dt.date.today().isoformat()} from "
        f"`scripts/parity/symbol_diffs/{lang}_*_all.txt`._",
        "",
        f"Bucket counts (full, pre-filter): {', '.join(summary_counts)}.",
        f"Filters: buckets={buckets_wanted}, kind={kind_filter or '*'}, "
        f"limit_per_kind={limit_per_kind or '∞'}, context=±{context}.",
        "",
        "Verdict legend:",
        "- **real_bug** — type-level permanent symbol, cgn (or ref) misses it. Fix parser.",
        "- **label_diff** — same declaration, kind label differs (EQUIV class miss). Update EQUIV map.",
        "- **design** — intentional drop (block-scoped transient, builder filter, etc).",
        "- **defensive** — predicate guard with no observed bug. Cosmetic.",
        "",
        "---",
        "",
    ]
    return "\n".join(header + sections)


# -- cli ---------------------------------------------------------------------


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument("--lang", default=None,
                    help="One of " + ", ".join(LANGS) + " (default: all 14)")
    ap.add_argument("--kind", default=None,
                    help="Restrict to a single kind label (eg Function)")
    ap.add_argument(
        "--bucket", default="real_rs,real_ref",
        help="Comma list of buckets to include: real_rs, real_ref, label, model "
             "(default: real_rs,real_ref)")
    ap.add_argument("--limit", type=int, default=50,
                    help="Max entries per kind per lang. 0 = no cap. Default 50.")
    ap.add_argument("--context", type=int, default=10,
                    help="±N source lines around the matched declaration. Default 10.")
    ap.add_argument("--out-dir", default=str(OUT_DIR),
                    help=f"Where to write <Lang>_review.md (default: {OUT_DIR})")
    args = ap.parse_args()

    if args.lang and args.lang not in LANGS:
        print(f"unknown lang: {args.lang} (expected one of {LANGS})")
        return 2
    buckets = [b.strip() for b in args.bucket.split(",") if b.strip()]
    bad = [b for b in buckets if b not in {"real_rs", "real_ref", "label", "model"}]
    if bad:
        print(f"unknown bucket(s): {bad}")
        return 2

    out_dir = Path(args.out_dir)
    out_dir.mkdir(parents=True, exist_ok=True)
    langs = [args.lang] if args.lang else LANGS
    written: list[tuple[str, Path, int]] = []
    for lang in langs:
        body = render_lang(lang, buckets, args.kind, args.limit, args.context)
        if body is None:
            print(f"{lang:<12} no entries after filter")
            continue
        out_file = out_dir / f"{lang}_review.md"
        out_file.write_text(body)
        n_blocks = body.count("\n### `")
        written.append((lang, out_file, n_blocks))
        print(f"{lang:<12} → {out_file.relative_to(SCRIPT_DIR.parent.parent) if out_file.is_absolute() else out_file}  ({n_blocks} entries)")
    if not written:
        print("no review packets written (everything filtered out).")
        return 1
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
