// Tests for scripts/dependency_report.mjs (node --test, part of scripts/check.sh).
// No network and no npm/cargo runs: the parsers, the range logic against the
// forms cargo really prints, and the report/issue rendering are pure.

import assert from "node:assert/strict";
import { test } from "node:test";

import {
  compareVersions,
  directRowsForTest,
  indexLinesFromVersions,
  indexEntries,
  isMajorChange,
  issueBody,
  newestAllowed,
  newestInRange,
  newestRelease,
  parseCargoUpdates,
  parseHistory,
  parseLockfile,
  parseManifestVersion,
  parseToolchain,
  prereleaseAhead,
  renderReport,
  satisfiesRange,
  sparseIndexPath,
} from "./dependency_report.mjs";

const lines = (versions) => indexLinesFromVersions("crate", versions);

test("versions compare like cargo: numerics, prereleases, build metadata", () => {
  assert.ok(compareVersions("1.2.3", "1.2.4") < 0);
  assert.ok(compareVersions("1.10.0", "1.9.9") > 0);
  assert.ok(compareVersions("0.8.0-alpha", "0.8.0") < 0);
  assert.ok(compareVersions("2.0.0-rc.2", "2.0.0-rc.10") < 0);
  assert.equal(compareVersions("1.1.4+spec-1.1.0", "1.1.4"), 0);
  assert.equal(isMajorChange("1.2.0", "2.0.0"), true);
  assert.equal(isMajorChange("1.2.0", "1.3.0"), false);
  // For 0.x crates the minor is the breaking position (semver for pre-1.0).
  assert.equal(isMajorChange("0.14.0", "0.15.0"), true);
  assert.equal(isMajorChange("0.33.5", "0.33.12"), false);
});

test("sparse index paths use the 1/2/3-letter rule", () => {
  assert.equal(sparseIndexPath("a"), "1/a");
  assert.equal(sparseIndexPath("ab"), "2/ab");
  assert.equal(sparseIndexPath("ort"), "3/o/ort");
  assert.equal(sparseIndexPath("log"), "3/l/log");
  assert.equal(sparseIndexPath("serde"), "se/rd/serde");
});

test("newestRelease skips prereleases and yanked rows", () => {
  assert.equal(newestRelease(lines(["1.0.0", "1.1.0-rc.1", "1.0.5"])), "1.0.5");
  const yanked = [JSON.stringify({ vers: "2.0.0", yanked: true }), JSON.stringify({ vers: "1.9.0", yanked: false })];
  assert.equal(newestRelease(yanked), "1.9.0");
});

test("newestInRange stays inside the locked range and ignores build metadata", () => {
  const index = lines(["0.9.2+spec-1.1.0", "0.9.4+spec-1.1.0", "1.0.0"]);
  assert.equal(newestInRange(index, "0.9.2+spec-1.1.0"), "0.9.4+spec-1.1.0");
  assert.equal(newestInRange(lines(["1.2.0", "1.3.0"]), "1.3.0"), "");
  // A prerelease moves within its own minor, to prereleases only.
  const pre = lines(["2.0.0-rc.12", "2.0.0-rc.15", "2.0.0", "2.1.0-rc.1"]);
  assert.equal(newestInRange(pre, "2.0.0-rc.12"), "2.0.0-rc.15");
});

test("newestAllowed follows the requirement cargo metadata reports", () => {
  const index = lines(["1.0.103", "1.0.104", "1.1.0", "2.0.0", "2.1.0-rc.1"]);
  assert.equal(newestAllowed(index, ["^1.0.104"], "1.0.104"), "1.1.0");
  // ~1.0.104 allows only a newer patch of 1.0; none is published here.
  assert.equal(newestAllowed(index, ["~1.0.104"], "1.0.104"), "");
  assert.equal(newestAllowed(lines(["1.0.104", "1.0.106", "1.1.0"]), ["~1.0.104"], "1.0.104"), "1.0.106");
  assert.equal(newestAllowed(index, ["=1.0.104"], "1.0.104"), "");
  assert.equal(newestAllowed(index, [">=1.0"], "1.0.104"), "2.0.0");
  // Never invents a release newer than the lock when the requirement is wider.
  assert.equal(newestAllowed(index, ["^1"], "1.0.104"), "1.1.0");
  const zero = lines(["0.8.1", "0.8.5", "0.9.0"]);
  assert.equal(newestAllowed(zero, ["^0.8.1"], "0.8.1"), "0.8.5");
  // A prerelease requirement spans its whole line up to that prerelease and
  // then the releases after it (rc.12 -> rc.15, and the stable 2.0.0).
  const pre = lines(["2.0.0-rc.9", "2.0.0-rc.12", "2.0.0-rc.15", "2.0.0-rc.10"]);
  assert.equal(newestAllowed(pre, ["=2.0.0-rc.9"], "2.0.0-rc.9"), "2.0.0-rc.15");
  assert.equal(newestAllowed(pre, ["=2.0.0-rc.12"], "2.0.0-rc.12"), "2.0.0-rc.15");
  assert.equal(newestAllowed(pre, ["=2.0.0-rc.15"], "2.0.0-rc.15"), "");
  assert.equal(newestAllowed(lines(["2.0.0-rc.15", "2.0.0"]), ["=2.0.0-rc.12"], "2.0.0-rc.12"), "2.0.0");
  assert.equal(prereleaseAhead(lines(["2.0.0-rc.12", "2.0.0-rc.15"]), "2.0.0-rc.12"), "");
  assert.equal(prereleaseAhead(lines(["2.0.0-rc.12", "2.0.0"]), "2.0.0-rc.12"), "2.0.0");
  assert.equal(prereleaseAhead(lines(["1.2.3", "1.3.0"]), "1.2.3"), "");
});

test("package.json ranges decide npm wanted lines", () => {
  assert.equal(satisfiesRange("19.3.1", "^19.2.8"), true);
  assert.equal(satisfiesRange("20.0.0", "^19.2.8"), false);
  // npm reads a bare "0.8.1" as "^0.8.1": 0.8.2+ compatible, 0.9.0 is not.
  assert.equal(satisfiesRange("0.8.5", "0.8.1"), true);
  assert.equal(satisfiesRange("0.9.0", "0.8.1"), false);
  assert.equal(satisfiesRange("3.0.7", "~3.0.6"), true);
  assert.equal(satisfiesRange("3.1.0", "~3.0.6"), false);
  assert.equal(satisfiesRange("1.33.0", "1.33.0"), true);
});

test("cargo update dry-run output parses, including a requirement change", () => {
  const { updates, rustVersion } = parseCargoUpdates(
    [
      "    Updating crates.io index",
      "     Locking 2 packages to latest Rust 1.88 compatible versions",
      "    Updating serde v1.0.210 -> v1.0.214",
      " Downgrading foo v2.0.0 -> v1.9.0",
      "    Updating bar v0.9.0 -> [1]",
      "note: pass `--verbose` to see 173 unchanged dependencies behind latest",
      "warning: not updating lockfile due to dry run",
    ].join("\n"),
  );
  assert.equal(rustVersion, "1.88");
  assert.deepEqual(updates, [
    { crate: "serde", from: "1.0.210", to: "1.0.214" },
    { crate: "foo", from: "2.0.0", to: "1.9.0" },
    { crate: "bar", from: "0.9.0", to: "[1]" },
  ]);
});

test("Cargo.lock and manifests are read without cargo", () => {
  const lock = parseLockfile(
    [
      "version = 4",
      "",
      "[[package]]",
      'name = "serde"',
      'version = "1.0.214"',
      'source = "registry+https://github.com/rust-lang/crates.io-index"',
      "",
      "[[package]]",
      'name = "rawler"',
      'version = "0.7.1"',
      "",
    ].join("\n"),
  );
  assert.deepEqual(lock, [
    { name: "serde", version: "1.0.214", source: "registry+https://github.com/rust-lang/crates.io-index" },
    { name: "rawler", version: "0.7.1", source: "local" },
  ]);
  assert.equal(parseToolchain('[toolchain]\nchannel = "1.98.1"\ncomponents = ["rustfmt"]\n'), "1.98.1");
  assert.equal(parseManifestVersion('[package]\nname = "rawler"\nversion = "0.7.1"\n\n[dependencies]\ntoml = "=0.8"\n'), "0.7.1");
});

test("the rendered report keeps the required sections and marks majors", () => {
  const data = {
    errors: [],
    npm: {
      audit: [],
      devAudit: [{ name: "glob", severity: "moderate", kind: "transitive", advisories: "Glob: AbortSignal", fix: "available" }],
      outdated: [
        { name: "framer-motion", current: "13.1.1", wanted: "13.5.1", latest: "14.9.0", major: true, usedBy: "prod", stale: false },
      ],
    },
    rust: {
      updates: [{ crate: "serde", from: "1.0.210", to: "1.0.214" }],
      direct: [
        { name: "ort", req: "=2.0.0-rc.12", pinned: true, ours: "2.0.0-rc.12", wanted: "2.0.0-rc.15", prelock: "2.0.0", lineNewest: "2.0.0", latest: "2.0.0-rc.15", behind: true, major: false, note: "" },
        { name: "glam", req: "^0.33.5", pinned: false, ours: "0.33.5", wanted: "0.33.12", prelock: "", latest: "0.34.2", behind: true, major: true, note: "" },
      ],
    },
    rawler: { local: "0.7.1", commit: "934af4b213a12b95", cratesIo: "0.8.0", fork: "0.8.0", upstream: "0.8.0", behind: true },
    toolchain: { pinned: "1.98.1", stable: "1.99.1", behind: true },
    branch: null,
    runUrl: null,
  };
  const report = renderReport(data, new Date("2026-11-03T05:17:00Z"));
  for (const heading of [
    "### Summary",
    "### npm security",
    "### npm outdated",
    "### Rust: lockfile updates",
    "### Rust: direct crates.io dependencies",
    "### Vendored rawler and Rust toolchain",
  ]) {
    assert.ok(report.includes(heading), heading);
  }
  assert.match(report, /\| framer-motion \| 13\.1\.1 \| 13\.5\.1 \| 14\.9\.0 \| \*\*major\*\* \| prod \|/);
  assert.match(report, /\| serde \| 1\.0\.210 \| 1\.0\.214 \|/);
  // In range it may move to rc.15 (the pinned `=` blocks only the lockfile),
  // and the stable release of that line is the news.
  assert.match(report, /`=2\.0\.0-rc\.12` pinned; a manifest edit/);
  assert.match(report, /ort \| 2\.0\.0-rc\.12 \| 2\.0\.0-rc\.15 \(prerelease\) \|/);
  assert.ok(!report.includes("renamed upstream"), "no rename note without one");
  // Two blank lines would end an inside-the-issue section early.
  assert.ok(!/\n\n\n/.test(report), "no triple newline");
});

test("the issue body carries the status and folds the previous run", () => {
  const previous = issueBody({
    report: "old report body",
    status: "1 npm package behind",
    at: new Date("2026-10-27T05:17:00Z"),
    runUrl: "https://github.com/o/r/actions/runs/1",
  });
  const history = parseHistory(previous);
  assert.equal(history.length, 1);
  assert.equal(history[0].report.trim(), "old report body");
  assert.equal(history[0].status, "1 npm package behind");
  assert.equal(history[0].runUrl, "https://github.com/o/r/actions/runs/1");
  const body = issueBody({
    report: "new report body",
    status: "nothing behind",
    at: new Date("2026-11-03T05:17:00Z"),
    runUrl: "https://github.com/o/r/actions/runs/2",
    history,
  });
  assert.match(body, /^\*\*Status:\*\* nothing behind$/m);
  assert.match(body, /## Current report\n\nnew report body/);
  assert.match(body, /## Earlier runs/);
  assert.match(body, /<summary>2026-10-27T05:17:00Z — 1 npm package behind/);
  // The next round folds both and keeps only the current one open.
  assert.equal(parseHistory(body).length, 2);
});

test("only the rows that wait are listed when the lockfile already has what is newer", () => {
  const direct = [
    { name: "a", ours: "1.0.0", wanted: "", prelock: "", latest: "1.0.0", major: false, behind: false, note: "", pinned: false },
    { name: "b", ours: "1.0.0", wanted: "1.0.1", prelock: "", latest: "1.0.1", major: false, behind: true, note: "", pinned: false, req: "^1.0" },
    { name: "c", ours: "1.0.0", wanted: "", prelock: "", latest: "2.0.0", major: true, behind: true, note: "", pinned: false },
  ];
  const names = (rows) => rows.map((row) => row[0]);
  // With a lockfile update listed above, what waits (in range or a major).
  assert.deepEqual(names(directRowsForTest(direct, true)), ["c", "b"]);
  // Without one, "newest in range" is ours anyway, so only the moves that
  // need a manifest edit; "b"'s in-range release is already in the lockfile.
  assert.deepEqual(names(directRowsForTest(direct, false)), ["c"]);
  // A pinned requirement whose own range already has something newer.
  const pinned = [{ ...direct[1], name: "d", pinned: true, req: "=1.0.0" }];
  assert.deepEqual(names(directRowsForTest(pinned, false)), ["d"]);
  const stale = [{ ...direct[2], name: "e", incomplete: true }];
  assert.deepEqual(names(directRowsForTest(stale, false)), []);
  assert.deepEqual(indexEntries(lines(["1.0.0"])), ["1.0.0"]);
});
