#!/usr/bin/env python3
import argparse
import difflib
import json
import subprocess
import sys
from pathlib import Path
from typing import Any


def normalize_json(data: Any) -> Any:
    """
    Recursively normalizes JSON data for comparison.
    Sorts lists of dictionaries based on a stable string representation
    if they contain unordered graph edges or nodes.
    """
    if isinstance(data, dict):
        return {k: normalize_json(v) for k, v in sorted(data.items())}
    if isinstance(data, list):
        normalized_list = [normalize_json(item) for item in data]
        try:
            return sorted(normalized_list)
        except TypeError:
            return sorted(normalized_list, key=lambda x: json.dumps(x, sort_keys=True))
    else:
        return data


def run_command(cmd: list[str], cwd: Path | None = None) -> Any:
    """Runs a CLI command and parses the JSON output."""
    try:
        result = subprocess.run(cmd, cwd=cwd, capture_output=True, text=True, check=True)
        # GitNexus sometimes appends --- Next: ... to the output. Extract just the JSON block.
        output = result.stdout.strip()
        if "---" in output:
            output = output.split("---")[0].strip()

        # If there are preamble messages, find the first '{' or '['
        start_idx = output.find("{")
        list_start_idx = output.find("[")

        if start_idx == -1 and list_start_idx == -1:
            raise json.JSONDecodeError("No JSON object found", output, 0)

        actual_start = (
            min(start_idx, list_start_idx)
            if start_idx != -1 and list_start_idx != -1
            else max(start_idx, list_start_idx)
        )

        return json.loads(output[actual_start:])
    except subprocess.CalledProcessError as e:
        print(f"Command failed: {' '.join(cmd)}")
        print(f"Error output:\n{e.stderr}\n{e.stdout}")
        sys.exit(1)
    except json.JSONDecodeError:
        print(f"Failed to parse JSON from command: {' '.join(cmd)}")
        print(f"Output was:\n{result.stdout}")
        sys.exit(1)


def is_subset(expected: Any, actual: Any, path: str = "") -> list[str]:
    """
    Returns a list of error messages if `expected` is not a subset of `actual`.
    Allows `actual` to contain extra keys (like startLine, kind).
    """
    errors = []
    if isinstance(expected, dict) and isinstance(actual, dict):
        for k, v in expected.items():
            if k not in actual:
                errors.append(f"Missing key '{k}' at path '{path}'")
            else:
                errors.extend(is_subset(v, actual[k], f"{path}.{k}" if path else k))
    elif isinstance(expected, list) and isinstance(actual, list):
        if len(expected) != len(actual):
            errors.append(
                f"List length mismatch at '{path}'. Expected {len(expected)}, got {len(actual)}"
            )
        else:
            # We assume lists are already sorted by `normalize_json`
            for i, (e_val, a_val) in enumerate(zip(expected, actual, strict=False)):
                errors.extend(is_subset(e_val, a_val, f"{path}[{i}]"))
    elif expected != actual:
        errors.append(f"Value mismatch at '{path}'. Expected '{expected}', got '{actual}'")

    return errors


def print_diff(dict1: Any, dict2: Any, name1: str, name2: str) -> None:
    """Prints a unified diff of two JSON objects."""
    str1 = json.dumps(dict1, indent=2, sort_keys=True).splitlines(keepends=True)
    str2 = json.dumps(dict2, indent=2, sort_keys=True).splitlines(keepends=True)

    diff = difflib.unified_diff(str1, str2, fromfile=name1, tofile=name2)
    sys.stdout.writelines(diff)


def main() -> None:
    parser = argparse.ArgumentParser(
        description="Run shadow parity validation between gnx and gnx-rs"
    )
    parser.add_argument(
        "fixture_path", type=Path, help="Path to the TypeScript fixture file or directory"
    )
    parser.add_argument("symbol", type=str, help="Symbol to query for context")
    args = parser.parse_args()

    fixture_path = args.fixture_path.resolve()
    symbol = args.symbol

    if not fixture_path.exists():
        print(f"Fixture path does not exist: {fixture_path}")
        sys.exit(1)

    workspace_root = Path.cwd()

    print(f"Running parity tests for fixture: {fixture_path} on symbol: {symbol}")

    print("\n[Analyze Phase]")
    gnx_analyze_cmd = ["gnx", "analyze", "--repo", str(fixture_path)]
    gnx_rs_analyze_cmd = [
        "cargo",
        "run",
        "--bin",
        "gnx-rs",
        "--",
        "analyze",
        "--repo",
        str(fixture_path),
    ]

    print("Running original gnx analyze...")
    subprocess.run(gnx_analyze_cmd, cwd=workspace_root, capture_output=True, check=False)

    print("Running new gnx-rs analyze...")
    subprocess.run(gnx_rs_analyze_cmd, cwd=workspace_root, capture_output=True, check=False)

    print(f"\n[Context Phase: {symbol}]")
    gnx_context_cmd = [
        "gnx",
        "context",
        "--name",
        symbol,
        "--repo",
        str(fixture_path),
        "--format",
        "json",
    ]
    gnx_rs_context_cmd = [
        "cargo",
        "run",
        "--bin",
        "gnx-rs",
        "--",
        "context",
        "--name",
        symbol,
        "--repo",
        str(fixture_path),
        "--format",
        "json",
    ]

    print("Running original gnx context...")
    gnx_output = run_command(gnx_context_cmd, cwd=workspace_root)

    print("Running new gnx-rs context...")
    gnx_rs_output = run_command(gnx_rs_context_cmd, cwd=workspace_root)

    normalized_gnx = normalize_json(gnx_output)
    normalized_gnx_rs = normalize_json(gnx_rs_output)

    errors = is_subset(normalized_gnx, normalized_gnx_rs)

    if not errors:
        print("\n✅ SUCCESS: 100% Parity Achieved! (gnx-rs is a superset of gnx)")
    else:
        print("\n❌ FAILURE: Mismatch detected. gnx-rs is missing expected fields or values.")
        for error in errors:
            print(f"  - {error}")
        print(
            "\n--- Diff (Note: Extra fields in gnx-rs are ACCEPTABLE, look for missing/changed fields) ---"
        )
        print_diff(normalized_gnx, normalized_gnx_rs, "gnx (original)", "gnx-rs (new)")
        sys.exit(1)


if __name__ == "__main__":
    main()
