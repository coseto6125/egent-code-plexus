#!/usr/bin/env python3
"""Aggregate per-lang symbol_diffs into a 14-lang root-cause table.

Reads <Lang>_rs_only.txt and <Lang>_ref_only.txt as sets of
`(kind, filePath, name)` tuples and classifies each row into:

  - model_diff: the kind exists only in one side's taxonomy (rs:
    EntryPoint/Process/Annotation/Trait/Impl; ref:
    Section/Folder/File/Document).
  - label_diff: same `(path, name)` symbol appears on both sides with
    label-equivalent kinds (Method↔Function, Const↔Variable,
    Property↔Variable, Annotation↔Class, etc.). Pure naming choice,
    not a parser gap.
  - real_gap: load-bearing under/over-emit not explainable above.

Cross-side pairing on `(path, name)` removes the spurious "ref-only
Class" rows that pair with "rs-only Annotation" rows for the same
`*Attribute` symbol — the per-kind aggregator missed those because
LABEL_PAIRS was one-directional.
"""
from __future__ import annotations
import heapq
import os
from collections import defaultdict
from pathlib import Path

DIFF_DIR = Path(os.environ.get("PARITY_DIFF_DIR",
    str(Path(__file__).resolve().parent / "symbol_diffs")))
LANGS = [
    "TypeScript", "JavaScript", "Python", "Java", "Kotlin",
    "CSharp", "Go", "Rust", "PHP", "Ruby",
    "Swift", "C", "Cpp", "Dart",
]

MODEL_RS_ONLY = {"EntryPoint", "Process", "Annotation", "Trait", "Impl"}
MODEL_REF_ONLY = {"Section", "Folder", "File", "Document"}

# Undirected equivalence classes. Two kinds are label-equivalent if they
# share a class. Classes were derived from cross-side observation of the
# `.sample_repo` corpus + ref-gitnexus / gnx-rs source.
_EQUIV_CLASSES: list[set[str]] = [
    # ref-gitnexus emits TS / JS / Dart constructors as Method (their
    # underlying tree-sitter node is method_definition); gnx-rs promotes
    # to a dedicated Constructor kind. Verified per-file: both sides
    # find the same declarations, just labeled differently.
    {"Method", "Function", "Template", "Constructor"},
    {"Typedef", "TypeAlias"},
    {"Const", "Variable", "Property", "Static"},
    # Trait joins this class so Swift `protocol P {}` (gnx-rs emits Trait,
    # ref emits Interface) pairs as label_diff. Rust `trait` still falls
    # through to MODEL_RS_ONLY because ref-gitnexus emits no equiv-class
    # kind for Rust traits — model_diff classification kicks in after
    # EQUIV pairing fails. Union joins the class because gnx-rs C parser
    # emits `union T {}` as Struct (queries.scm:28 explicit design) while
    # ref-gitnexus emits Union; without pairing, every C/Cpp union shows
    # as ref_over (7 C + 7 Cpp in current `.sample_repo`).
    {"Interface", "Struct", "Enum", "Annotation", "Class", "Trait", "Union"},
    {"Delegate", "Function"},
]


def _build_equiv_map() -> dict[str, frozenset[str]]:
    """Flatten equivalence classes into a `kind -> equivalence_set` map.

    Union overlapping classes (e.g. {Delegate, Function} overlaps with
    {Method, Function, Template} via Function) so cross-class transitivity
    is preserved.
    """
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


# Cross-kind, cross-name alias: ref `Route /<path>` ↔ rs `EntryPoint route@<func>`.
# Same Python Blueprint / FastAPI shorthand (`@bp.get("/path") def func():`)
# captured differently per side:
#   ref Route row → name = URL path (`/block`)
#   rs EntryPoint row → name = `route@<funcname>` (Blueprint shorthand emit)
# `(path, name)` never overlaps so EQUIV can't pair them. Per-file count
# match is the deterministic alias: pair `min(R, E)` per file as label_diff
# and remove the paired rows from rs_only/ref_only. Only `route@` prefix is
# safe — `main@*` is `if __name__ == "__main__":` blocks, not routes, and
# TS `framework_ref@*` is a NestJS Controller class (1:N to Route methods,
# not a 1:1 alias). Keep scope narrow until additional shapes are verified.
# ref-side double-emit: `export const fn = (...) => ...` (TS / JS arrow-
# function-bound const) surfaces twice on ref-gitnexus — once as `Const`
# at the binding declaration, once as `Function` at the arrow expression.
# gnx-rs collapses both into a single `Function` node (the callable view,
# which is what `gnx search "fn"` should resolve to). The leftover ref-
# only `(Const, p, n)` row is a label mismatch, not a missing symbol.
#
# Quantified on `.sample_repo` 2026-05-19: TS 291 rows + JS 204 rows
# match the shape (ref has BOTH Const + Function at `(p, n)`, rs has
# Function). Pair them as label_diff to keep the parity report focused
# on real coverage gaps.
#
# Narrow on purpose — does NOT widen EQUIV to `Const ↔ Function`. A
# plain `const x = 42; function x() {}` would collide under that broader
# rule even though the two declarations refer to different source spans.
# This helper requires the ref-side Function to be present at the same
# `(p, n)` — the load-bearing signal that they are the SAME source span.
def _pair_ref_const_function_double_emit(
    ref_only: set[tuple[str, str, str]],
    rs_by_pn: dict[tuple[str, str], list[str]],
    ref_by_pn: dict[tuple[str, str], list[str]],
) -> tuple[set[tuple[str, str, str]], int]:
    drop_ref: set[tuple[str, str, str]] = set()
    pairs = 0
    for row in ref_only:
        if row[0] != "Const":
            continue
        ref_kinds = ref_by_pn.get((row[1], row[2]), [])
        rs_kinds = rs_by_pn.get((row[1], row[2]), [])
        if "Function" in ref_kinds and "Function" in rs_kinds:
            drop_ref.add(row)
            pairs += 1
    return ref_only - drop_ref, pairs


def _pair_route_aliases(
    rs_only: set[tuple[str, str, str]],
    ref_only: set[tuple[str, str, str]],
) -> tuple[set[tuple[str, str, str]], set[tuple[str, str, str]], int]:
    rs_by_file: dict[str, list[tuple[str, str, str]]] = defaultdict(list)
    ref_by_file: dict[str, list[tuple[str, str, str]]] = defaultdict(list)
    for row in rs_only:
        if row[0] == "EntryPoint" and row[2].startswith("route@"):
            rs_by_file[row[1]].append(row)
    for row in ref_only:
        if row[0] == "Route":
            ref_by_file[row[1]].append(row)
    drop_rs: set[tuple[str, str, str]] = set()
    drop_ref: set[tuple[str, str, str]] = set()
    pairs = 0
    for fp, ref_rows in ref_by_file.items():
        rs_rows = rs_by_file.get(fp, [])
        n = min(len(ref_rows), len(rs_rows))
        if n == 0:
            continue
        pairs += n
        drop_rs.update(rs_rows[:n])
        drop_ref.update(ref_rows[:n])
    return rs_only - drop_rs, ref_only - drop_ref, pairs


def read_rows(path: Path) -> list[tuple[str, str, str]]:
    if not path.exists():
        return []
    rows = []
    for line in path.read_text(errors="replace").splitlines():
        parts = line.split("\t", 2)
        if len(parts) == 3:
            rows.append((parts[0], parts[1], parts[2]))
    return rows


def lang_summary(lang: str) -> dict:
    """Read full sets for cross-side pairing, then classify only the rows
    that fall in the set-diff slice.

    Previous version read `_only.txt` (which is `rs - ref` / `ref - rs` per
    exact `(kind, path, name)` triplet). That hid label pairs whose shared
    side sat in `common` — e.g. rs `(Function, p, at)` + ref `(Function, p,
    at)` were both in `common`, while ref also had `(Template, p, at)` in
    `ref_only`. The aggregator only saw the ref-only Template row and never
    knew about the rs-side Function row → false `real_ref`.

    Fix: read `_all.txt` for the full per-side set, then iterate over the
    diff slice with cross-side lookup against the full map.
    """
    rs_all = read_rows(DIFF_DIR / f"{lang}_rs_all.txt")
    ref_all = read_rows(DIFF_DIR / f"{lang}_ref_all.txt")
    # Backwards-compat fallback for stale dumps that only have `_only.txt`.
    if not rs_all and not ref_all:
        rs_all = read_rows(DIFF_DIR / f"{lang}_rs_only.txt")
        ref_all = read_rows(DIFF_DIR / f"{lang}_ref_only.txt")
    if not rs_all and not ref_all:
        return {"lang": lang, "status": "missing"}

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
    rs_only, ref_only, route_alias_pairs = _pair_route_aliases(rs_only, ref_only)
    ref_only, const_fn_double_emit_pairs = _pair_ref_const_function_double_emit(
        ref_only, rs_by_pn, ref_by_pn
    )

    buckets = {
        "model": 0,
        "label": route_alias_pairs + const_fn_double_emit_pairs,
        "real_rs": 0,
        "real_ref": 0,
    }
    real_rs: dict[str, int] = defaultdict(int)
    real_ref: dict[str, int] = defaultdict(int)

    def classify_one(kind: str, side: str) -> str:
        if side == "rs" and kind in MODEL_RS_ONLY:
            return "model"
        if side == "ref" and kind in MODEL_REF_ONLY:
            return "model"
        return "real"

    for rk, p, n in rs_only:
        ref_kinds = ref_by_pn.get((p, n), [])
        paired_label = (
            rk in EQUIV
            and any(fk in EQUIV.get(rk, set()) for fk in ref_kinds)
        )
        if paired_label:
            buckets["label"] += 1
            continue
        cls = classify_one(rk, "rs")
        if cls == "model":
            buckets["model"] += 1
        else:
            buckets["real_rs"] += 1
            real_rs[rk] += 1
    for fk, p, n in ref_only:
        rs_kinds = rs_by_pn.get((p, n), [])
        paired_label = (
            fk in EQUIV
            and any(rk in EQUIV.get(fk, set()) for rk in rs_kinds)
        )
        if paired_label:
            buckets["label"] += 1
            continue
        cls = classify_one(fk, "ref")
        if cls == "model":
            buckets["model"] += 1
        else:
            buckets["real_ref"] += 1
            real_ref[fk] += 1

    top_rs = heapq.nlargest(5, real_rs.items(), key=lambda kv: kv[1])
    top_ref = heapq.nlargest(5, real_ref.items(), key=lambda kv: kv[1])
    return {
        "lang": lang,
        "status": "ok",
        "rs_total": len(rs_only),
        "ref_total": len(ref_only),
        "buckets": buckets,
        "top_rs": top_rs,
        "top_ref": top_ref,
    }


def main() -> None:
    print(f"{'Lang':<12} {'rs_only':>8} {'ref_only':>9} | "
          f"{'model':>6} {'label':>6} {'real_rs':>8} {'real_ref':>9} | "
          f"top_real_gap")
    print("-" * 110)
    grand = {"model": 0, "label": 0, "real_rs": 0, "real_ref": 0}
    for lang in LANGS:
        s = lang_summary(lang)
        if s.get("status") == "missing":
            print(f"{lang:<12} {'—':>8} {'—':>9} | (no diff files yet)")
            continue
        b = s["buckets"]
        for k in grand:
            grand[k] += b[k]
        top_combo = []
        for k, v in s["top_rs"][:1]:
            top_combo.append(f"{k}+{v}")
        for k, v in s["top_ref"][:1]:
            top_combo.append(f"{k}-{v}")
        top_str = ", ".join(top_combo) if top_combo else "—"
        print(f"{lang:<12} {s['rs_total']:>8} {s['ref_total']:>9} | "
              f"{b['model']:>6} {b['label']:>6} {b['real_rs']:>8} {b['real_ref']:>9} | "
              f"{top_str}")
    print("-" * 110)
    print(f"{'TOTAL':<12} {'':>8} {'':>9} | "
          f"{grand['model']:>6} {grand['label']:>6} {grand['real_rs']:>8} {grand['real_ref']:>9}")
    print()
    print("Detail per lang (top-5 real gaps):")
    for lang in LANGS:
        s = lang_summary(lang)
        if s.get("status") == "missing":
            continue
        rs_str = ", ".join(f"{k}+{v}" for k, v in s["top_rs"]) or "—"
        ref_str = ", ".join(f"{k}-{v}" for k, v in s["top_ref"]) or "—"
        print(f"  {lang:<12} rs_over: {rs_str:<60} ref_over: {ref_str}")


if __name__ == "__main__":
    main()
