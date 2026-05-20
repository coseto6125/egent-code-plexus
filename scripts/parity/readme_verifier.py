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

Usage (from egent-code-plexus repo root):

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
DEFAULT_SAMPLE_REPO = REPO_ROOT / ".sample_repo"
DEFAULT_README = REPO_ROOT / "README.md"
ANALYZER_SRC = REPO_ROOT / "crates" / "ecp-analyzer" / "src"

# README lang name → (spec/parser dir name, list of file extensions).
#
# Extensions scope cypher counts to files the parser actually handles, not
# the bootstrap-clone directory the corpus was checked into. The previous
# `STARTS WITH '<dir>/'` scoping had two failure modes:
#   1. `JavaScript/` corpus (Express) is CommonJS-only → JS Heritage / Imports
#      counts read 0 even though the parser handles ES modules correctly
#      (proven by .js files in `TypeScript/sample/` and `solidity/test/`).
#   2. `STARTS WITH 'Java/'` would prefix-collide with `JavaScript/` paths,
#      double-counting Java's `*.java` against the JS row in pathological
#      sample layouts.
# Extension-based scoping treats `.js` as JavaScript wherever it lives,
# which is what the parser dispatch actually does.
LANG_DIRS: dict[str, tuple[str, list[str]]] = {
    "TypeScript": ("typescript", [".ts", ".tsx"]),
    "JavaScript": ("javascript", [".js", ".mjs", ".cjs", ".jsx"]),
    "Python": ("python", [".py", ".pyi"]),
    "Java": ("java", [".java"]),
    "Kotlin": ("kotlin", [".kt", ".kts"]),
    "C#": ("c_sharp", [".cs"]),
    "Go": ("go", [".go"]),
    "Rust": ("rust", [".rs"]),
    "PHP": ("php", [".php"]),
    "Ruby": ("ruby", [".rb"]),
    "Swift": ("swift", [".swift"]),
    # `.h` is ambiguous (C or C++); the C row claims it via tree-sitter-c
    # being the parser dispatch default. C++ takes the cpp-specific
    # variants so the count is mutually exclusive enough.
    "C": ("c", [".c", ".h"]),
    "C++": ("cpp", [".cpp", ".cc", ".cxx", ".hpp", ".hh", ".hxx"]),
    "Dart": ("dart", [".dart"]),
}


def _ext_clause(file_exts: list[str], var: str = "a") -> str:
    """Build a parenthesized OR clause matching any of the extensions.

    Returns e.g. `(a.filePath ENDS WITH '.js' OR a.filePath ENDS WITH '.mjs')`
    — always parenthesized so it can be AND-combined with other WHERE
    conditions without operator-precedence surprises.
    """
    inner = " OR ".join(f"{var}.filePath ENDS WITH '{ext}'" for ext in file_exts)
    return f"({inner})"


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
    ecp_bin: str

    def cypher_count(self, query: str) -> int:
        """Run a `RETURN count(*)`-shaped cypher; return the integer.

        ecp cypher's JSON output is inconsistent for single-column results
        (sometimes `rows: [N]`, sometimes `rows: [[N]]`); this handles both.
        Returns 0 on any error — the verifier treats 0 as `NO`, the user
        re-runs the predicate manually to investigate.
        """
        r = subprocess.run(
            [self.ecp_bin, "cypher", "--repo", self.sample_repo, query, "--format=json"],
            capture_output=True,
            text=True,
            timeout=30,
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


# All predicates share the (file_exts, lang_dir, ctx) signature so the
# PREDICATES dispatch table can hold function references directly — no
# bespoke lambdas, no per-entry signature drift. Predicates that don't
# need a particular arg take `_` for it.
def dim_imports(file_exts: list[str], _lang_dir: str, ctx: AuditCtx) -> Verdict:
    """'Imports' = ≥1 `Imports` edge originating in a file of this lang.

    Imports are tracked as edges (file -[:Imports]-> module), not as a
    standalone `NodeKind::Import`. ref-gitnexus emits `Import` nodes
    but ecp models them as relationships.
    """
    where = _ext_clause(file_exts, "a")
    n = ctx.cypher_count(f"MATCH (a)-[:Imports]->(b) WHERE {where} RETURN count(*)")
    return Verdict(Cell.YES if n > 0 else Cell.NO, f"{n} Imports edges")


SYMBOL_KINDS = [
    "Function",
    "Class",
    "Method",
    "Interface",
    "Constructor",
    "Property",
    "Variable",
    "Const",
    "Struct",
    "Enum",
    "Typedef",
    "Macro",
    "Annotation",
    "Trait",
    "Module",
    "Namespace",
]


def dim_named(file_exts: list[str], _lang_dir: str, ctx: AuditCtx) -> Verdict:
    """'Named' = ≥1 symbol-kind node in a file of this lang.

    Symbol kinds = Function / Class / Method / … (excludes File / Import /
    Route / EntryPoint / Process — those have their own dimensions or
    aren't "named symbols").
    """
    kinds_q = ", ".join(f"'{k}'" for k in SYMBOL_KINDS)
    where = _ext_clause(file_exts, "n")
    n = ctx.cypher_count(f"MATCH (n) WHERE n.kind IN [{kinds_q}] AND {where} RETURN count(*)")
    return Verdict(Cell.YES if n > 0 else Cell.NO, f"{n} named-symbol nodes")


def dim_heritage(file_exts: list[str], _lang_dir: str, ctx: AuditCtx) -> Verdict:
    """'Heritage' = ≥1 Extends or Implements edge originating in this lang."""
    where = _ext_clause(file_exts, "a")
    n = ctx.cypher_count(f"MATCH (a)-[:Extends|Implements]->(b) WHERE {where} RETURN count(*)")
    return Verdict(Cell.YES if n > 0 else Cell.NO, f"{n} Extends/Implements edges")


def dim_ctor(file_exts: list[str], _lang_dir: str, ctx: AuditCtx) -> Verdict:
    """'Ctor' = ≥1 node with kind=Constructor in a file of this lang."""
    where = _ext_clause(file_exts, "n")
    n = ctx.cypher_count(f"MATCH (n) WHERE n.kind='Constructor' AND {where} RETURN count(*)")
    return Verdict(Cell.YES if n > 0 else Cell.NO, f"{n} Constructor nodes")


def dim_entry(file_exts: list[str], _lang_dir: str, ctx: AuditCtx) -> Verdict:
    """'Entry' = ≥1 node with kind=EntryPoint in a file of this lang."""
    where = _ext_clause(file_exts, "n")
    n = ctx.cypher_count(f"MATCH (n) WHERE n.kind='EntryPoint' AND {where} RETURN count(*)")
    return Verdict(Cell.YES if n > 0 else Cell.NO, f"{n} EntryPoint nodes")


def dim_call(file_exts: list[str], _lang_dir: str, ctx: AuditCtx) -> Verdict:
    """'Call' = ≥1 Calls edge originating in this lang."""
    where = _ext_clause(file_exts, "a")
    n = ctx.cypher_count(f"MATCH (a)-[:Calls]->(b) WHERE {where} RETURN count(*)")
    return Verdict(Cell.YES if n > 0 else Cell.NO, f"{n} Calls edges")


def dim_rename(_file_exts: list[str], lang_dir: str, _ctx: AuditCtx) -> Verdict:
    """'Rename' = identifier_finder module exists for this lang.

    Code-level check: `ecp rename` dispatches per-lang via the
    `identifier_finder/<lang>.rs` module. Presence of that file is the
    proxy for rename support — without it the rename command lacks the
    lang-specific identifier-range table and exits no-op.
    """
    p = ANALYZER_SRC / "identifier_finder" / f"{lang_dir}.rs"
    exists = p.exists()
    return Verdict(
        Cell.YES if exists else Cell.NO,
        f"identifier_finder/{lang_dir}.rs {'exists' if exists else 'missing'}",
    )


# Dimensions not yet auto-checkable. Cypher can't access RawNode fields
# directly (`is_exported`, `type_annotation`, `heritage` list), and
# `Config` / `Frameworks` lack a canonical NodeKind / edge type. Return
# MANUAL — the verifier flags drift but doesn't auto-fail.
def dim_manual(reason: str):
    def predicate(_file_exts: list[str], _lang_dir: str, _ctx: AuditCtx) -> Verdict:
        return Verdict(Cell.MANUAL, reason)

    return predicate


PREDICATES = {
    "Imports": dim_imports,
    "Named": dim_named,
    "Exports": dim_manual("cypher can't read n.is_exported"),
    "Heritage": dim_heritage,
    "Types": dim_manual("cypher can't read n.type_annotation"),
    "Ctor": dim_ctor,
    "Config": dim_manual("no canonical Config NodeKind"),
    "Frameworks": dim_manual("framework_refs not exposed via cypher"),
    "Entry": dim_entry,
    "Call": dim_call,
    "Rename": dim_rename,
}


# ───────── README parser ─────────


README_ROW_RE = re.compile(
    r"^\|\s*([A-Za-z0-9#+]+(?:\s*[A-Za-z0-9#+/-]+)?)\s*\|"  # lang cell
    r"(\s*[^|]+\|){11}"  # 11 dimension cells
    r"\s*$"
)


def parse_readme_claims(readme: Path) -> dict[str, dict[str, Cell]]:
    """Parse `## Language Matrix` table → {lang: {dim: Cell}}."""
    if not readme.exists():
        return {}
    lines = readme.read_text().splitlines()
    header_idx = next(
        (i for i, line in enumerate(lines) if line.startswith("| Language | Imports | Named")),
        None,
    )
    if header_idx is None:
        return {}
    cols = [c.strip() for c in lines[header_idx].split("|")[1:-1]]
    dim_cols = cols[1:]  # drop "Language"
    claims: dict[str, dict[str, Cell]] = {}
    for line in lines[header_idx + 2 :]:  # skip separator row
        if not line.startswith("|"):
            break
        cells = [c.strip() for c in line.split("|")[1:-1]]
        if len(cells) != len(cols):
            continue
        lang = cells[0]
        if not lang or lang.startswith("─"):
            continue
        # zip(strict=True) is redundant given the length check above but
        # documents that header / row column counts MUST agree — a defence
        # against the length check ever being weakened in a refactor.
        claims[lang] = {
            dim: _cell_from_str(val) for dim, val in zip(dim_cols, cells[1:], strict=True)
        }
    return claims


def _cell_from_str(val: str) -> Cell:
    match val:
        case "✓":
            return Cell.YES
        case "☐":
            return Cell.NO
        case "—":
            return Cell.NA
        case _:
            return Cell.MANUAL


# ───────── main ─────────


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument(
        "--only",
        type=str,
        default="",
        help="Comma-separated README lang names to limit output (e.g. 'Rust,Kotlin')",
    )
    ap.add_argument(
        "--generate", action="store_true", help="Emit a fresh markdown table from verified facts"
    )
    ap.add_argument(
        "--sample-repo",
        type=Path,
        default=DEFAULT_SAMPLE_REPO,
        help="Path to .sample_repo (default: <repo>/.sample_repo)",
    )
    ap.add_argument(
        "--ecp-bin", type=str, default="ecp", help="ecp binary (default: 'ecp' from PATH)"
    )
    ap.add_argument(
        "--readme",
        type=Path,
        default=DEFAULT_README,
        help="README to verify (default: <repo>/README.md; "
        "use README_zh-TW.md to check the zh-TW variant)",
    )
    args = ap.parse_args()

    ctx = AuditCtx(sample_repo=str(args.sample_repo), ecp_bin=args.ecp_bin)
    readme_claims = parse_readme_claims(args.readme)
    if not readme_claims:
        sys.stderr.write(f"!! could not parse README Language Matrix from {args.readme}\n")
        return 2

    only = {s.strip() for s in args.only.split(",") if s.strip()}
    langs = list(LANG_DIRS.keys())
    if only:
        langs = [lang for lang in langs if lang in only]

    if args.generate:
        print("| Language | " + " | ".join(PREDICATES.keys()) + " |")
        print("| :--- | " + " | ".join([":---:"] * len(PREDICATES)) + " |")
        for lang in langs:
            lang_dir, file_exts = LANG_DIRS[lang]
            cells = [pred(file_exts, lang_dir, ctx).cell.value for pred in PREDICATES.values()]
            print(f"| {lang} | " + " | ".join(cells) + " |")
        return 0

    # Drift report
    print(f"{'lang':<12} {'dim':<11} {'README':>7} {'verified':>9}  evidence")
    print("-" * 80)
    drift = 0
    manual = 0
    for lang in langs:
        lang_dir, file_exts = LANG_DIRS[lang]
        claims = readme_claims.get(lang, {})
        for dim, pred in PREDICATES.items():
            v = pred(file_exts, lang_dir, ctx)
            claim = claims.get(dim, Cell.MANUAL)
            if v.cell == Cell.MANUAL:
                manual += 1
                continue
            # README `—` (NA) means "this language doesn't have the concept";
            # the expected emission count is zero. A `NO` (☐) verdict from the
            # predicate IS the proof of NA — not a drift. Only flag NA-vs-YES
            # (unexpected emission) as drift on this row.
            if claim == Cell.NA and v.cell == Cell.NO:
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
