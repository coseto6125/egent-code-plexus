#!/usr/bin/env python3
"""Head-to-head perf benchmark: gitnexus (upstream Node CLI) vs ecp (this Rust port).

Verb mapping (commands compared side-by-side):

    cold index         gitnexus analyze        ecp admin index
    one-symbol context gitnexus context <n>    ecp inspect <n>
    blast radius       gitnexus impact <n>     ecp impact <n> --direction up
    raw cypher         gitnexus cypher '<q>'   ecp cypher '<q>'

Usage:
    python scripts/parity/benchmark_vs_gitnexus.py
    python scripts/parity/benchmark_vs_gitnexus.py --runs 5 --json out.json
    python scripts/parity/benchmark_vs_gitnexus.py --skip-cold     # use existing indexes
"""

from __future__ import annotations

import argparse
import json
import re
import subprocess
import sys
import time
from dataclasses import asdict, dataclass, field
from pathlib import Path
from statistics import median

CMD_TIMEOUT_S = 1800  # cold-analyze can take 10+ min for gitnexus on 22k-file corpus
WORKSPACE_ROOT = Path(__file__).resolve().parent.parent.parent
DEFAULT_ECP = WORKSPACE_ROOT / "target" / "release" / "ecp"
DEFAULT_GITNEXUS = Path.home() / ".npm-global" / "bin" / "gitnexus"
DEFAULT_REPO = Path("/home/enor/code-graph-nexus/.sample_repo")


@dataclass
class Sample:
    """One benched command (one tool × one phase × N runs)."""

    tool: str  # "ecp" | "gitnexus"
    phase: str  # "cold-index" | "context" | "impact" | "cypher"
    cmd: list[str]
    cwd: str | None = None
    runs: list[float] = field(default_factory=list)
    err: str | None = None
    stdout_bytes: int = 0  # output size for the LAST run (token-cost proxy)

    @property
    def median_s(self) -> float | None:
        return median(self.runs) if self.runs else None


def _fmt(seconds: float | None) -> str:
    if seconds is None:
        return "    FAIL"
    return f"{seconds * 1000:>7.1f} ms" if seconds < 1 else f"{seconds:>7.2f} s "


def _run_one(cmd: list[str], cwd: Path | None, timeout_s: int) -> tuple[float, int, bytes, bytes]:
    start = time.perf_counter()
    try:
        proc = subprocess.run(cmd, cwd=cwd, capture_output=True, timeout=timeout_s)
    except subprocess.TimeoutExpired:
        return time.perf_counter() - start, 124, b"", b"timeout"
    return time.perf_counter() - start, proc.returncode, proc.stdout, proc.stderr


def _bench(
    tool: str, phase: str, cmd: list[str], cwd: Path, runs: int, timeout_s: int = CMD_TIMEOUT_S
) -> Sample:
    s = Sample(tool=tool, phase=phase, cmd=cmd, cwd=str(cwd))
    last_stdout = b""
    for _ in range(runs):
        elapsed, rc, stdout, stderr = _run_one(cmd, cwd, timeout_s)
        if rc != 0:
            s.err = (stderr or stdout).decode("utf-8", errors="replace")[:200]
            return s
        s.runs.append(elapsed)
        last_stdout = stdout
    s.stdout_bytes = len(last_stdout)
    return s


def _drop_ecp(ecp: Path, repo: Path) -> None:
    subprocess.run(
        [str(ecp), "admin", "drop", "--repo", str(repo)], capture_output=True, timeout=30
    )


def _drop_gitnexus(repo: Path) -> None:
    # gitnexus clean operates on cwd-resolved repo. Run from inside .sample_repo
    # and use --skip-git semantics so it targets the sample_repo, not the parent.
    subprocess.run(["gitnexus", "clean", "--force"], cwd=repo, capture_output=True, timeout=60)


def _probe_shared_symbol(ecp: Path, gn: Path, repo: Path) -> dict[str, str]:
    """Find a symbol both tools have indexed under their own schema.

    gitnexus and ecp label Rust types differently: ecp folds Rust `struct` /
    `enum` / `trait` into `Class`, while gitnexus keeps them separate as
    `Struct` / `Enum` / `Trait`. Iterate ecp's `Class` candidates and pick
    the first one gitnexus can find (probably under `Struct`).
    """
    proc = subprocess.run(
        [
            str(ecp),
            "cypher",
            "MATCH (a:Class) RETURN a.name LIMIT 50",
            "--format",
            "json",
            "--repo",
            str(repo),
        ],
        capture_output=True,
        text=True,
        timeout=60,
    )
    if proc.returncode != 0:
        return {}
    try:
        rows = json.loads(proc.stdout).get("rows", [])
    except json.JSONDecodeError:
        return {}
    # When SELECT returns a single column, ecp cypher rows are flat strings
    # (not nested arrays). Drop single-char names + names with spaces /
    # special chars — those are typically workflow-name labels rather than
    # canonical code symbols both tools will agree on.
    names = [r for r in rows if isinstance(r, str) and len(r) >= 4 and r.replace("_", "").isalnum()]
    # Probe gitnexus for each candidate. Match success by parsing the
    # `row_count` field; gitnexus emits `"row_count": 1` (or higher) on a hit
    # and `"row_count": 0` (or omits the field) on a miss.
    row_count_re = re.compile(r'"row_count":\s*(\d+)')
    for name in names:
        check = subprocess.run(
            [
                str(gn),
                "cypher",
                f"MATCH (a) WHERE a.name='{name}' RETURN a LIMIT 1",
                "--repo",
                str(repo),
            ],
            capture_output=True,
            text=True,
            timeout=30,
        )
        if check.returncode != 0:
            continue
        m = row_count_re.search(check.stdout)
        if m and int(m.group(1)) >= 1:
            return {"class_name": name}
    return {}


def _hardware() -> dict[str, str]:
    import os
    import platform

    try:
        with open("/proc/cpuinfo") as f:
            cpu = next(
                (line.split(":", 1)[1].strip() for line in f if line.startswith("model name")), "?"
            )
    except OSError:
        cpu = "?"
    try:
        with open("/proc/meminfo") as f:
            kb = next((int(line.split()[1]) for line in f if line.startswith("MemTotal")), 0)
        mem = f"{kb / 1024 / 1024:.1f} GiB"
    except OSError:
        mem = "?"
    return {
        "cpu": cpu,
        "cpu_count_logical": str(os.cpu_count() or "?"),
        "mem": mem,
        "platform": platform.platform(),
    }


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument("--ecp-binary", type=Path, default=DEFAULT_ECP)
    ap.add_argument("--gitnexus-binary", type=Path, default=DEFAULT_GITNEXUS)
    ap.add_argument("--repo", type=Path, default=DEFAULT_REPO)
    ap.add_argument("--runs", type=int, default=3, help="runs per query phase")
    ap.add_argument(
        "--skip-cold",
        action="store_true",
        help="reuse existing indexes; only measure per-query latency",
    )
    ap.add_argument("--json", type=Path, help="dump full results to this path")
    ap.add_argument(
        "--symbol", type=str, help="explicit symbol to use for per-query phases (skips auto-probe)"
    )
    args = ap.parse_args()

    if not args.ecp_binary.exists():
        print(f"error: ecp binary not at {args.ecp_binary}", file=sys.stderr)
        return 1
    if not args.gitnexus_binary.exists():
        print(f"error: gitnexus binary not at {args.gitnexus_binary}", file=sys.stderr)
        return 1
    if not args.repo.is_dir():
        print(f"error: repo not a dir: {args.repo}", file=sys.stderr)
        return 1

    hw = _hardware()
    print(f"ecp       : {args.ecp_binary}")
    print(
        f"gitnexus  : {args.gitnexus_binary}  (v{subprocess.run([str(args.gitnexus_binary), '--version'], capture_output=True, text=True).stdout.strip()})"
    )
    print(f"repo      : {args.repo}")
    print(f"cpu       : {hw['cpu']}  (logical {hw['cpu_count_logical']})")
    print(f"mem       : {hw['mem']}")
    print()

    samples: list[Sample] = []

    # ── Phase 1: cold index ────────────────────────────────────────────────
    if not args.skip_cold:
        print("→ drop both indexes")
        _drop_ecp(args.ecp_binary, args.repo)
        _drop_gitnexus(args.repo)

        print("→ ecp cold index")
        s = _bench(
            "ecp",
            "cold-index",
            [str(args.ecp_binary), "admin", "index", "--repo", str(args.repo)],
            cwd=args.repo,
            runs=1,
        )
        samples.append(s)
        print(f"  {_fmt(s.median_s)}" if s.runs else f"  FAIL: {s.err}")

        print("→ gitnexus cold analyze")
        s = _bench(
            "gitnexus",
            "cold-index",
            [str(args.gitnexus_binary), "analyze", str(args.repo), "--skip-git"],
            cwd=args.repo,
            runs=1,
            timeout_s=CMD_TIMEOUT_S,
        )
        samples.append(s)
        print(f"  {_fmt(s.median_s)}" if s.runs else f"  FAIL: {s.err}")

    # ── Phase 2: probe a symbol both tools can target ──────────────────────
    if args.symbol:
        class_name = args.symbol
        print(f"→ using explicit --symbol {class_name}")
    else:
        print("→ probe shared symbol (ecp Class ⇄ gitnexus Class|Struct|Enum)")
        sym = _probe_shared_symbol(args.ecp_binary, args.gitnexus_binary, args.repo)
        if not sym:
            print("  FAIL: no shared symbol; can't run per-query phases")
            _print_summary(samples)
            return 2
        class_name = sym["class_name"]
        print(f"  symbol={class_name}")
    print()

    # gitnexus globally indexes many repos and requires --repo to disambiguate;
    # pass the absolute path which it accepts as a repo identifier.
    gn_repo = ["--repo", str(args.repo)]

    # ── Phase 3: per-query latency ─────────────────────────────────────────
    # Unified phase names (`symbol-context`, `blast-radius`, `cypher`) so ecp
    # and gitnexus rows merge into a single row per phase in the summary.
    queries: list[tuple[str, str, list[str], Path]] = [
        (
            "ecp",
            "symbol-context",
            [str(args.ecp_binary), "inspect", "--name", class_name, "--repo", str(args.repo)],
            args.repo,
        ),
        (
            "gitnexus",
            "symbol-context",
            [str(args.gitnexus_binary), "context", class_name, *gn_repo],
            args.repo,
        ),
        (
            "ecp",
            "blast-radius",
            [
                str(args.ecp_binary),
                "impact",
                class_name,
                "--direction",
                "up",
                "--repo",
                str(args.repo),
            ],
            args.repo,
        ),
        (
            "gitnexus",
            "blast-radius",
            [str(args.gitnexus_binary), "impact", class_name, *gn_repo],
            args.repo,
        ),
        # cypher — use a schema-agnostic `MATCH (a)` so both tools answer
        # against their own labeling (ecp: Class; gitnexus: Struct/Class/Enum).
        (
            "ecp",
            "cypher",
            [
                str(args.ecp_binary),
                "cypher",
                f"MATCH (a) WHERE a.name='{class_name}' RETURN a",
                "--repo",
                str(args.repo),
            ],
            args.repo,
        ),
        (
            "gitnexus",
            "cypher",
            [
                str(args.gitnexus_binary),
                "cypher",
                f"MATCH (a) WHERE a.name='{class_name}' RETURN a",
                *gn_repo,
            ],
            args.repo,
        ),
    ]

    for tool, phase, cmd, cwd in queries:
        print(f"→ {tool} {phase}")
        s = _bench(tool, phase, cmd, cwd, runs=args.runs)
        samples.append(s)
        if s.err:
            print(f"  FAIL: {s.err.strip()}")
        else:
            print(f"  {_fmt(s.median_s)}  (out {s.stdout_bytes} B)")

    print()
    _print_summary(samples)

    if args.json:
        payload = {
            "hardware": hw,
            "repo": str(args.repo),
            "samples": [asdict(s) for s in samples],
        }
        args.json.write_text(json.dumps(payload, indent=2))
        print(f"→ wrote {args.json}")

    return 0 if all(s.runs for s in samples) else 3


def _print_summary(samples: list[Sample]) -> None:
    # Group by phase, then show ecp vs gitnexus side-by-side.
    phases: dict[str, dict[str, Sample]] = {}
    for s in samples:
        phases.setdefault(s.phase, {})[s.tool] = s

    print(f"{'─' * 78}")
    print(f"{'phase':<14} {'ecp':>22} {'gitnexus':>22} {'speedup':>14}")
    print(f"{'─' * 78}")
    for phase, tools in phases.items():
        ecp = tools.get("ecp")
        gn = tools.get("gitnexus")
        ecp_med = ecp.median_s if ecp and ecp.runs else None
        gn_med = gn.median_s if gn and gn.runs else None
        speedup = "—"
        if ecp_med and gn_med:
            speedup = f"{gn_med / ecp_med:.1f}×"
        ecp_str = _fmt(ecp_med) if ecp else " (skipped)"
        gn_str = _fmt(gn_med) if gn else " (skipped)"
        if ecp and ecp.stdout_bytes:
            ecp_str = f"{ecp_str} ({ecp.stdout_bytes}B)"
        if gn and gn.stdout_bytes:
            gn_str = f"{gn_str} ({gn.stdout_bytes}B)"
        print(f"{phase:<14} {ecp_str:>22} {gn_str:>22} {speedup:>14}")
    print(f"{'─' * 78}")


if __name__ == "__main__":
    sys.exit(main())
