"""EgentCodePlexus (ecp) — prebuilt-binary launcher distributed on PyPI.

Each platform wheel ships exactly one native `ecp` binary alongside this
package. Import-time logic only locates it; the CLI entry point execs it.
"""

from __future__ import annotations

import sys
from pathlib import Path

_BIN_NAME = "ecp.exe" if sys.platform == "win32" else "ecp"


def binary_path() -> Path:
    """Absolute path to the bundled ecp binary, or raise if the wheel for this
    platform was not the one installed (e.g. forced wrong-platform install)."""
    candidate = Path(__file__).parent / "_bin" / _BIN_NAME
    if not candidate.exists():
        raise FileNotFoundError(
            f"ecp: bundled binary missing at {candidate}. The installed wheel "
            f"does not match this platform ({sys.platform}). Reinstall with: "
            f"uv tool install --reinstall egent-code-plexus"
        )
    return candidate
