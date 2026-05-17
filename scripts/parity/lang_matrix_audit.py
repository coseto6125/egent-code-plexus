#!/usr/bin/env python3
"""3-way Language Matrix audit — spec.rs vs parser.rs vs runtime baseline.

The static spec.rs `CAPTURE_KIND` table is *not* the whole story for what
a language's parser emits. Languages with parser-side kind override
(e.g. Kotlin's `is_enum_class()` promoting Class → Enum, C#'s
`*Attribute` heritage check promoting Class → Annotation, Swift's
`is_class_method()` promoting Function → Method) emit kinds that don't
appear in the spec table. The 3-way audit makes those hidden paths
visible by cross-checking:

  L1 (spec)    — `phf::Map` literal in `<lang>/spec.rs::CAPTURE_KIND`
  L2 (parser)  — `NodeKind::<Kind>` references in `<lang>/parser.rs`
  L3 (runtime) — actual emit counts from `final_baseline.txt`
                 (or any dump produced by `dump_per_lang_kinds.py`)

Output is a per-(lang, kind) row marking each source Y/N, sorted by
disagreement type:

  YYY    — consistent (listed, mentioned, emitted)
  NYY    — hidden post-process kind (parser-only path)
  YYN    — dead spec entry (listed but never fires)
  YNY    — impossible (parser must reference to emit) — flag
  NNY    — unexplained runtime kind (not in spec or parser?)

Usage (from gitnexus-rs repo root):

    python3 scripts/parity/lang_matrix_audit.py
    python3 scripts/parity/lang_matrix_audit.py --runtime /path/to/dump.txt
    python3 scripts/parity/lang_matrix_audit.py --only Kotlin,Swift
    python3 scripts/parity/lang_matrix_audit.py --consistent  # show YYY/NNN too

By default only rows with disagreement are printed (the interesting
cells). Pass `--consistent` to dump the full matrix.
"""

from __future__ import annotations

import argparse
import re
import sys
from pathlib import Path

REPO_ROOT = Path(__file__).resolve().parent.parent.parent
ANALYZER_SRC = REPO_ROOT / "crates" / "graph-nexus-analyzer" / "src"
DEFAULT_RUNTIME = REPO_ROOT / "scripts" / "parity" / "final_baseline.txt"

# NodeKind variants that are *emit-capable symbol kinds*. Skipped:
# File / Folder / Section / Document / Process / Route / Import / EntryPoint
# — these are framework-level or document-structure kinds emitted via paths
# orthogonal to the spec table, so 3-way comparison on them is noise.
TRACKED_KINDS = {
    "Function", "Class", "Method", "Interface", "Constructor", "Property",
    "Variable", "Const", "Struct", "Enum", "Typedef", "Namespace", "Macro",
    "Annotation", "Trait", "Module", "Impl",
}

CAPTURE_KIND_BLOCK_RE = re.compile(
    r"CAPTURE_KIND\s*:\s*phf::Map[^=]*=\s*phf::phf_map!\s*\{(.*?)\};",
    re.DOTALL,
)
KIND_REF_RE = re.compile(r"NodeKind::(\w+)\b")
RUNTIME_HEADER_RE = re.compile(
    r"=== (\w+)\s+\(rs total \d+, ref total \d+, delta [+-]\d+\) ==="
)
RUNTIME_ROW_RE = re.compile(r"^\s+(\w+)\s+(\d+)\s+\d+\s+[+-]\d+", re.MULTILINE)


def scan_spec(lang_dir: Path) -> set[str]:
    spec = lang_dir / "spec.rs"
    if not spec.exists():
        return set()
    text = spec.read_text()
    m = CAPTURE_KIND_BLOCK_RE.search(text)
    body = m.group(1) if m else ""
    return {k for k in KIND_REF_RE.findall(body) if k in TRACKED_KINDS}


def scan_parser(lang_dir: Path) -> set[str]:
    parser = lang_dir / "parser.rs"
    if not parser.exists():
        return set()
    text = parser.read_text()
    return {k for k in KIND_REF_RE.findall(text) if k in TRACKED_KINDS}


def scan_runtime(dump_path: Path) -> dict[str, dict[str, int]]:
    """Parse `final_baseline.txt`-style dump → {lang: {kind: count}} (rs side only)."""
    if not dump_path.exists():
        return {}
    text = dump_path.read_text()
    result: dict[str, dict[str, int]] = {}
    current_lang: str | None = None
    for line in text.splitlines():
        h = RUNTIME_HEADER_RE.match(line)
        if h:
            current_lang = h.group(1)
            result[current_lang] = {}
            continue
        if current_lang is None:
            continue
        r = RUNTIME_ROW_RE.match(line)
        if r:
            kind, rs_count = r.group(1), int(r.group(2))
            if kind in TRACKED_KINDS:
                result[current_lang][kind] = rs_count
    return result


# Per-lang dir name → runtime baseline lang name (capital-case used in dump).
LANG_DIR_TO_RUNTIME = {
    "c_sharp": "CSharp",
    "cpp": "Cpp",
    "javascript": "JavaScript",
    "typescript": "TypeScript",
}


def runtime_lang_name(dir_name: str) -> str:
    return LANG_DIR_TO_RUNTIME.get(dir_name, dir_name.capitalize())


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument("--runtime", type=Path, default=DEFAULT_RUNTIME,
                    help="Path to runtime dump (default: scripts/parity/final_baseline.txt)")
    ap.add_argument("--only", type=str, default="",
                    help="Comma-separated lang dirs to limit output (e.g. 'kotlin,swift')")
    ap.add_argument("--consistent", action="store_true",
                    help="Include consistent (YYY / NNN) rows in output")
    args = ap.parse_args()

    lang_dirs = [p for p in sorted(ANALYZER_SRC.iterdir())
                 if p.is_dir() and (p / "parser.rs").exists()]
    if args.only:
        wanted = {s.strip() for s in args.only.split(",") if s.strip()}
        lang_dirs = [p for p in lang_dirs if p.name in wanted]

    runtime = scan_runtime(args.runtime)
    if not runtime:
        sys.stderr.write(
            f"!! runtime dump missing or unparseable: {args.runtime}\n"
            f"   regenerate via: python3 scripts/parity/dump_per_lang_kinds.py > {args.runtime}\n"
        )

    print(f"{'lang':<16} {'kind':<14} {'spec':>4} {'parser':>6} {'runtime':>8}  diagnosis")
    print("-" * 70)

    drift_count = 0
    for lang_dir in lang_dirs:
        spec_set = scan_spec(lang_dir)
        parser_set = scan_parser(lang_dir)
        rt_lang = runtime_lang_name(lang_dir.name)
        rt_counts = runtime.get(rt_lang, {})

        kinds = sorted(TRACKED_KINDS)
        for kind in kinds:
            in_spec = kind in spec_set
            in_parser = kind in parser_set
            rt_count = rt_counts.get(kind, 0)
            in_runtime = rt_count > 0

            triple = (in_spec, in_parser, in_runtime)
            if not args.consistent:
                if triple in {
                    (True, True, True),    # aligned (spec + parser-ref)
                    (True, False, True),   # aligned (spec-dispatched, post-LangSpec normal)
                    (False, False, False), # lang doesn't have this kind
                    (False, True, False),  # parser match arm, no emit on corpus
                    (True, False, False),  # spec lists, no emit — ambiguous (corpus lacks
                                           # instances OR spec entry unused). Too noisy
                                           # to surface; rely on YYN (with parser ref) to
                                           # flag truly dead entries.
                }:
                    continue

            diagnosis = _diagnose(triple, rt_count)
            if diagnosis.startswith("drift"):
                drift_count += 1
            s = "Y" if in_spec else "·"
            p = "Y" if in_parser else "·"
            r_str = f"{rt_count}" if rt_count > 0 else "·"
            print(f"{lang_dir.name:<16} {kind:<14} {s:>4} {p:>6} {r_str:>8}  {diagnosis}")

    print("-" * 70)
    print(f"\n{drift_count} drift row(s) reported. "
          "Hidden post-process kinds (NYY) are usually intentional — verify by reading parser.rs. "
          "Dead spec entries (YYN) suggest the capture isn't firing on this corpus.")
    return 0


def _diagnose(triple: tuple[bool, bool, bool], rt_count: int) -> str:
    """Classify a (spec, parser, runtime) triple.

    Post-Phase-B, parser.rs dispatches via the spec table without an
    explicit `NodeKind::X` reference — so YNY (spec lists, parser
    doesn't ref, runtime emits) is the *normal* spec-driven path, not
    drift. Only NYY (spec doesn't list, parser refs directly, runtime
    emits) marks a hidden post-process kind.
    """
    match triple:
        case (False, True, True):
            return "drift: hidden (parser post-process emits, spec doesn't list)"
        case (True, True, False):
            return "drift: dead spec entry (listed but no emit on this corpus)"
        case (False, False, True):
            return "drift: emit without spec or parser ref (impossible — check)"
        case (True, False, True):
            return "aligned (spec-dispatched)"
        case (True, True, True):
            return "aligned (spec + parser-ref)"
        case (False, False, False):
            return "lang doesn't have this kind"
        case (True, False, False):
            return "spec-only (listed, parser doesn't ref) — unused"
        case (False, True, False):
            return "parser-ref-only (match arm, no emit)"
        case _:
            return "unknown"


if __name__ == "__main__":
    sys.exit(main())
