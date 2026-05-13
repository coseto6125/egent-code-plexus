#!/usr/bin/env python3
import subprocess
import json
import argparse
import sys
import difflib
from pathlib import Path
from typing import Any

def normalize_json(data: Any) -> Any:
    """Recursively normalizes JSON data for comparison.
    Sorts lists of dictionaries based on a stable string representation
    if they contain unordered graph edges or nodes.
    """
    if isinstance(data, dict):
        return {k: normalize_json(v) for k, v in sorted(data.items())}
    elif isinstance(data, list):
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
        return json.loads(result.stdout)
    except subprocess.CalledProcessError as e:
        print(f"Command failed: {' '.join(cmd)}")
        print(f"Error output:\n{e.stderr}\n{e.stdout}")
        sys.exit(1)
    except json.JSONDecodeError as e:
        print(f"Failed to parse JSON from command: {' '.join(cmd)}")
        print(f"Output was:\n{result.stdout}")
        sys.exit(1)

def print_diff(dict1: Any, dict2: Any, name1: str, name2: str) -> None:
    """Prints a unified diff of two JSON objects."""
    str1 = json.dumps(dict1, indent=2, sort_keys=True).splitlines(keepends=True)
    str2 = json.dumps(dict2, indent=2, sort_keys=True).splitlines(keepends=True)
    
    diff = difflib.unified_diff(str1, str2, fromfile=name1, tofile=name2)
    sys.stdout.writelines(diff)

def main() -> None:
    parser = argparse.ArgumentParser(description="Run shadow parity validation between gnx and gnx-rs")
    parser.add_argument("fixture_path", type=Path, help="Path to the TypeScript fixture file or directory")
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
    gnx_rs_analyze_cmd = ["cargo", "run", "--bin", "gnx-rs", "--", "analyze", "--repo", str(fixture_path)]

    print("Running original gnx analyze...")
    subprocess.run(gnx_analyze_cmd, cwd=workspace_root, capture_output=True, check=False)
    
    print("Running new gnx-rs analyze...")
    subprocess.run(gnx_rs_analyze_cmd, cwd=workspace_root, capture_output=True, check=False)

    print(f"\n[Context Phase: {symbol}]")
    gnx_context_cmd = ["gnx", "context", "--name", symbol, "--repo", str(fixture_path), "--format", "json"]
    gnx_rs_context_cmd = ["cargo", "run", "--bin", "gnx-rs", "--", "context", "--name", symbol, "--repo", str(fixture_path), "--format", "json"]
    
    print("Running original gnx context...")
    gnx_output = run_command(gnx_context_cmd, cwd=workspace_root)
    
    print("Running new gnx-rs context...")
    gnx_rs_output = run_command(gnx_rs_context_cmd, cwd=workspace_root)

    normalized_gnx = normalize_json(gnx_output)
    normalized_gnx_rs = normalize_json(gnx_rs_output)

    if normalized_gnx == normalized_gnx_rs:
        print("\n✅ SUCCESS: 100% Parity Achieved!")
    else:
        print("\n❌ FAILURE: Mismatch detected between gnx and gnx-rs outputs.")
        print_diff(normalized_gnx, normalized_gnx_rs, "gnx (original)", "gnx-rs (new)")
        sys.exit(1)

if __name__ == "__main__":
    main()
