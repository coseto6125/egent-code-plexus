#!/usr/bin/env python3
"""Parity drift gate — diff current gnx output against a saved baseline.

Run from gitnexus-rs repo root after building / installing a new `gnx`:

    python3 scripts/parity/check_drift.py [--threshold-abs 200]

The script re-runs `dump_per_lang_kinds.py`, compares per-lang absolute
deltas against `scripts/parity/final_baseline.txt`, and exits non-zero
when any lang's |delta| has worsened by more than the thresholds.

Use this before pushing parser changes to catch silent regressions like
the ones that motivated Phase C investigation (Phase B's Kotlin
@variable scope tightening dropped 2684 nodes without alerting anyone).

Exit codes:
    0 — within tolerance for every lang
    1 — at least one lang regressed beyond threshold
    2 — script / environment failure (missing baseline, gnx not installed, …)

Tolerances (combined OR):
    --threshold-abs   absolute shift in |delta| (default 200 nodes)
    --threshold-pct   relative shift in |delta| (default 0.10 = 10%)

A lang's `|delta|` is `abs(rs_total - ref_total)` — distance from ref.
A regression is when current |delta| > baseline |delta| + threshold.
Improvements (current |delta| < baseline |delta|) are always allowed.
"""

from __future__ import annotations

import argparse
import re
import subprocess
import sys
from pathlib import Path

REPO_ROOT = Path(__file__).resolve().parent.parent.parent
BASELINE = REPO_ROOT / "scripts" / "parity" / "final_baseline.txt"
DUMP_SCRIPT = REPO_ROOT / "scripts" / "parity" / "dump_per_lang_kinds.py"

HEADER_RE = re.compile(
    r"=== (\w+)\s+\(rs total (\d+), ref total (\d+), delta ([+-]\d+)\) ==="
)


def parse_lang_deltas(text: str) -> dict[str, int]:
    """Returns {lang: signed_delta} from a dump file/string."""
    return {m.group(1): int(m.group(4)) for m in HEADER_RE.finditer(text)}


def run_dump() -> str:
    if not DUMP_SCRIPT.exists():
        sys.stderr.write(f"!! dump script missing: {DUMP_SCRIPT}\n")
        sys.exit(2)
    r = subprocess.run(
        ["python3", str(DUMP_SCRIPT)],
        capture_output=True,
        text=True,
        timeout=600,
        cwd=str(REPO_ROOT),
    )
    if r.returncode != 0:
        sys.stderr.write(
            f"!! dump_per_lang_kinds.py exited {r.returncode}\n"
            f"   stderr (last 400 chars): {r.stderr[-400:]}\n"
        )
        sys.exit(2)
    return r.stdout


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument("--threshold-abs", type=int, default=200)
    ap.add_argument("--threshold-pct", type=float, default=0.10)
    ap.add_argument("--baseline", type=Path, default=BASELINE)
    args = ap.parse_args()

    if not args.baseline.exists():
        sys.stderr.write(f"!! baseline file missing: {args.baseline}\n")
        return 2

    baseline_text = args.baseline.read_text()
    baseline_d = parse_lang_deltas(baseline_text)
    if not baseline_d:
        sys.stderr.write(f"!! could not parse any lang headers from {args.baseline}\n")
        return 2

    sys.stderr.write(f"[check_drift] running {DUMP_SCRIPT.name} …\n")
    current_text = run_dump()
    current_d = parse_lang_deltas(current_text)
    if not current_d:
        sys.stderr.write("!! current dump produced no lang headers\n")
        return 2

    regressions: list[tuple[str, int, int, int]] = []
    print(f"{'lang':<12} {'baseline':>10} {'current':>10} {'shift':>10}  status")
    print("-" * 56)
    for lang in sorted(set(baseline_d) | set(current_d)):
        b = baseline_d.get(lang, 0)
        c = current_d.get(lang, 0)
        shift = abs(c) - abs(b)
        if shift <= 0:
            status = "ok"
        elif shift > args.threshold_abs or (
            abs(b) > 0 and shift / abs(b) > args.threshold_pct
        ):
            status = "REGRESSION"
            regressions.append((lang, b, c, shift))
        else:
            status = "within-tol"
        print(f"{lang:<12} {b:>+10} {c:>+10} {shift:>+10}  {status}")

    print("-" * 56)
    if regressions:
        print(f"\n!! {len(regressions)} lang(s) regressed beyond threshold "
              f"(abs>{args.threshold_abs} OR pct>{args.threshold_pct*100:.0f}%):")
        for lang, b, c, shift in regressions:
            print(f"  - {lang}: baseline |delta|={abs(b)}, current |delta|={abs(c)}, shift +{shift}")
        print("\nIf this is an intentional change, regenerate the baseline:")
        print(f"  python3 scripts/parity/dump_per_lang_kinds.py > {args.baseline}")
        return 1

    print("\nall langs within tolerance — no regression.")
    return 0


if __name__ == "__main__":
    sys.exit(main())
