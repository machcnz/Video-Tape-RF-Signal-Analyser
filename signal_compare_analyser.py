#!/usr/bin/env python3
"""
VHS RF Signal Analyser v6.5
Thin wrapper that delegates analysis to the native Rust backend.
"""

import os
import sys
import subprocess
from pathlib import Path


def find_compare_rf() -> Path:
    script_dir = Path(__file__).resolve().parent
    candidates = [
        script_dir / "compare-rf.exe",
        script_dir / "target" / "release" / "compare-rf.exe",
        Path.cwd() / "compare-rf.exe",
        Path.cwd() / "target" / "release" / "compare-rf.exe",
    ]
    for candidate in candidates:
        if candidate.exists():
            return candidate
    raise FileNotFoundError("compare-rf.exe not found")


def main() -> int:
    try:
        backend = find_compare_rf()
    except FileNotFoundError as exc:
        print(f"Error: {exc}", file=sys.stderr)
        return 1

    env = os.environ.copy()
    backend_dir = str(backend.parent)
    env["PATH"] = backend_dir + os.pathsep + env.get("PATH", "")

    result = subprocess.run([str(backend), *sys.argv[1:]], env=env)
    return int(result.returncode)


if __name__ == "__main__":
    sys.exit(main())
