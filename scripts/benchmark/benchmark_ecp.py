#!/usr/bin/env python3
"""Regression-style benchmark for the ecp CLI — runs every public subcommand
against a sample repo and reports wall-clock latency.

Typical use:
    python scripts/benchmark/benchmark_ecp.py                              # full sweep
    python scripts/benchmark/benchmark_ecp.py --runs 5 --json out.json     # CI mode
    python scripts/benchmark/benchmark_ecp.py --skip-cold                  # don't wipe index
"""

from __future__ import annotations

import argparse
import json
import os
import platform
import subprocess
import sys
import time
from dataclasses import asdict, dataclass, field
from pathlib import Path
from statistics import median

CMD_TIMEOUT_S = 600
# Resolve workspace dynamically: scripts/benchmark/benchmark_ecp.py → parent.parent → workspace root.
# Hard-coding `/home/enor/code-graph-nexus` would cargo-build main even when this
# script runs from a worktree, defeating the auto-rebuild check entirely.
WORKSPACE_ROOT = Path(__file__).resolve().parent.parent.parent
DEFAULT_BINARY = WORKSPACE_ROOT / "target" / "release" / "ecp"
# Bench fixtures live in the canonical repo, not per-worktree — worktrees never
# copy `.sample_repo` (multi-GB of polyglot test sources). Keep absolute so a
# bench run from a worktree still targets the canonical fixture.
DEFAULT_REPO = Path("/home/enor/code-graph-nexus/.sample_repo")
DEFAULT_GIT_REPO = Path("/home/enor/code-graph-nexus")


def _ensure_binary_fresh(binary: Path, *, skip: bool) -> None:
    """Auto-rebuild the binary so the bench never runs against a stale build.

    Mtime comparison against tracked .rs files is unreliable — `git checkout`
    can stamp src files older than the previous release binary, hiding real
    drift (post-PR fix already in main but binary still pre-fix). Cargo's
    own fingerprint check is the source of truth, so just invoke it: noop
    when up-to-date (<100ms), rebuild when source / deps moved.
    """
    if skip:
        return
    proc = subprocess.run(
        ["cargo", "build", "--release", "-p", "egent-code-plexus", "--bin", "ecp"],
        cwd=WORKSPACE_ROOT,
        capture_output=True,
        text=True,
        timeout=900,
    )
    if proc.returncode != 0:
        print(
            f"error: cargo build failed (rc={proc.returncode}):\n{proc.stderr}",
            file=sys.stderr,
        )
        sys.exit(1)
    if not binary.exists():
        print(f"error: cargo build succeeded but {binary} missing", file=sys.stderr)
        sys.exit(1)


def _admin_drop(binary: Path, repo: Path) -> None:
    """Issue `ecp admin drop --repo <repo>` to wipe this repo's index.

    Replaces the legacy `_resolve_index_dir` + `shutil.rmtree` pattern,
    which assumed an outdated `<home>/<name>/<branch>` layout — the
    current layout is `<home>/<dir-name>__<hash>/commits/<dirname>/`
    and the canonical wipe lives in the CLI itself. Quiet on not-indexed
    repos.
    """
    subprocess.run(
        [str(binary), "admin", "drop", "--repo", str(repo)],
        capture_output=True,
        text=True,
        timeout=30,
    )


@dataclass
class Sample:
    name: str
    cmd: list[str]
    cwd: str | None = None
    runs: list[float] = field(default_factory=list)
    err: str | None = None

    @property
    def median_s(self) -> float | None:
        return median(self.runs) if self.runs else None


def _fmt(seconds: float) -> str:
    return f"{seconds * 1000:>6.1f}ms" if seconds < 1 else f"{seconds:>6.2f}s "


def _run(cmd: list[str], cwd: Path | None) -> tuple[float, int, str, str]:
    start = time.perf_counter()
    try:
        proc = subprocess.run(cmd, cwd=cwd, capture_output=True, text=True, timeout=CMD_TIMEOUT_S)
    except subprocess.TimeoutExpired:
        return CMD_TIMEOUT_S, 124, "", f"timeout after {CMD_TIMEOUT_S}s"
    return time.perf_counter() - start, proc.returncode, proc.stdout, proc.stderr


def _bench(name: str, cmd: list[str], cwd: Path, runs: int) -> Sample:
    s = Sample(name=name, cmd=cmd, cwd=str(cwd))
    for _ in range(runs):
        elapsed, rc, _out, stderr = _run(cmd, cwd)
        if rc != 0:
            tail = stderr.strip().splitlines()[-1] if stderr.strip() else f"exit {rc}"
            s.err = tail[:160]
            break
        s.runs.append(elapsed)
    return s


def _probe_symbols(binary: Path, repo: Path) -> dict[str, str]:
    """Pick one Class + one Method from the graph for inspect/impact/query tests.

    Strategy: cypher `Class-[:HasMethod]->Method` first row supplies names plus
    file/kind disambiguators for later `impact --target` calls.
    """
    out: dict[str, str] = {}
    elapsed, rc, stdout, stderr = _run(
        [
            str(binary),
            "cypher",
            "MATCH (a:Class)-[:HasMethod]->(b:Method) RETURN a,b",
            "--format",
            "json",
            "--repo",
            str(repo),
        ],
        cwd=repo,
    )
    if rc != 0:
        return out
    try:
        data = json.loads(stdout)
        rows = data.get("rows", [])
    except (json.JSONDecodeError, AttributeError):
        return out
    if not rows:
        return out

    # columns: ["a.name", "a.kind", "a.filePath", "b.name", "b.kind", "b.filePath"]
    first = rows[0]
    out["class_name"] = first[0]
    out["class_kind"] = first[1]
    out["class_file"] = first[2]
    out["method_name"] = first[3]
    out["method_kind"] = first[4]
    out["method_file"] = first[5]
    return out


def _hardware() -> dict[str, object]:
    info: dict[str, object] = {
        "arch": platform.machine(),
        "platform": platform.platform(),
        "cpu_count_logical": os.cpu_count(),
    }
    try:
        with open("/proc/cpuinfo") as f:
            for line in f:
                if line.startswith("model name"):
                    info["cpu"] = line.split(":", 1)[1].strip()
                    break
    except FileNotFoundError:
        pass
    try:
        with open("/proc/meminfo") as f:
            for line in f:
                if line.startswith("MemTotal:"):
                    info["mem_gb"] = round(int(line.split()[1]) / 1024 / 1024, 1)
                    break
    except FileNotFoundError:
        pass
    return info


def _count_files_by_lang(repo: Path) -> dict[str, int]:
    """Map well-known extensions/basenames to language file counts."""
    ext_lang = {
        ".py": "Python",
        ".ts": "TypeScript",
        ".tsx": "TypeScript",
        ".js": "JavaScript",
        ".jsx": "JavaScript",
        ".rs": "Rust",
        ".go": "Go",
        ".java": "Java",
        ".kt": "Kotlin",
        ".cs": "C#",
        ".cpp": "C++",
        ".hpp": "C++",
        ".c": "C",
        ".h": "C",
        ".php": "PHP",
        ".rb": "Ruby",
        ".swift": "Swift",
        ".dart": "Dart",
        ".sh": "Bash",
        ".bash": "Bash",
        ".cr": "Crystal",
        ".cairo": "Cairo",
        ".move": "Move",
        ".nim": "Nim",
        ".sol": "Solidity",
        ".sql": "SQL",
        ".vy": "Vyper",
        ".zig": "Zig",
        ".lua": "Lua",
        ".v": "Verilog",
        ".sv": "Verilog",
        ".md": "Markdown",
        ".yml": "YAML",
        ".yaml": "YAML",
        ".tf": "HCL",
        ".hcl": "HCL",
    }
    counts: dict[str, int] = {}
    for p in repo.rglob("*"):
        if not p.is_file():
            continue
        name = p.name.lower()
        lang: str | None
        if name == "dockerfile" or name.startswith("dockerfile."):
            lang = "Dockerfile"
        elif name in {"docker-compose.yml", "docker-compose.yaml", "compose.yml", "compose.yaml"}:
            lang = "Docker Compose"
        else:
            lang = ext_lang.get(p.suffix.lower())
        if lang:
            counts[lang] = counts.get(lang, 0) + 1
    return counts


def _print_summary(samples: list[Sample]) -> None:
    bar = "─" * 78
    print(f"\n{bar}")
    print(f"{'phase':<28}{'median':>10}{'min':>10}{'max':>10}{'runs':>6}  status")
    print(bar)
    for s in samples:
        if s.runs:
            print(
                f"{s.name:<28}"
                f"{_fmt(s.median_s):>10}{_fmt(min(s.runs)):>10}{_fmt(max(s.runs)):>10}"
                f"{len(s.runs):>6}  ok"
            )
        else:
            print(f"{s.name:<28}{'FAIL':>10}{'':>10}{'':>10}{0:>6}  err: {s.err or '?'}")
    print(bar)


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument(
        "--repo", type=Path, default=DEFAULT_REPO, help="Target repo for analyze + query benchmarks"
    )
    ap.add_argument(
        "--git-repo",
        type=Path,
        default=DEFAULT_GIT_REPO,
        help="A real git repo for detect-changes (sample-repo isn't a git checkout)",
    )
    ap.add_argument(
        "--binary", type=Path, default=DEFAULT_BINARY, help="Path to the ecp release binary"
    )
    ap.add_argument(
        "--runs", type=int, default=3, help="Repeats per query command (analyze runs once)"
    )
    ap.add_argument("--json", type=Path, help="Write JSON result to this path")
    ap.add_argument(
        "--skip-cold",
        action="store_true",
        help="Don't delete the registry index dir before the first analyze",
    )
    ap.add_argument(
        "--no-build",
        action="store_true",
        help="Skip the auto `cargo build --release` step (use the existing binary as-is)",
    )
    args = ap.parse_args()
    args.repo = args.repo.resolve()
    args.git_repo = args.git_repo.resolve()
    args.binary = args.binary.resolve()

    _ensure_binary_fresh(args.binary, skip=args.no_build)
    if not args.binary.exists():
        print(
            f"error: {args.binary} missing — run "
            f"`cargo build --release -p egent-code-plexus --bin ecp`",
            file=sys.stderr,
        )
        return 1
    if not args.repo.is_dir():
        print(f"error: repo {args.repo} not found", file=sys.stderr)
        return 1

    hw = _hardware()
    lang_counts = _count_files_by_lang(args.repo)
    total_files = sum(1 for _ in args.repo.rglob("*") if _.is_file())

    print(f"binary  : {args.binary}")
    print(f"repo    : {args.repo}  ({total_files:,} files, {len(lang_counts)} languages)")
    print(
        f"langs   : {', '.join(f'{k}={v}' for k, v in sorted(lang_counts.items(), key=lambda kv: -kv[1]))}"
    )
    print(f"cpu     : {hw.get('cpu', '?')}  (logical {hw.get('cpu_count_logical')})")
    print(f"mem     : {hw.get('mem_gb', '?')} GiB")
    print(f"os      : {hw.get('platform')}")
    print()

    samples: list[Sample] = []

    # Phase 1: analyze (cold)
    if not args.skip_cold:
        print(f"→ ecp admin drop --repo {args.repo}")
        _admin_drop(args.binary, args.repo)
    label = "analyze (cold)" if not args.skip_cold else "analyze (baseline)"
    print(f"→ {label}")
    s = _bench(
        label, [str(args.binary), "admin", "index", "--repo", str(args.repo)], cwd=args.repo, runs=1
    )
    samples.append(s)
    if s.err:
        print(f"  FAIL: {s.err}")
        _print_summary(samples)
        return 2
    print(f"  {s.median_s:.2f}s")

    # Phase 2: analyze (incremental, hash-cache hot)
    print("→ analyze (incremental)")
    s = _bench(
        "analyze (incremental)",
        [str(args.binary), "admin", "index", "--repo", str(args.repo)],
        cwd=args.repo,
        runs=1,
    )
    samples.append(s)
    print(f"  {s.median_s:.3f}s" if s.runs else f"  FAIL: {s.err}")

    # Phase 2b: ensure git-repo is indexed for impact tests
    if args.git_repo.is_dir() and (args.git_repo / ".git").exists() and args.git_repo != args.repo:
        print(f"→ analyze git-repo: {args.git_repo}")
        _run([str(args.binary), "admin", "index", "--repo", str(args.git_repo)], cwd=args.git_repo)

    # Phase 3: probe a Class + Method for downstream tests
    print("→ probing graph for sample symbols")
    sym = _probe_symbols(args.binary, args.repo)
    if sym:
        print(f"  class={sym.get('class_name', '-')}  method={sym.get('method_name', '-')}")
    else:
        print("  none found — inspect/impact/query will be skipped")
    print()

    # Phase 4: query-shape commands.
    # cypher is "minimal cypher": single MATCH path with optional WHERE a.name='Val'.
    # No LIMIT / no count() / no aggregation.
    cypher_class_contains = (
        "MATCH (a:Class)-[:HasMethod]->(b:Method) "
        f"WHERE a.name='{sym.get('class_name', 'AppController')}' RETURN a,b"
    )
    queries: list[tuple[str, list[str], Path]] = [
        (
            "cypher Class->Method (one)",
            [str(args.binary), "cypher", cypher_class_contains, "--repo", str(args.repo)],
            args.repo,
        ),
        (
            "cypher Method-Calls->Method",
            [
                str(args.binary),
                "cypher",
                "MATCH (a:Method)-[:Calls]->(b:Method) "
                f"WHERE a.name='{sym.get('method_name', 'main')}' RETURN a,b",
                "--repo",
                str(args.repo),
            ],
            args.repo,
        ),
        ("routes", [str(args.binary), "routes", "--repo", str(args.repo)], args.repo),
        ("summary", [str(args.binary), "summary", "--repo", str(args.repo)], args.repo),
        (
            "summary --detailed",
            [str(args.binary), "summary", "--detailed", "--repo", str(args.repo)],
            args.repo,
        ),
        # COUNT(*) ungrouped exercises the Accumulator fast path that bypasses
        # per-row Binding cloning (FU-2026-05-23-006 sibling). Without this row
        # the bench can't detect aggregate-path regressions or wins.
        (
            "cypher count(*) ungrouped",
            [
                str(args.binary),
                "cypher",
                "MATCH (m:Method) RETURN count(*)",
                "--repo",
                str(args.repo),
            ],
            args.repo,
        ),
        # `<lit> IN m.decorators` exercises the predicate pushdown that walks
        # the archived rkyv slice directly (PR #426 / FU-006). 'Override' is
        # Java-specific but present in any Java/Kotlin subtree of .sample_repo.
        (
            "cypher decorator IN",
            [
                str(args.binary),
                "cypher",
                "MATCH (m:Method) WHERE 'Override' IN m.decorators RETURN count(*)",
                "--repo",
                str(args.repo),
            ],
            args.repo,
        ),
    ]
    if name := sym.get("class_name"):
        queries.append(
            (
                "inspect (Class)",
                [str(args.binary), "inspect", "--name", name, "--repo", str(args.repo)],
                args.repo,
            )
        )
    if name := sym.get("method_name"):
        queries.append(
            (
                "find (bm25)",
                [str(args.binary), "find", name, "--mode", "bm25", "--repo", str(args.repo)],
                args.repo,
            )
        )
    if name := sym.get("class_name"):
        queries.append(
            (
                "impact upstream",
                [
                    str(args.binary),
                    "impact",
                    "--target",
                    name,
                    "--kind",
                    sym.get("class_kind", "Class").lower(),
                    "--file_path",
                    sym.get("class_file", ""),
                    "--direction",
                    "upstream",
                    "--repo",
                    str(args.repo),
                ],
                args.repo,
            )
        )
    if name := sym.get("method_name"):
        queries.append(
            (
                "impact downstream",
                [
                    str(args.binary),
                    "impact",
                    "--target",
                    name,
                    "--kind",
                    sym.get("method_kind", "Method").lower(),
                    "--file_path",
                    sym.get("method_file", ""),
                    "--direction",
                    "downstream",
                    "--repo",
                    str(args.repo),
                ],
                args.repo,
            )
        )

    if args.git_repo.is_dir() and (args.git_repo / ".git").exists():
        queries.append(
            (
                "impact --baseline HEAD~1",
                [str(args.binary), "impact", "--baseline", "HEAD~1", "--repo", str(args.git_repo)],
                args.git_repo,
            )
        )

    for name, cmd, cwd in queries:
        print(f"→ {name}")
        s = _bench(name, cmd, cwd, args.runs)
        samples.append(s)
        if s.err:
            print(f"  FAIL: {s.err}")
        else:
            print(
                f"  median {_fmt(s.median_s).strip()}  "
                f"(min {_fmt(min(s.runs)).strip()}, max {_fmt(max(s.runs)).strip()})"
            )

    _print_summary(samples)

    if args.json:
        payload = {
            "binary": str(args.binary),
            "repo": str(args.repo),
            "total_files": total_files,
            "language_counts": lang_counts,
            "hardware": hw,
            "samples": [asdict(s) for s in samples],
        }
        args.json.write_text(json.dumps(payload, indent=2))
        print(f"\n→ wrote {args.json}")

    return 0 if all(s.runs for s in samples) else 3


if __name__ == "__main__":
    sys.exit(main())
