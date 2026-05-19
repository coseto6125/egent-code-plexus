#!/usr/bin/env python3
"""Manual verification helper — `dump_per_lang_kinds.py` wrapper that
prints a per-lang shift table against `final_baseline.txt`.

Run from gitnexus-rs repo root before pushing a parser change:

    python3 scripts/parity/check_drift.py

Eyeball the `shift` column:

  - Improvements (negative shift) are always OK.
  - Positive shifts that you *intended* (e.g. you tightened a capture)
    mean it's time to regenerate the baseline:
        cgn admin drop --repo .sample_repo
        cgn admin index --repo .sample_repo
        python3 scripts/parity/dump_per_lang_kinds.py > scripts/parity/final_baseline.txt
    Commit the new baseline alongside the parser change.
  - Positive shifts you didn't expect → silent regression candidate.
    Investigate before pushing.

The script exits 0 in normal use; nothing is "gated" automatically.
Phase B's Kotlin `@variable` scope tightening dropped 2 684 nodes
without anyone noticing because no one re-ran the comparison —
running this script is the discipline that catches it. It's not a
CI gate; it's a tool to make the discipline cheap.

CI integration hook (opt-in):
    --strict           exit 1 on any positive shift exceeding thresholds
    --threshold-abs N  absolute shift in |delta| (default 200)
    --threshold-pct F  relative shift in |delta| (default 0.10 = 10%)

Exit codes:
    0 — script ran (default; or strict mode with no regressions)
    1 — strict mode + at least one lang regressed beyond threshold
    2 — script / environment failure (missing baseline, dump errored, …)
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
    ap.add_argument("--strict", action="store_true",
                    help="Exit 1 on threshold-exceeding shifts (CI / pre-push gate mode).")
    ap.add_argument("--threshold-abs", type=int, default=200,
                    help="Absolute shift threshold; only used with --strict.")
    ap.add_argument("--threshold-pct", type=float, default=0.10,
                    help="Relative shift threshold (fraction); only used with --strict.")
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
    print(f"{'lang':<12} {'baseline':>10} {'current':>10} {'shift':>10}  note")
    print("-" * 56)
    for lang in sorted(set(baseline_d) | set(current_d)):
        b = baseline_d.get(lang, 0)
        c = current_d.get(lang, 0)
        shift = abs(c) - abs(b)
        if shift < 0:
            note = "improved"
        elif shift == 0:
            note = "same"
        elif args.strict and (
            shift > args.threshold_abs
            or (abs(b) > 0 and shift / abs(b) > args.threshold_pct)
        ):
            note = "OVER THRESHOLD"
            regressions.append((lang, b, c, shift))
        else:
            note = "drift+"
        print(f"{lang:<12} {b:>+10} {c:>+10} {shift:>+10}  {note}")

    print("-" * 56)
    print(
        "\nEyeball the `shift` column. Positive shifts you didn't intend → "
        "investigate. Positive shifts you *did* intend → regenerate the "
        "baseline (see docstring)."
    )

    if args.strict and regressions:
        print(
            f"\n!! [--strict] {len(regressions)} lang(s) over threshold "
            f"(abs>{args.threshold_abs} OR pct>{args.threshold_pct*100:.0f}%):"
        )
        for lang, b, c, shift in regressions:
            print(f"  - {lang}: baseline |delta|={abs(b)}, current |delta|={abs(c)}, shift +{shift}")
        return 1

    return 0


if __name__ == "__main__":
    sys.exit(main())
