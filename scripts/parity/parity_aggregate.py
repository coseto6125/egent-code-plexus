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
    "/home/enor/gitnexus-rs/scripts/parity/symbol_diffs"))
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
    # EQUIV pairing fails.
    {"Interface", "Struct", "Enum", "Annotation", "Class", "Trait"},
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

    buckets = {"model": 0, "label": 0, "real_rs": 0, "real_ref": 0}
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
