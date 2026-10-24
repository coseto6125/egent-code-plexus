#!/usr/bin/env python3
"""Regression-style benchmark for the ecp CLI — runs every public subcommand
against a sample repo and reports wall-clock latency.

Typical use:
    python scripts/benchmark/benchmark_ecp.py                              # full sweep
    python scripts/benchmark/benchmark_ecp.py --runs 5 --json out.json     # CI mode
    python scripts/benchmark/benchmark_ecp.py --skip-cold                  # don't wipe index
    python scripts/benchmark/benchmark_ecp.py --dirty-only --dirty-repo ~/vscode \
        --dirty-file src/vs/base/common/strings.ts --no-build                # dirty-file latency
    # analyze phases default to 1 sample (~±20% wall noise); --runs N>1 or
    # --analyze-runs N raises analyze repeats for CI regression detection.
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


def _run(
    cmd: list[str], cwd: Path | None, env: dict[str, str] | None = None
) -> tuple[float, int, str, str]:
    start = time.perf_counter()
    try:
        proc = subprocess.run(
            cmd, cwd=cwd, env=env, capture_output=True, text=True, timeout=CMD_TIMEOUT_S
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


# Every variable `session/resolver.rs` reads a session id from. Each dirty-file
# run clears all of them, then sets the one id its phase needs, so the host
# session of whoever launched the bench never leaks into a sample.
SESSION_ENV_VARS = (
    "ECP_SESSION_ID",
    "GEMINI_CLI_SESSION_ID",
    "CODEX_SESSION_ID",
    "CODEX_THREAD_ID",
    "CLAUDE_CODE_SESSION_ID",
)

# The appended edit per suffix: one new exported function, as the latency
# protocol edits `src/vs/base/common/strings.ts` on vscode. `{name}` is unique
# per bench invocation so the edit never collides with a real symbol.
DIRTY_APPEND = {
    ".ts": "\nexport function {name}(): number {{\n\treturn 1;\n}}\n",
    ".tsx": "\nexport function {name}(): number {{\n\treturn 1;\n}}\n",
    ".js": "\nexport function {name}() {{\n\treturn 1;\n}}\n",
    ".py": "\n\ndef {name}():\n    return 1\n",
    ".rs": "\npub fn {name}() -> i32 {{\n    1\n}}\n",
    ".go": "\nfunc {name}() int {{\n\treturn 1\n}}\n",
}


def _session_env(session_id: str | None) -> dict[str, str]:
    env = {k: v for k, v in os.environ.items() if k not in SESSION_ENV_VARS}
    if session_id is not None:
        env["ECP_SESSION_ID"] = session_id
    return env


def _git_head(repo: Path) -> str | None:
    proc = subprocess.run(
        ["git", "rev-parse", "HEAD"], cwd=repo, capture_output=True, text=True, timeout=30
    )
    return proc.stdout.strip() if proc.returncode == 0 else None


def _restore_file(
    target: Path, original: bytes, written: bytes, times_ns: tuple[int, int]
) -> bool:
    """Put `original` back unless the file no longer holds what the script wrote.

    A save by the user (or a deletion) while the benchmark runs is newer than
    the script's copy, so the file stays as it is and a warning names it.
    """
    try:
        current = target.read_bytes()
    except OSError as e:
        print(f"warning: {target} left as is, cannot read it: {e}", file=sys.stderr)
        return False
    if current != written:
        print(
            f"warning: {target} changed while the benchmark ran; not restoring it",
            file=sys.stderr,
        )
        return False
    target.write_bytes(original)
    os.utime(target, ns=times_ns)
    return True


def _bench_dirty(
    binary: Path, repo: Path, rel_file: str, query: str, mode: str | None, append: str, runs: int
) -> list[Sample]:
    """Time `ecp find` on a clean tree and after one edit, in paired runs.

    Each pair: (a) the clean tree with a new session id; (b) the first query
    after the edit, with a new session id so its L1 state starts empty; (c) a
    later query in that same session and dirty state; (d) the dirty state with
    every session variable cleared (the no-session case). Pair 0 is a discarded
    warm-up. The file's bytes and mtime are restored before every pair and on
    any exit, so git sees the tree clean again; a file the user changed or
    deleted meanwhile is left alone with a warning.
    """
    target = repo / rel_file
    original = target.read_bytes()
    stat = target.stat()
    edited = original + append.encode()
    cmd = [str(binary), "find", query, "--repo", str(repo)]
    if mode:
        cmd += ["--mode", mode]
    phases = (
        "find dirty: clean",
        "find dirty: first after edit",
        "find dirty: later after edit",
        "find dirty: no session",
    )
    samples = [Sample(name=name, cmd=cmd, cwd=str(repo)) for name in phases]
    tag = f"{os.getpid()}-{time.time_ns()}"

    # What the script itself last put on disk: a save by the user in between
    # makes the file differ, and `_restore_file` then leaves it alone.
    written = original
    abandoned = False

    def restore() -> bool:
        nonlocal written, abandoned
        if abandoned:
            return False
        if not _restore_file(target, original, written, (stat.st_atime_ns, stat.st_mtime_ns)):
            abandoned = True
            return False
        written = original
        return True

    try:
        for i in range(runs + 1):
            if not restore():
                samples[0].err = f"{rel_file} changed during the run; benchmark stopped"
                return samples
            clean_sid = f"bench-clean-{tag}-{i}"
            dirty_sid = f"bench-dirty-{tag}-{i}"
            pair: list[float] = []
            for phase, sid in enumerate((clean_sid, dirty_sid, dirty_sid, None)):
                if phase == 1:
                    target.write_bytes(edited)
                    written = edited
                elapsed, rc, _out, stderr = _run(cmd, repo, env=_session_env(sid))
                if rc != 0:
                    tail = stderr.strip().splitlines()[-1] if stderr.strip() else f"exit {rc}"
                    samples[phase].err = tail[:160]
                    return samples
                pair.append(elapsed)
            if i == 0:
                continue
            for s, elapsed in zip(samples, pair):
                s.runs.append(elapsed)
    finally:
        restore()
    return samples


def _dirty_exit_code(samples: list[Sample], dirty: object) -> int:
    """0 only when every phase has runs and no round recorded an error.

    A failure in a later round keeps the runs of the rounds before it, so
    "every sample has a run" alone would report success after a failed run.
    """
    return 0 if dirty and all(s.runs for s in samples) and not any(s.err for s in samples) else 3


def _print_dirty_deltas(samples: list[Sample]) -> None:
    """Median of the paired differences against the clean query of each pair."""
    clean = samples[0]
    if not clean.runs:
        return
    for s in samples[1:]:
        if len(s.runs) != len(clean.runs):
            continue
        delta = median(b - a for a, b in zip(clean.runs, s.runs))
        ratio = median(b / a for a, b in zip(clean.runs, s.runs))
        print(f"  {s.name:<30} paired delta {delta * 1000:+7.1f}ms  ratio x{ratio:.3f}")


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


def _dirty_phase(args: argparse.Namespace) -> tuple[list[Sample], dict[str, object] | None]:
    """Run the dirty-file scenario; return its samples and its protocol record."""
    repo: Path = args.dirty_repo
    target = repo / args.dirty_file
    if not target.is_file():
        print(f"error: dirty file {target} not found", file=sys.stderr)
        return [], None
    name = f"ecpBenchDirty{os.getpid()}"
    append = args.dirty_append
    if append is None:
        template = DIRTY_APPEND.get(target.suffix)
        if template is None:
            print(f"error: no default edit for {target.suffix}; pass --dirty-append", file=sys.stderr)
            return [], None
        append = template.format(name=name)
    print(f"→ dirty-file scenario: index {repo}")
    _run([str(args.binary), "admin", "index", "--repo", str(repo)], cwd=repo, env=_session_env(None))
    query = args.dirty_query or _probe_symbols(args.binary, repo).get("method_name", "main")
    print(
        f"→ dirty-file scenario: find {query!r} mode={args.dirty_mode or 'default'} "
        f"edit={args.dirty_file} runs={args.dirty_runs}+1 warm-up"
    )
    samples = _bench_dirty(
        args.binary, repo, args.dirty_file, query, args.dirty_mode, append, args.dirty_runs
    )
    for s in samples:
        if s.err:
            print(f"  {s.name}: FAIL: {s.err}")
        elif s.runs:
            print(f"  {s.name:<30} median {_fmt(s.median_s).strip()}")
    _print_dirty_deltas(samples)
    protocol: dict[str, object] = {
        "repo": str(repo),
        "head": _git_head(repo),
        "file": args.dirty_file,
        "append": append,
        "query": query,
        "mode": args.dirty_mode,
        "paired_runs": args.dirty_runs,
        "warmup_runs": 1,
    }
    return samples, protocol


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
    ap.add_argument(
        "--analyze-runs",
        type=int,
        default=0,
        help="Repeats for the analyze (cold/baseline + incremental) phases. "
        "Default 0 means: imply --runs when --runs>1, else 1. Single-sample "
        "analyze has ~±20%% wall-time noise; use --analyze-runs 10 for CI "
        "regression detection.",
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
    ap.add_argument(
        "--dirty-file",
        help="Repo-relative file the dirty-file scenario edits; the scenario runs only when set",
    )
    ap.add_argument(
        "--dirty-repo",
        type=Path,
        help="Indexed git repo for the dirty-file scenario (default: --git-repo)",
    )
    ap.add_argument(
        "--dirty-query",
        help="`ecp find` pattern for the dirty-file scenario (default: a probed method name)",
    )
    ap.add_argument(
        "--dirty-mode", help="`ecp find --mode` for the dirty-file scenario (default: the CLI default)"
    )
    ap.add_argument(
        "--dirty-append",
        help="Text appended to --dirty-file (default: one exported function for its suffix)",
    )
    ap.add_argument(
        "--dirty-runs",
        type=int,
        default=10,
        help="Paired runs for the dirty-file scenario, after one discarded warm-up",
    )
    ap.add_argument(
        "--dirty-only",
        action="store_true",
        help="Run only the dirty-file scenario (needs --dirty-file)",
    )
    args = ap.parse_args()
    analyze_runs = args.analyze_runs or (args.runs if args.runs > 1 else 1)
    args.repo = args.repo.resolve()
    args.git_repo = args.git_repo.resolve()
    args.binary = args.binary.resolve()
    args.dirty_repo = (args.dirty_repo or args.git_repo).resolve()
    if args.dirty_only and not args.dirty_file:
        print("error: --dirty-only needs --dirty-file", file=sys.stderr)
        return 1

    _ensure_binary_fresh(args.binary, skip=args.no_build)
    if not args.binary.exists():
        print(
            f"error: {args.binary} missing — run "
            f"`cargo build --release -p egent-code-plexus --bin ecp`",
            file=sys.stderr,
        )
        return 1
    if args.dirty_only:
        samples, dirty = _dirty_phase(args)
        _print_summary(samples)
        if args.json:
            payload = {"binary": str(args.binary), "hardware": _hardware(), "dirty": dirty}
            payload["samples"] = [asdict(s) for s in samples]
            args.json.write_text(json.dumps(payload, indent=2))
            print(f"\n→ wrote {args.json}")
        return _dirty_exit_code(samples, dirty)
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
        label, [str(args.binary), "admin", "index", "--repo", str(args.repo)], cwd=args.repo, runs=analyze_runs
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
        runs=analyze_runs,
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
        # COLLECT() projection: aggregates child names into a list per parent.
        # Exercises the grouping accumulator's list-collection path, distinct
        # from scalar count(*). Without it, COLLECT regressions go undetected.
        (
            "cypher COLLECT",
            [
                str(args.binary),
                "cypher",
                "MATCH (c:Class)-[:HasMethod]->(m:Method) RETURN c.name, collect(m.name)",
                "--repo",
                str(args.repo),
            ],
            args.repo,
        ),
        # IN [literal,...] literal-list filter: distinct from `<lit> IN prop`
        # (decorator IN row). Exercises the literal-list membership predicate.
        (
            "cypher IN literal-list",
            [
                str(args.binary),
                "cypher",
                "MATCH (m:Method) WHERE m.name IN ['main','run','init'] RETURN count(*)",
                "--repo",
                str(args.repo),
            ],
            args.repo,
        ),
        # Multi-hop variable-length edge: exercises the frontier-expansion path
        # over 1..3 Calls hops, the most allocation-heavy traversal shape.
        # DISTINCT: count(*) counted walks, which Cypher's exact-or-refuse rule now refuses.
        (
            "cypher multi-hop Calls*1..3",
            [
                str(args.binary),
                "cypher",
                "MATCH (a:Method)-[:Calls*1..3]->(b:Method) RETURN count(DISTINCT b)",
                "--repo",
                str(args.repo),
            ],
            args.repo,
        ),
        # Multi-aggregate GROUP BY: groups all nodes by kind with a count,
        # exercising the grouped-accumulator + ORDER BY on an aggregate.
        (
            "cypher GROUP BY kind",
            [
                str(args.binary),
                "cypher",
                "MATCH (n) RETURN n.kind, count(*) ORDER BY count(*) DESC",
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

    dirty: dict[str, object] | None = None
    if args.dirty_file:
        dirty_samples, dirty = _dirty_phase(args)
        samples.extend(dirty_samples)

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
        if dirty is not None:
            payload["dirty"] = dirty
        args.json.write_text(json.dumps(payload, indent=2))
        print(f"\n→ wrote {args.json}")

    return 0 if all(s.runs for s in samples) and not any(s.err for s in samples) else 3


if __name__ == "__main__":
    sys.exit(main())
