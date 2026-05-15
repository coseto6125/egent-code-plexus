#!/usr/bin/env python3
"""Regression-style benchmark for the gnx CLI — runs every public subcommand
against a sample repo and reports wall-clock latency.

Typical use:
    python scripts/benchmark_gnx.py                              # full sweep
    python scripts/benchmark_gnx.py --runs 5 --json out.json     # CI mode
    python scripts/benchmark_gnx.py --skip-cold                  # don't wipe index
"""

from __future__ import annotations

import argparse
import json
import os
import platform
import shutil
import subprocess
import sys
import time
from dataclasses import asdict, dataclass, field
from pathlib import Path
from statistics import median

DEFAULT_REPO = Path("/home/enor/gitnexus-rs/.sample_repo")
DEFAULT_GIT_REPO = Path("/home/enor/gitnexus-rs")
DEFAULT_BINARY = Path("/home/enor/gitnexus-rs/target/release/gnx")
DEFAULT_HOME_GNX = Path.home() / ".gnx"
CMD_TIMEOUT_S = 600


def _resolve_index_dir(home_gnx: Path, repo: Path) -> Path:
    """Mirror IndexLayout::resolve — repo basename + current branch (no collision suffix)."""
    name = repo.resolve().name.lstrip(".-") or "unknown"
    branch = "main"
    head_file = repo / ".git" / "HEAD"
    if head_file.exists():
        ref = head_file.read_text().strip()
        if ref.startswith("ref: refs/heads/"):
            branch = ref.removeprefix("ref: refs/heads/")
    return home_gnx / name / branch


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
        proc = subprocess.run(
            cmd, cwd=cwd, capture_output=True, text=True, timeout=CMD_TIMEOUT_S
        )
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
    """Pick one Class + one Method from the graph for context/impact/query tests.

    Strategy: cypher `Class-CONTAINS->Method` first row supplies the names;
    `context --name <class>` resolves the canonical uid.
    """
    out: dict[str, str] = {}
    elapsed, rc, stdout, _ = _run(
        [
            str(binary), "cypher",
            "MATCH (a:Class)-[r:CONTAINS]->(b:Method) RETURN a,b",
            "--format", "json",
            "--repo", str(repo),
        ],
        cwd=repo,
    )
    if rc != 0:
        return out
    try:
        rows = json.loads(stdout).get("results", [])
    except json.JSONDecodeError:
        return out
    if not rows:
        return out
    first = rows[0]
    if (cn := first.get("source", {}).get("name")) and isinstance(cn, str):
        out["class_name"] = cn
    target = first.get("target", {})
    if all(k in target for k in ("filePath", "kind", "name")):
        out["method_name"] = target["name"]
        out["method_uid"] = f"{target['kind']}:{target['filePath']}:{target['name']}"

    if name := out.get("class_name"):
        _, rc2, stdout2, _ = _run(
            [str(binary), "context", "--name", name,
             "--format", "json", "--repo", str(repo)],
            cwd=repo,
        )
        if rc2 == 0:
            try:
                cands = json.loads(stdout2).get("candidates", [])
                if cands and (uid := cands[0].get("uid")):
                    out["class_uid"] = uid
            except json.JSONDecodeError:
                pass
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
        ".py": "Python", ".ts": "TypeScript", ".tsx": "TypeScript",
        ".js": "JavaScript", ".jsx": "JavaScript", ".rs": "Rust",
        ".go": "Go", ".java": "Java", ".kt": "Kotlin", ".cs": "C#",
        ".cpp": "C++", ".hpp": "C++", ".c": "C", ".h": "C",
        ".php": "PHP", ".rb": "Ruby", ".swift": "Swift", ".dart": "Dart",
        ".sh": "Bash", ".bash": "Bash", ".cr": "Crystal",
        ".cairo": "Cairo", ".move": "Move", ".nim": "Nim",
        ".sol": "Solidity", ".sql": "SQL", ".vy": "Vyper",
        ".zig": "Zig", ".lua": "Lua", ".v": "Verilog", ".sv": "Verilog",
        ".md": "Markdown", ".yml": "YAML", ".yaml": "YAML",
        ".tf": "HCL", ".hcl": "HCL",
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
    ap.add_argument("--repo", type=Path, default=DEFAULT_REPO,
                    help="Target repo for analyze + query benchmarks")
    ap.add_argument("--git-repo", type=Path, default=DEFAULT_GIT_REPO,
                    help="A real git repo for detect-changes (sample-repo isn't a git checkout)")
    ap.add_argument("--binary", type=Path, default=DEFAULT_BINARY,
                    help="Path to the gnx release binary")
    ap.add_argument("--runs", type=int, default=3,
                    help="Repeats per query command (analyze runs once)")
    ap.add_argument("--json", type=Path, help="Write JSON result to this path")
    ap.add_argument("--skip-cold", action="store_true",
                    help="Don't delete the registry index dir before the first analyze")
    ap.add_argument("--home-gnx", type=Path, default=DEFAULT_HOME_GNX,
                    help="gnx home dir (default ~/.gnx); graph is stored at <home>/<repo>/<branch>/graph.bin")
    ap.add_argument("--with-embeddings", action="store_true",
                    help="After the no-embedding sweep, rebuild with --embeddings and re-run query commands "
                         "to measure BGE-M3 INT8 dense-vector overhead (slow: minutes)")
    args = ap.parse_args()

    if not args.binary.exists():
        print(
            f"error: {args.binary} missing — run "
            f"`cargo build --release -p graph-nexus-cli`",
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
    print(f"langs   : {', '.join(f'{k}={v}' for k, v in sorted(lang_counts.items(), key=lambda kv: -kv[1]))}")
    print(f"cpu     : {hw.get('cpu', '?')}  (logical {hw.get('cpu_count_logical')})")
    print(f"mem     : {hw.get('mem_gb', '?')} GiB")
    print(f"os      : {hw.get('platform')}")
    print()

    samples: list[Sample] = []

    # Phase 1: analyze (cold)
    idx = _resolve_index_dir(args.home_gnx, args.repo)
    if not args.skip_cold and idx.exists():
        print(f"→ rm -rf {idx}")
        shutil.rmtree(idx)
    label = "analyze (cold)" if not args.skip_cold else "analyze (baseline)"
    print(f"→ {label}")
    s = _bench(label, [str(args.binary), "admin", "index", "--repo", str(args.repo)],
               cwd=args.repo, runs=1)
    samples.append(s)
    if s.err:
        print(f"  FAIL: {s.err}")
        _print_summary(samples)
        return 2
    print(f"  {s.median_s:.2f}s")

    # Phase 2: analyze (incremental, hash-cache hot)
    print("→ analyze (incremental)")
    s = _bench("analyze (incremental)",
               [str(args.binary), "admin", "index", "--repo", str(args.repo)],
               cwd=args.repo, runs=1)
    samples.append(s)
    print(f"  {s.median_s:.3f}s" if s.runs else f"  FAIL: {s.err}")

    # Phase 3: probe a Class + Method for downstream tests
    print("→ probing graph for sample symbols")
    sym = _probe_symbols(args.binary, args.repo)
    if sym:
        print(f"  class={sym.get('class_name', '-')}  method={sym.get('method_name', '-')}")
    else:
        print("  none found — context/impact/query will be skipped")
    print()

    # Phase 4: query-shape commands.
    # cypher is "minimal cypher": single MATCH path with optional WHERE a.name='Val'.
    # No LIMIT / no count() / no aggregation.
    cypher_class_contains = (
        "MATCH (a:Class)-[r:CONTAINS]->(b:Method) "
        f"WHERE a.name='{sym.get('class_name', 'AppController')}' RETURN a,b"
    )
    queries: list[tuple[str, list[str], Path]] = [
        ("cypher Class->Method (one)",
         [str(args.binary), "cypher", cypher_class_contains,
          "--repo", str(args.repo)],
         args.repo),
        ("cypher Method-CALLS->Method",
         [str(args.binary), "cypher",
          "MATCH (a:Method)-[r:CALLS]->(b:Method) "
          f"WHERE a.name='{sym.get('method_name', 'main')}' RETURN a,b",
          "--repo", str(args.repo)],
         args.repo),
        ("routes",
         [str(args.binary), "routes", "--repo", str(args.repo)],
         args.repo),
        ("coverage",
         [str(args.binary), "coverage"],
         args.repo),
        ("coverage --detailed",
         [str(args.binary), "coverage", "--detailed", "--repo", str(args.repo)],
         args.repo),
    ]
    if name := sym.get("class_name"):
        queries.append((
            "inspect (Class)",
            [str(args.binary), "inspect", "--name", name, "--repo", str(args.repo)],
            args.repo,
        ))
    if name := sym.get("method_name"):
        queries.append((
            "search (lexical)",
            [str(args.binary), "search", "--query", name, "--repo", str(args.repo)],
            args.repo,
        ))
    if uid := sym.get("class_uid"):
        queries.append((
            "impact upstream",
            [str(args.binary), "impact", "--target", uid,
             "--direction", "upstream", "--repo", str(args.repo)],
            args.repo,
        ))
    if uid := sym.get("method_uid"):
        queries.append((
            "impact downstream",
            [str(args.binary), "impact", "--target", uid,
             "--direction", "downstream", "--repo", str(args.repo)],
            args.repo,
        ))

    if args.git_repo.is_dir() and (args.git_repo / ".git").exists():
        queries.append((
            "impact --since HEAD~1",
            [str(args.binary), "impact", "--since", "HEAD~1",
             "--repo", str(args.git_repo)],
            args.git_repo,
        ))

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

    # Phase 5: optional --embeddings sweep
    if args.with_embeddings:
        print()
        print(f"→ rm -rf {idx}  (rebuild with embeddings)")
        if idx.exists():
            shutil.rmtree(idx)
        print("→ admin index --embeddings (cold)")
        s = _bench(
            "admin index --embeddings (cold)",
            [str(args.binary), "admin", "index", "--repo", str(args.repo), "--embeddings"],
            cwd=args.repo, runs=1,
        )
        samples.append(s)
        if s.runs:
            print(f"  {s.median_s:.2f}s")
        else:
            print(f"  FAIL: {s.err}")

        print("→ admin index --embeddings (incremental, embed cache hot)")
        s = _bench(
            "admin index --embeddings (incremental)",
            [str(args.binary), "admin", "index", "--repo", str(args.repo), "--embeddings"],
            cwd=args.repo, runs=1,
        )
        samples.append(s)
        if s.runs:
            print(f"  {s.median_s:.2f}s")
        else:
            print(f"  FAIL: {s.err}")

        emb_queries: list[tuple[str, list[str], Path]] = [
            ("search (hybrid, w/ emb)",
             [str(args.binary), "search", "--query",
              sym.get("method_name", "main"), "--repo", str(args.repo)],
             args.repo),
        ]
        if name := sym.get("class_name"):
            emb_queries.append((
                "inspect (w/ emb graph)",
                [str(args.binary), "inspect", "--name", name, "--repo", str(args.repo)],
                args.repo,
            ))
        for name, cmd, cwd in emb_queries:
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
