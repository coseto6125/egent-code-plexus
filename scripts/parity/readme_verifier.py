#!/usr/bin/env python3
"""README Language Matrix verifier — criteria-as-code consistency check.

The README's `## Language Matrix` table has 11 columns × ~29 lang rows.
Each cell is `✓` / `☐` / `—`. Without programmatic acceptance criteria,
those marks drift from the actual codebase (some Phase B/C/D parser
changes already silently invalidated cells). This script:

  1. Defines each dimension as a Python predicate `(lang, ctx) → Cell`
  2. Runs predicates against the indexed `.sample_repo`
  3. Compares to the README's claimed cell
  4. Reports drift rows

The README ✓/☐/— remains the user-facing claim. The predicate's
returned `Cell` is the verified state. Drift = claim ≠ verified.

Acceptance criteria are explicit in the predicate docstrings — anyone
maintaining the README can read what `✓` means for each dimension
without guessing.

Usage (from gitnexus-rs repo root):

    python3 scripts/parity/readme_verifier.py
    python3 scripts/parity/readme_verifier.py --generate  # emit a fresh table
    python3 scripts/parity/readme_verifier.py --only typescript,rust

Dimensions not yet auto-checkable (Exports, Types, Config, Frameworks)
return `Cell.MANUAL` — drift reports note them but don't fail.
"""

from __future__ import annotations

import argparse
import json
import re
import subprocess
import sys
from dataclasses import dataclass
from enum import Enum
from pathlib import Path

REPO_ROOT = Path(__file__).resolve().parent.parent.parent
SAMPLE_REPO = "/home/enor/gitnexus-rs/.sample_repo"
GNX = "/home/enor/.cargo/bin/gnx"
README = REPO_ROOT / "README.md"
ANALYZER_SRC = REPO_ROOT / "crates" / "graph-nexus-analyzer" / "src"

# README lang name → spec/parser dir name + sample_repo path prefix.
LANG_DIRS: dict[str, tuple[str, str]] = {
    "TypeScript": ("typescript", "TypeScript"),
    "JavaScript": ("javascript", "JavaScript"),
    "Python": ("python", "Python"),
    "Java": ("java", "Java"),
    "Kotlin": ("kotlin", "Kotlin"),
    "C#": ("c_sharp", "CSharp"),
    "Go": ("go", "Go"),
    "Rust": ("rust", "Rust"),
    "PHP": ("php", "PHP"),
    "Ruby": ("ruby", "Ruby"),
    "Swift": ("swift", "Swift"),
    "C": ("c", "C"),
    "C++": ("cpp", "Cpp"),
    "Dart": ("dart", "Dart"),
}


class Cell(Enum):
    YES = "✓"
    NO = "☐"
    NA = "—"
    MANUAL = "[?]"


@dataclass
class Verdict:
    cell: Cell
    evidence: str


@dataclass
class AuditCtx:
    sample_repo: str
    gnx_bin: str

    def cypher_count(self, query: str) -> int:
        """Run a `RETURN count(*)`-shaped cypher; return the integer.

        gnx cypher's JSON output is inconsistent for single-column results
        (sometimes `rows: [N]`, sometimes `rows: [[N]]`); this handles both.
        Returns 0 on any error — the verifier treats 0 as `NO`, the user
        re-runs the predicate manually to investigate.
        """
        r = subprocess.run(
            [self.gnx_bin, "cypher", "--repo", self.sample_repo, query, "--format=json"],
            capture_output=True, text=True, timeout=30,
        )
        if r.returncode != 0:
            return 0
        try:
            obj = json.loads(r.stdout)
            rows = obj.get("rows", [])
            if not rows:
                return 0
            first = rows[0]
            if isinstance(first, list):
                return int(first[0])
            return int(first)
        except (json.JSONDecodeError, ValueError, IndexError, TypeError):
            return 0


# ───────── per-dimension predicates ─────────


def dim_imports(lang_path: str, ctx: AuditCtx) -> Verdict:
    """'Imports' = ≥1 `Imports` edge originating in this lang's corpus.

    Imports are tracked as edges (file -[:Imports]-> module), not as a
    standalone `NodeKind::Import`. ref-gitnexus emits `Import` nodes
    but gnx-rs models them as relationships.
    """
    n = ctx.cypher_count(
        f"MATCH (a)-[:Imports]->(b) WHERE a.filePath STARTS WITH '{lang_path}/' "
        "RETURN count(*)"
    )
    return Verdict(Cell.YES if n > 0 else Cell.NO, f"{n} Imports edges")


SYMBOL_KINDS = [
    "Function", "Class", "Method", "Interface", "Constructor",
    "Property", "Variable", "Const", "Struct", "Enum", "Typedef",
    "Macro", "Annotation", "Trait", "Module", "Namespace",
]


def dim_named(lang_path: str, ctx: AuditCtx) -> Verdict:
    """'Named' = ≥1 symbol-kind node in this lang's corpus.

    Symbol kinds = Function / Class / Method / … (excludes File / Import /
    Route / EntryPoint / Process — those have their own dimensions or
    aren't "named symbols").
    """
    kinds_q = ", ".join(f"'{k}'" for k in SYMBOL_KINDS)
    n = ctx.cypher_count(
        f"MATCH (n) WHERE n.kind IN [{kinds_q}] AND n.filePath STARTS WITH '{lang_path}/' "
        "RETURN count(*)"
    )
    return Verdict(Cell.YES if n > 0 else Cell.NO, f"{n} named-symbol nodes")


def dim_heritage(lang_path: str, ctx: AuditCtx) -> Verdict:
    """'Heritage' = ≥1 Extends or Implements edge originating in this lang."""
    n = ctx.cypher_count(
        f"MATCH (a)-[:Extends|Implements]->(b) WHERE a.filePath STARTS WITH '{lang_path}/' "
        "RETURN count(*)"
    )
    return Verdict(Cell.YES if n > 0 else Cell.NO, f"{n} Extends/Implements edges")


def dim_ctor(lang_path: str, ctx: AuditCtx) -> Verdict:
    """'Ctor' = ≥1 node with kind=Constructor in this lang's corpus."""
    n = ctx.cypher_count(
        f"MATCH (n) WHERE n.kind='Constructor' AND n.filePath STARTS WITH '{lang_path}/' "
        "RETURN count(*)"
    )
    return Verdict(Cell.YES if n > 0 else Cell.NO, f"{n} Constructor nodes")


def dim_entry(lang_path: str, ctx: AuditCtx) -> Verdict:
    """'Entry' = ≥1 node with kind=EntryPoint in this lang's corpus."""
    n = ctx.cypher_count(
        f"MATCH (n) WHERE n.kind='EntryPoint' AND n.filePath STARTS WITH '{lang_path}/' "
        "RETURN count(*)"
    )
    return Verdict(Cell.YES if n > 0 else Cell.NO, f"{n} EntryPoint nodes")


def dim_call(lang_path: str, ctx: AuditCtx) -> Verdict:
    """'Call' = ≥1 Calls edge originating in this lang."""
    n = ctx.cypher_count(
        f"MATCH (a)-[:Calls]->(b) WHERE a.filePath STARTS WITH '{lang_path}/' "
        "RETURN count(*)"
    )
    return Verdict(Cell.YES if n > 0 else Cell.NO, f"{n} Calls edges")


def dim_rename(lang_dir: str) -> Verdict:
    """'Rename' = identifier_finder module exists for this lang.

    Code-level check: `gnx rename` dispatches per-lang via the
    `identifier_finder/<lang>.rs` module. Presence of that file is the
    proxy for rename support — without it the rename command lacks the
    lang-specific identifier-range table and exits no-op.
    """
    p = ANALYZER_SRC / "identifier_finder" / f"{lang_dir}.rs"
    return Verdict(
        Cell.YES if p.exists() else Cell.NO,
        f"identifier_finder/{lang_dir}.rs {'exists' if p.exists() else 'missing'}",
    )


# Dimensions not yet auto-checkable. Cypher can't access RawNode fields
# directly (`is_exported`, `type_annotation`, `heritage` list), and
# `Config` / `Frameworks` lack a canonical NodeKind / edge type. Return
# MANUAL — the verifier flags drift but doesn't auto-fail.
def dim_manual(reason: str):
    def predicate(lang_path: str, ctx: AuditCtx) -> Verdict:
        return Verdict(Cell.MANUAL, reason)
    return predicate


PREDICATES = {
    "Imports": lambda lp, lr, ctx: dim_imports(lp, ctx),
    "Named":   lambda lp, lr, ctx: dim_named(lp, ctx),
    "Exports": lambda lp, lr, ctx: dim_manual("cypher can't read n.is_exported")(lp, ctx),
    "Heritage": lambda lp, lr, ctx: dim_heritage(lp, ctx),
    "Types":   lambda lp, lr, ctx: dim_manual("cypher can't read n.type_annotation")(lp, ctx),
    "Ctor":    lambda lp, lr, ctx: dim_ctor(lp, ctx),
    "Config":  lambda lp, lr, ctx: dim_manual("no canonical Config NodeKind")(lp, ctx),
    "Frameworks": lambda lp, lr, ctx: dim_manual("framework_refs not exposed via cypher")(lp, ctx),
    "Entry":   lambda lp, lr, ctx: dim_entry(lp, ctx),
    "Call":    lambda lp, lr, ctx: dim_call(lp, ctx),
    "Rename":  lambda lp, lr, ctx: dim_rename(lr),
}


# ───────── README parser ─────────


README_ROW_RE = re.compile(
    r"^\|\s*([A-Za-z0-9#+]+(?:\s*[A-Za-z0-9#+/-]+)?)\s*\|"  # lang cell
    r"(\s*[^|]+\|){11}"  # 11 dimension cells
    r"\s*$"
)


def parse_readme_claims() -> dict[str, dict[str, Cell]]:
    """Parse `## Language Matrix` table → {lang: {dim: Cell}}."""
    if not README.exists():
        return {}
    text = README.read_text()
    lines = text.split("\n")
    # Find header line
    header_idx = None
    for i, line in enumerate(lines):
        if line.startswith("| Language | Imports | Named"):
            header_idx = i
            break
    if header_idx is None:
        return {}
    # Extract column names
    cols = [c.strip() for c in lines[header_idx].split("|")[1:-1]]
    dim_cols = cols[1:]  # drop "Language"
    claims: dict[str, dict[str, Cell]] = {}
    for line in lines[header_idx + 2:]:  # skip separator row
        if not line.startswith("|"):
            break
        cells = [c.strip() for c in line.split("|")[1:-1]]
        if len(cells) != len(cols):
            continue
        lang = cells[0]
        if not lang or lang.startswith("─"):
            continue
        claims[lang] = {}
        for dim, val in zip(dim_cols, cells[1:]):
            if val == "✓":
                claims[lang][dim] = Cell.YES
            elif val == "☐":
                claims[lang][dim] = Cell.NO
            elif val == "—":
                claims[lang][dim] = Cell.NA
            else:
                claims[lang][dim] = Cell.MANUAL
    return claims


# ───────── main ─────────


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument("--only", type=str, default="",
                    help="Comma-separated README lang names to limit output (e.g. 'Rust,Kotlin')")
    ap.add_argument("--generate", action="store_true",
                    help="Emit a fresh markdown table from verified facts")
    ap.add_argument("--sample-repo", type=str, default=SAMPLE_REPO)
    args = ap.parse_args()

    ctx = AuditCtx(sample_repo=args.sample_repo, gnx_bin=GNX)
    readme_claims = parse_readme_claims()
    if not readme_claims:
        sys.stderr.write(f"!! could not parse README Language Matrix from {README}\n")
        return 2

    only = {s.strip() for s in args.only.split(",") if s.strip()}
    langs = list(LANG_DIRS.keys())
    if only:
        langs = [l for l in langs if l in only]

    if args.generate:
        print("| Language | " + " | ".join(PREDICATES.keys()) + " |")
        print("| :--- | " + " | ".join([":---:"] * len(PREDICATES)) + " |")
        for lang in langs:
            lang_dir, lang_path = LANG_DIRS[lang]
            cells = []
            for dim, pred in PREDICATES.items():
                v = pred(lang_path, lang_dir, ctx)
                cells.append(v.cell.value)
            print(f"| {lang} | " + " | ".join(cells) + " |")
        return 0

    # Drift report
    print(f"{'lang':<12} {'dim':<11} {'README':>7} {'verified':>9}  evidence")
    print("-" * 80)
    drift = 0
    manual = 0
    for lang in langs:
        lang_dir, lang_path = LANG_DIRS[lang]
        claims = readme_claims.get(lang, {})
        for dim, pred in PREDICATES.items():
            v = pred(lang_path, lang_dir, ctx)
            claim = claims.get(dim, Cell.MANUAL)
            if v.cell == Cell.MANUAL:
                manual += 1
                continue
            if claim != v.cell:
                drift += 1
                print(f"{lang:<12} {dim:<11} {claim.value:>7} {v.cell.value:>9}  {v.evidence}")
    print("-" * 80)
    print(f"\n{drift} drift cell(s) — README claim ≠ verified state.")
    print(f"{manual} manual cell(s) — predicate returns MANUAL (Exports/Types/Config/Frameworks).")
    if drift > 0:
        print("\nUse --generate to print a fresh table for selected langs.")
    return 1 if drift > 0 else 0


if __name__ == "__main__":
    sys.exit(main())
