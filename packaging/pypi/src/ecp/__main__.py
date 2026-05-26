"""`python -m ecp` and the `ecp` console-script entry point.

Replace the current process with the native binary so signals, exit codes, and
stdio pass through transparently (no Python wrapper left in the process tree).
"""

from __future__ import annotations

import os
import subprocess
import sys

from . import binary_path


def main() -> int | None:
    binary = str(binary_path())
    argv = [binary, *sys.argv[1:]]
    # POSIX: execv hands the process over entirely — zero overhead, native signals.
    if sys.platform != "win32":
        os.execv(binary, argv)
    # Windows lacks a real execv; forward via subprocess and mirror the exit code.
    return subprocess.run(argv, check=False).returncode


if __name__ == "__main__":
    raise SystemExit(main())
