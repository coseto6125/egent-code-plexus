#!/usr/bin/env python3
"""Assemble per-platform PyPI wheels from prebuilt release binaries.

    python build_wheels.py --version 0.5.0 --artifacts <dir> --out <dir>

<artifacts> holds the extracted release tarballs, one dir per rust target:
    ecp-v0.5.0-x86_64-unknown-linux-gnu/ecp
    ecp-v0.5.0-x86_64-pc-windows-msvc/ecp.exe

For each platform we drop the binary into src/ecp/_bin/, build a wheel via
hatchling, then retag the resulting `py3-none-any` wheel to the platform tag so
pip/uv select the right one. No Rust compilation happens here.
"""

from __future__ import annotations

import argparse
import shutil
import subprocess
import sys
from pathlib import Path

HERE = Path(__file__).parent

# rust target → (PyPI platform tag, binary filename)
MATRIX = [
    ("x86_64-unknown-linux-gnu", "manylinux2014_x86_64", "ecp"),
    ("aarch64-unknown-linux-gnu", "manylinux2014_aarch64", "ecp"),
    ("x86_64-apple-darwin", "macosx_10_12_x86_64", "ecp"),
    ("aarch64-apple-darwin", "macosx_11_0_arm64", "ecp"),
    ("x86_64-pc-windows-msvc", "win_amd64", "ecp.exe"),
]


def stamp_version(version: str) -> str:
    """Write the release version into pyproject.toml, returning the original
    text so the caller can restore it (keeps the committed source on 0.0.0)."""
    pyproject = HERE / "pyproject.toml"
    original = pyproject.read_text()
    pyproject.write_text(original.replace('version = "0.0.0"', f'version = "{version}"', 1))
    return original


def build_one(version: str, artifacts: Path, out: Path, target: str, plat_tag: str, bin_name: str) -> None:
    bin_dir = HERE / "src" / "ecp" / "_bin"
    if bin_dir.exists():
        shutil.rmtree(bin_dir)
    bin_dir.mkdir(parents=True)

    src_bin = artifacts / f"ecp-v{version}-{target}" / bin_name
    dst_bin = bin_dir / bin_name
    shutil.copy2(src_bin, dst_bin)
    if not bin_name.endswith(".exe"):
        dst_bin.chmod(0o755)

    staging = out / f"_build-{plat_tag}"
    subprocess.run(
        [sys.executable, "-m", "build", "--wheel", "--outdir", str(staging), str(HERE)],
        check=True,
    )
    built = next(staging.glob("egent_code_plexus-*-py3-none-any.whl"))
    # `wheel tags` rewrites the platform tag and renames the file accordingly.
    subprocess.run(
        [
            sys.executable, "-m", "wheel", "tags",
            "--platform-tag", plat_tag,
            "--python-tag", "py3",
            "--abi-tag", "none",
            "--remove",
            str(built),
        ],
        check=True,
        cwd=staging,
    )
    retagged = next(staging.glob(f"egent_code_plexus-*-{plat_tag}.whl"))
    out.mkdir(parents=True, exist_ok=True)
    shutil.move(str(retagged), str(out / retagged.name))
    shutil.rmtree(staging)
    print(f"built {retagged.name}")


def main() -> None:
    ap = argparse.ArgumentParser()
    ap.add_argument("--version", required=True)
    ap.add_argument("--artifacts", required=True, type=Path)
    ap.add_argument("--out", required=True, type=Path)
    args = ap.parse_args()

    original_pyproject = stamp_version(args.version)
    try:
        for target, plat_tag, bin_name in MATRIX:
            build_one(args.version, args.artifacts, args.out, target, plat_tag, bin_name)
    finally:
        # Restore the committed placeholder + drop the injected binary so the
        # source tree is left untouched whether the build succeeds or fails.
        (HERE / "pyproject.toml").write_text(original_pyproject)
        leftover = HERE / "src" / "ecp" / "_bin"
        if leftover.exists():
            shutil.rmtree(leftover)


if __name__ == "__main__":
    main()
