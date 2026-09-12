"""Compare value-flow changes using fresh processes and interleaved samples.

Build both revisions with the same release profile before running this script.
The output directory retains fixtures, command output, and raw measurements.
"""

import argparse
import hashlib
import json
import os
import platform
import statistics
import subprocess
import time
from pathlib import Path


def fixture(root: Path, kind: str, size: int) -> Path:
    repo = root / f"{kind}-{size}"
    repo.mkdir(parents=True, exist_ok=True)
    match kind:
        case "flat":
            lines = [
                f"function f{i}(x) {{ return f{(i + 1) % size}(x + 1); }}" for i in range(size)
            ]
        case "closures":
            lines = [
                f"function owner{i}() {{ var step = function() {{ return 1; }}; "
                "var perView = function() { return step() + 1; }; return perView(); }"
                for i in range(size)
            ]
        case "flow":
            lines = ["const input = 1;"] + [
                f"function f{i}(x) {{ return x + {i}; }} consume(f{i}(input));" for i in range(size)
            ]
        case _:
            raise ValueError(kind)
    (repo / "main.js").write_text("\n".join(lines) + "\n")
    subprocess.run(["git", "init", "-q", str(repo)], check=True)
    subprocess.run(["git", "-C", str(repo), "add", "main.js"], check=True)
    subprocess.run(
        [
            "git",
            "-C",
            str(repo),
            "-c",
            "user.name=Benchmark",
            "-c",
            "user.email=benchmark@example.invalid",
            "commit",
            "-qm",
            "fixture",
        ],
        check=True,
    )
    return repo


def measure(
    binary: Path, repo: Path, args: list[str], home: Path, stem: Path, stdin: str | None = None
) -> dict:
    command = [str(binary), *args]
    started = time.perf_counter()
    # Pipes use selector-based communication. wait(timeout) on redirected files
    # polls with sleeps up to 50 ms and distorts short command measurements.
    result = subprocess.run(
        ["/usr/bin/time", "-f", "%M", "-o", str(stem.with_suffix(".rss")), *command],
        cwd=repo,
        env={**os.environ, "ECP_HOME": str(home)},
        input=stdin,
        text=True,
        capture_output=True,
        timeout=120,
    )
    elapsed = time.perf_counter() - started
    stem.with_suffix(".stdout").write_text(result.stdout)
    stem.with_suffix(".stderr").write_text(result.stderr)
    if result.returncode:
        raise RuntimeError(f"Command failed: {command}; inspect {stem}.stderr")
    return {
        "command": command,
        "seconds": elapsed,
        "rss_kib": int(stem.with_suffix(".rss").read_text()),
        "evidence": str(stem),
    }


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--before", required=True, type=Path)
    parser.add_argument("--after", required=True, type=Path)
    parser.add_argument("--output", required=True, type=Path)
    parser.add_argument("--runs", type=int, default=7)
    parser.add_argument("--sizes", type=int, nargs="+", default=[100, 500, 1000])
    options = parser.parse_args()
    if options.runs < 3 or any(size < 1 for size in options.sizes):
        parser.error("Use at least three runs and positive fixture sizes")
    root = options.output.resolve()
    root.mkdir(parents=True, exist_ok=False)
    binaries = {"before": options.before.resolve(), "after": options.after.resolve()}
    report = {
        "platform": platform.platform(),
        "cpu_count": os.cpu_count(),
        "method": "Interleaved fresh processes; warm filesystem; release binaries supplied by caller",
        "binaries": {},
        "cases": {},
    }
    for name, binary in binaries.items():
        version = subprocess.check_output([str(binary), "--version"], text=True).strip()
        with binary.open("rb") as stream:
            digest = hashlib.file_digest(stream, "sha256").hexdigest()
        report["binaries"][name] = {"path": str(binary), "version": version, "sha256": digest}

    def record(name: str, variant: str, row: dict) -> None:
        report["cases"].setdefault(name, {}).setdefault(variant, []).append(row)
        (root / "raw.json").write_text(json.dumps(report, indent=2) + "\n")

    for size in options.sizes:
        for kind in ("flat", "closures"):
            repo = fixture(root, kind, size)
            for run in range(options.runs):
                order = list(binaries) if run % 2 == 0 else list(reversed(binaries))
                for variant in order:
                    stem = root / f"{kind}-{size}-{variant}-{run}"
                    row = measure(
                        binaries[variant],
                        repo,
                        ["admin", "index", "--repo", str(repo), "--force"],
                        root / f"state-{variant}",
                        stem,
                    )
                    record(f"index-{kind}-{size}", variant, row)
        repo = fixture(root, "flow", size)
        flow_args = [
            "flow",
            "--file",
            "main.js",
            "--line",
            "1",
            "--column",
            "7",
            "--subject",
            "binding",
            "--format",
            "json",
        ]
        for run in range(options.runs):
            for mode in ("uncached", "cached"):
                args = flow_args + (["--no-cache"] if mode == "uncached" else [])
                stem = root / f"flow-{size}-{mode}-{run}"
                row = measure(binaries["after"], repo, args, root / "flow-state", stem)
                evidence = json.loads(stem.with_suffix(".stdout").read_text())
                if evidence["truncated"] or not evidence["consumers"]:
                    raise RuntimeError(f"Incomplete flow evidence: {stem}")
                row.update({key: evidence[key] for key in ("cache_hit", "truncated")})
                row.update(
                    {
                        key: len(evidence[key])
                        for key in ("nodes", "edges", "consumers", "boundaries")
                    }
                )
                record(f"flow-{size}-{mode}", "after", row)
        envelope = json.dumps(
            {"cwd": str(repo), "tool_name": "Bash", "tool_input": {"command": "echo hello"}}
        )
        for run in range(options.runs):
            order = list(binaries) if run % 2 == 0 else list(reversed(binaries))
            for variant in order:
                row = measure(
                    binaries[variant],
                    repo,
                    ["hook", "pre-tool-use", "--claude-code"],
                    root / f"hook-{variant}",
                    root / f"hook-{size}-{variant}-{run}",
                    envelope,
                )
                record(f"hook-control-{size}", variant, row)
        for run in range(options.runs):
            old, new = (1, 2) if run % 2 == 0 else (2, 1)
            envelope = json.dumps(
                {
                    "cwd": str(repo),
                    "session_id": "benchmark",
                    "tool_use_id": str(run),
                    "tool_name": "Edit",
                    "tool_input": {
                        "file_path": str(repo / "main.js"),
                        "old_string": f"const input = {old};",
                        "new_string": f"const input = {new};",
                    },
                }
            )
            for phase in ("pre", "post"):
                if phase == "post":
                    source = repo / "main.js"
                    source.write_text(
                        source.read_text().replace(
                            f"const input = {old};", f"const input = {new};", 1
                        )
                    )
                stem = root / f"hook-edit-{size}-{phase}-{run}"
                row = measure(
                    binaries["after"],
                    repo,
                    ["hook", f"{phase}-tool-use", "--claude-code"],
                    root / "edit-state",
                    stem,
                    envelope,
                )
                evidence = json.loads(stem.with_suffix(".stdout").read_text())
                context = evidence["hookSpecificOutput"]["additionalContext"]
                expected_phase = "before" if phase == "pre" else "after"
                consumer_lines = [
                    line
                    for line in context.splitlines()
                    if line.startswith("  main.js:") and " argument " in line
                ]
                if not (
                    context.startswith(f"ecp flow {expected_phase} edit:")
                    and "source_hashes=" in context
                    and consumer_lines
                ):
                    raise RuntimeError(f"Edit evidence missing: {stem}")
                row["context_bytes"] = len(context.encode())
                row["displayed_argument_consumers"] = len(consumer_lines)
                row["context_truncated"] = "truncated=true" in context
                record(f"hook-edit-{size}-{phase}", "after", row)
    summary = {}
    for case, variants in report["cases"].items():
        summary[case] = {}
        for variant, rows in variants.items():
            # The first cached query populates the cache; retain it only in raw evidence.
            samples = (
                [row for row in rows if row.get("cache_hit", True)]
                if case.endswith("-cached")
                else rows
            )
            if not samples:
                raise RuntimeError(f"No verified samples for {case}/{variant}")
            summary[case][variant] = {
                "median_ms": statistics.median(row["seconds"] for row in samples) * 1000,
                "min_ms": min(row["seconds"] for row in samples) * 1000,
                "max_rss_kib": max(row["rss_kib"] for row in samples),
            }
    (root / "summary.json").write_text(json.dumps(summary, indent=2) + "\n")
    print(json.dumps(summary, indent=2))


if __name__ == "__main__":
    main()
