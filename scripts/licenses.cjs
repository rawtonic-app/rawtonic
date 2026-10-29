#!/usr/bin/env node
/**
 * `npm run licenses` on every platform: finds a Python 3 and runs
 * `third_party_licenses.py` with the arguments unchanged.
 *
 * This entry point is Node, not Python, on purpose. npm runs a script through
 * `sh` on Unix but through `cmd.exe` on Windows, where the interpreter has
 * another name. A Python entry point would have to be started by a Python that
 * may not exist yet: on Windows the `python3.exe` on PATH is the Microsoft
 * Store's app alias, which prints its own hint and exits 9009 before any
 * Python code runs. Node (a prerequisite of this repository) has no such
 * problem and starts every candidate without a shell, so no candidate name is
 * ever expanded or re-quoted by `cmd.exe`.
 *
 * Candidates, first that answers with a real Python 3:
 * - Unix: `python3`, as the scripts and CI call it.
 * - Windows: `py -3` (the launcher installed with Python 3 chooses the real
 *   `python.exe`), then `python` (a PATH that carries only `python.exe`).
 *   `python3` is not tried there: it is the Store alias that exits 9009 when
 *   the Store app is not installed.
 *
 * A candidate counts only if `-c` reports a Python 3, so an alias or an old
 * Python 2 falls through to the next one instead of failing confusingly.
 * The child's stdout/stderr go straight to the terminal (progress and errors
 * stay streamed), and its exit status (or killing signal) becomes ours.
 * Args pass through unchanged: `--check` from scripts/check.sh, `--out FILE`.
 *
 * The launcher must not change the collector's generated output.
 */
"use strict";

const { spawnSync } = require("node:child_process");
const path = require("node:path");

const COLLECTOR = path.join(__dirname, "third_party_licenses.py");
const PROBE = "import sys; sys.exit(0 if sys.version_info[0] == 3 else 1)";
const CANDIDATES = process.platform === "win32" ? [["py", "-3"], ["python"]] : [["python3"]];

function findPython() {
  const failed = [];
  for (const candidate of CANDIDATES) {
    const name = candidate.join(" ");
    // Stdin and output are discarded: this is a capability probe, not a run.
    const probe = spawnSync(candidate[0], [...candidate.slice(1), "-c", PROBE], {
      stdio: "ignore",
      windowsHide: true,
    });
    // ENOENT: program not on PATH. Non-zero: not Python 3 (Python 2, or the
    // Microsoft Store alias, which exits 9009).
    if (probe.error?.code === "ENOENT") {
      failed.push(`${name} (not found)`);
      continue;
    }
    if (probe.error) {
      failed.push(`${name} (${probe.error.message})`);
      continue;
    }
    if (probe.signal) {
      failed.push(`${name} (killed by ${probe.signal})`);
      continue;
    }
    if (probe.status === 0) {
      return candidate;
    }
    failed.push(`${name} (exit ${probe.status})`);
    // With a choice left, say why it is skipped, so a Store alias on PATH is
    // visible instead of a silent fallback.
    if (candidate !== CANDIDATES[CANDIDATES.length - 1]) {
      process.stderr.write(
        `licenses: ${name} is not Python 3 (exit ${probe.status}); trying the next.\n`,
      );
    }
  }
  const tried = failed.length ? `; tried ${failed.join(", ")}` : "";
  process.stderr.write(
    `licenses: no Python 3 interpreter found${tried}. ` +
      "Install Python 3 and make it available on PATH.\n",
  );
  return null;
}

function main() {
  const python = findPython();
  if (!python) {
    return 1;
  }
  const child = spawnSync(python[0], [...python.slice(1), COLLECTOR, ...process.argv.slice(2)], {
    stdio: "inherit",
    windowsHide: true,
  });
  if (child.error) {
    process.stderr.write(`licenses: could not start ${python.join(" ")}: ${child.error.message}\n`);
    return 1;
  }
  if (child.signal) {
    // Killed by a signal: report it the way a shell would (128 + signal).
    const numbers = { SIGHUP: 1, SIGINT: 2, SIGQUIT: 3, SIGPIPE: 13, SIGTERM: 15, SIGKILL: 9 };
    return 128 + (numbers[child.signal] ?? 0);
  }
  return child.status;
}

process.exitCode = main();
