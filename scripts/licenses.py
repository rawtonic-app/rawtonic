#!/usr/bin/env python3
"""`npm run licenses` on every platform: runs `third_party_licenses.py`.

npm runs a script through `sh` on Unix and `cmd.exe` on Windows, where the
interpreter has a different name — hence this entry point, started by
`package.json`'s `licenses` script. The collector itself is unchanged.
Args pass through unchanged (`--check` in scripts/check.sh, `--out`).
"""
import subprocess
import sys
from pathlib import Path

collector = Path(__file__).resolve().parent / "third_party_licenses.py"
# Unix: `python3`, as the scripts and CI call it. Windows: the `py` launcher
# (installed with Python 3) chooses the platform's real `python.exe`; the
# `python3.exe` on PATH there is the Microsoft Store's app alias, which exits
# 9009 when the Store app is not installed. `py` is not everywhere (a PATH
# that only carries a python.exe), so it falls back to `python`.
if sys.platform == "win32":
    launchers = (["py", "-3"], ["python"])
else:
    launchers = (["python3"],)
for launcher in launchers:
    try:
        done = subprocess.run([*launcher, str(collector), *sys.argv[1:]])
    except OSError:
        continue
    sys.exit(done.returncode)
tried = ", ".join(launcher[0] for launcher in launchers)
sys.exit(f"licenses: no Python 3 interpreter found (tried {tried})")
