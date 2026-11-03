// Tests for scripts/dependency_report.mjs (node --test, part of scripts/check.sh).
// No network and no npm/cargo runs: the parsers, the range logic against the
// forms cargo really prints, and the report/issue rendering are pure.

import assert from "node:assert/strict";
import { mkdirSync, mkdtempSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import path from "node:path";
import { test } from "node:test";

import {
  behindList,
  cratesIoCacheMtimes,
  createRequester,
  compareVersions,
  directRowsForTest,
  heldBack,
  highestRustVersion,
  indexLinesFromVersions,
  indexEntries,
  indexRowsFor,
  indexRowsWithSpans,
  isMajorChange,
  issueBody,
  newestAllowed,
  newestInRange,
  newestRelease,
  parseHistory,
  parseLockfile,
  parseCargoUpdates,
  parseManifestVersion,
  parseToolchain,
  readCargoIndexCache,
  prereleaseAhead,
  renderReport,
  satisfiesRange,
  summaryItems,
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

/** A direct-dependency row as rustDirectSection builds it (fields the table reads). */
const row = (fields) => ({
  ours: "1.0.0",
  wanted: "",
  held: "",
  heldReason: "",
  prelock: "",
  latest: "1.0.0",
  major: false,
  behind: false,
  note: "",
  pinned: false,
  req: "^1.0",
  incomplete: false,
  ...fields,
});

const rowsFor = (rows) =>
  directRowsForTest(rows).map((cells) => ({ name: cells[0], inRange: cells[2], newest: cells[3], note: cells[5] }));

test("the table lists only what waits, and a release cargo does not take is held back", () => {
  const direct = [
    row({ name: "a" }),
    // In range and the lockfile already holds it: current, stays out.
    row({ name: "b", wanted: "", latest: "1.0.0" }),
    // A release `cargo update` takes.
    row({ name: "c", wanted: "1.0.1", latest: "1.0.1", behind: true }),
    // A release the requirement allows that cargo does not take: in the
    // table as held back, never as the newest cargo would give.
    row({
      name: "glam",
      req: "^0.33.5",
      ours: "0.33.5",
      wanted: "",
      held: "0.33.12",
      heldReason: "not taken by cargo; it names no reason (another dependency's requirement)",
      latest: "0.34.2",
      major: true,
      behind: true,
    }),
    // A pinned requirement whose own range already has something newer.
    row({ name: "d", pinned: true, req: "=1.0.0", wanted: "1.0.1", latest: "1.0.1" }),
    // Index rows older than the lock, no major reading: checked is unknown.
    row({ name: "e", incomplete: true, latest: null }),
    // The name is not on crates.io at all (HTTP 404).
    row({ name: "f", latest: null, note: "not found on crates.io (HTTP 404)" }),
  ];
  const shown = rowsFor(direct);
  assert.deepEqual(
    shown.map((entry) => entry.name),
    ["glam", "c", "d", "f"],
  );
  const glamCell = shown.find((entry) => entry.name === "glam");
  assert.equal(glamCell.inRange, "0.33.12 (held back)");
  assert.match(glamCell.note, /^in range, held back \(not taken by cargo/);
  assert.equal(shown.find((entry) => entry.name === "c").inRange, "1.0.1");
  assert.equal(shown.find((entry) => entry.name === "d").note, "`=1.0.0` pinned; a manifest edit");
  // A requirement whose take list is newer as well shows take, then hold.
  const both = rowsFor([row({ name: "x", wanted: "1.0.1", held: "1.0.2", heldReason: "needs Rust 1.90; cargo resolves for Rust 1.88" })]);
  assert.equal(both[0].inRange, "1.0.1, then 1.0.2 (held back)");
  assert.deepEqual(indexEntries(lines(["1.0.0"])), ["1.0.0"]);
});

// MARK: crates.io answers

const answer = (status, body = "") => ({ status, ok: status >= 200 && status < 300, text: async () => body });

test("a 304 is Not Modified: no conditional header goes out, and it is never read as 'not found'", async () => {
  const rows = indexLinesFromVersions("ort", ["2.0.0-rc.12", "2.0.0-rc.15"]);
  const requests = [];
  // The promise-style fetch the script hands to its requester.
  const conditional = (url, options) => {
    requests.push({ url, headers: options.headers });
    // A cache that got a conditional header it cannot fulfil answers 304.
    const ifNoneMatch = options.headers["If-None-Match"] ?? options.headers["if-none-match"];
    return Promise.resolve(ifNoneMatch ? answer(304) : answer(200, rows.join("\n")));
  };
  const index = await indexRowsFor(["ort"], new Map([["ort", ["2.0.0-rc.12"]]]), createRequester(conditional));
  assert.ok(requests.length > 0);
  assert.ok(requests.every(({ headers }) => !("If-None-Match" in headers) && !("If-Modified-Since" in headers)), "no conditional headers");
  assert.equal(index.get("ort").indexKnown, true);
  assert.equal(index.get("ort").notFound, false);
  // The 200 body is read as it arrives (no span entry in this test, so the
  // rows stand as cargo lists them).
  assert.equal(newestAllowed(index.get("ort").rows, ["=2.0.0-rc.12"], "2.0.0-rc.12"), "2.0.0-rc.15");
  // Only a 404 says the name is not on crates.io.
  const notFound = await indexRowsFor(["ort"], new Map(), createRequester(() => Promise.resolve(answer(404))));
  assert.deepEqual(
    { notFound: notFound.get("ort").notFound, error: notFound.get("ort").error },
    { notFound: true, error: "ort: HTTP 404 on the crates.io index" },
  );
  // A 304 with no body is a status, not proof of a rename.
  const notModified = await indexRowsFor(["ort"], new Map([["ort", ["2.0.0-rc.12"]]]), createRequester(() => Promise.resolve(answer(304))));
  assert.equal(notModified.get("ort").notFound, false);
  assert.equal(notModified.get("ort").indexKnown, false);
  assert.match(notModified.get("ort").error, /304 \(Not Modified\)/);
});

test("ort: a newer prerelease is in range for an `=` prerelease pin, and a stable line above the MSRV is held back", () => {
  const now = Date.parse("2026-11-03T12:00:00Z");
  const ort = indexLinesFromVersions("ort", [
    "2.0.0-rc.12",
    { vers: "2.0.0-rc.15", rustVersion: "1.92", publishedAt: "2026-10-19T07:00:00Z" },
  ]);
  // Prereleases count as newer for "=2.0.0-rc.12": rc.15 is the lockfile update.
  assert.equal(newestAllowed(ort, ["=2.0.0-rc.12"], "2.0.0-rc.12"), "2.0.0-rc.15");
  // rc.15's rust-version (1.92) is above cargo's resolved 1.88 here: held
  // back and named, never "renamed upstream" with an empty Newest.
  assert.deepEqual(heldBack({ fresh: false, cargoRustVersion: "1.88" }, ort, ["=2.0.0-rc.12"], "2.0.0-rc.12", { now }), {
    version: "2.0.0-rc.15",
    reason: "needs Rust 1.92; cargo resolves for Rust 1.88",
  });
  // rc.15 is the first rc after rc.12: the update is a lockfile change.
  assert.equal(newestInRange(ort, "2.0.0-rc.12"), "2.0.0-rc.15");
  // The stable of the line is in the requirement's range too (`=` spans the
  // line), and its rust-version 1.92 is above what cargo resolves for: not
  // "renamed upstream", not silently missing — held back and named.
  const withStable = ort.concat(indexLinesFromVersions("ort", [{ vers: "2.0.0", rustVersion: "1.92" }]));
  assert.equal(newestInRange(withStable, "2.0.0-rc.12", true), "2.0.0");
  assert.equal(newestAllowed(withStable, ["=2.0.0-rc.12"], "2.0.0-rc.12"), "2.0.0");
  assert.deepEqual(heldBack({ fresh: false, cargoRustVersion: "1.88" }, withStable, ["=2.0.0-rc.12"], "2.0.0-rc.12", { now }), {
    version: "2.0.0",
    reason: "needs Rust 1.92; cargo resolves for Rust 1.88",
  });
});

test("glam: an in-range release cargo does not take is held back, never 'newest in range'", () => {
  const glam = indexLinesFromVersions("glam", [
    "0.33.5",
    { vers: "0.33.12", rustVersion: "1.68.2", publishedAt: "2026-10-01T10:00:48Z" },
    { vers: "0.34.2", rustVersion: "1.85.0", publishedAt: "2026-11-01T10:38:50Z" },
  ]);
  // `cargo update --workspace --dry-run --verbose` names glam and updates
  // nothing: cargo's own verdict is that it does not take 0.33.12.
  const behind = behindList("   Unchanged glam v0.33.5 (available: v0.33.12)");
  const held = { version: "0.33.12", reason: 'cargo leaves it out ("glam v0.33.5 (available: v0.33.12)" unchanged)' };
  assert.deepEqual(heldBack({ behind, cargoRustVersion: "1.88" }, glam, ["^0.33.5"], "0.33.5"), held);
  // No list from cargo (an older cargo, verbose off): no reason is claimed —
  // the row is then news, not a cargo refusal.
  assert.equal(heldBack({ behind: behindList("Locking 0 packages"), cargoRustVersion: "1.88" }, glam, ["^0.33.5"], "0.33.5"), null);
  // The lockfile already holds 0.33.12, or cargo's list is about another
  // crate: nothing held.
  assert.equal(heldBack({ behind: { glam: { ours: "0.33.12", available: "0.33.12", listed: true } }, cargoRustVersion: "1.88" }, glam, ["^0.33.5"], "0.33.5"), null);
  assert.equal(heldBack({ behind: { wgpu: { ours: "29.0.4", available: "30.0.1", listed: true } }, cargoRustVersion: "1.88" }, glam, ["^0.33.5"], "0.33.5"), null);
  // Without a reason the row is a plain in-range update (the lockfile section
  // above lists it).
  assert.equal(newestAllowed(glam, ["^0.33.5"], "0.33.5"), "0.33.12");
});

test("the cargo cache is read as (version, row) records, and a span fills the releases cargo skipped", () => {
  const dir = mkdtempSync(path.join(tmpdir(), "depw-index-"));
  const cacheDir = path.join(dir, ".cache", "3", "o");
  mkdirSync(cacheDir, { recursive: true });
  const record = (vers, row) => Buffer.concat([Buffer.from(`${vers}\0`), Buffer.from(`${JSON.stringify(row)}`), Buffer.from([0])]);
  const span = { vers: "(span)", deps: JSON.stringify(["=2.0.0-rc.12", "2.0.0-rc.12"]), features: { force: null } };
  const file = Buffer.concat([
    Buffer.from([4, 0, 0, 0]),
    Buffer.from(`etag: "93c2c14d"`),
    Buffer.from([0]),
    record("2.0.0-rc.12", { name: "ort", vers: "2.0.0-rc.12", deps: [], yanked: false }),
    record("(span)", span),
    record("2.0.0-rc.15", { name: "ort", vers: "2.0.0-rc.15", deps: [], rust_version: "1.92", yanked: false }),
  ]);
  writeFileSync(path.join(cacheDir, "ort"), file);
  const cache = readCargoIndexCache(dir);
  // The cache holds every row (the span entry is kept for its span), and the
  // span takes it out of the release rows. A file that already carries a span
  // came from another resolver than this run's, so the crate is listed as
  // spanned.
  assert.equal(cache.crates.get("ort").length, 3);
  assert.deepEqual([...cache.spanned], ["ort"]);

  // The span [=2.0.0-rc.12, 2.0.0-rc.12] covers every 2.0.0 row: cargo only
  // takes one release out of it, so the rest were skipped. With 2.0.0
  // published, the span ends in the stable release.
  const index = indexLinesFromVersions("ort", [
    { vers: "2.0.0-rc.12", yanked: false },
    "2.0.0-rc.15",
    { vers: "2.0.0", rustVersion: "1.92" },
  ]);
  // 2.0.0-rc.12 is in a span, so its own index file lists no rows for it; a
  // crate with none of these spans keeps every row as cargo lists it.
  const rows = indexRowsWithSpans(index.concat([JSON.stringify(span)]), ["2.0.0-rc.12"]);
  const byVersion = Object.fromEntries(rows.map((entry) => [entry.vers, entry]));
  // Cargo takes one release out of the span: with a stable lockfile release
  // cargo's own list names it (the lockfile was updated), a --force-era
  // prerelease is no member.
  assert.equal(byVersion["2.0.0-rc.12"].member, undefined, "a prerelease is never a span member");
  // Cargo's list names the span's release; without it the line's newest
  // stable release stands in (here the published 2.0.0).
  assert.equal(byVersion["2.0.0"].member, true);
  assert.equal(byVersion["2.0.0"].covered, undefined);
  const stated = Object.fromEntries(
    indexRowsWithSpans(index.concat([JSON.stringify(span)]), ["2.0.0"], { ort: { ours: "2.0.0", available: "2.0.0" } }).map((entry) => [
      entry.vers,
      entry,
    ]),
  );
  assert.equal(stated["2.0.0"].member, true, "cargo's list names the span's member");
  assert.equal(stated["2.0.0-rc.12"].member, undefined);
  // A prerelease requirement (`=2.0.0-rc.12`) reaches rc.15 inside the span,
  // and rows above the span's member stay readable: a release in range stays
  // in the row, never "not found".
  const prereleases = indexLinesFromVersions("ort", ["2.0.0-rc.12", "2.0.0-rc.15"]).concat([JSON.stringify(span)]);
  assert.equal(newestAllowed(prereleases, ["=2.0.0-rc.12"], "2.0.0-rc.12"), "2.0.0-rc.15");
  assert.equal(heldBack({ behind: {} }, prereleases, ["=2.0.0-rc.12"], "2.0.0-rc.12"), null, "a span holds the range; no reason is claimed without cargo's list");
});

test("cargo's own list of what it left alone is the evidence, and the member MSRV", () => {
  const text = [
    "     Locking 0 packages to latest Rust 1.88 compatible versions",
    "   Updating serde v1.0.1 -> v1.0.2",
    "   Unchanged aes v0.9.2 (available: v0.9.4)",
    "   Unchanged glam v0.33.5 (available: v0.33.12)",
  ].join("\n");
  const behind = behindList(text);
  assert.deepEqual(behind.glam, { ours: "0.33.5", available: "0.33.12", listed: true });
  assert.equal(behind.serde.reason, "Updating serde v1.0.1 -> v1.0.2");
  // No list (an older cargo): no reason is claimed from a missing record.
  assert.deepEqual(behindList("Locking 0 packages"), { unavailable: true });
  // A move cargo did take is not a hold (the lockfile section lists it).
  assert.deepEqual(behindList(["     Locking 1 package", "   Updating glam v0.33.5 -> v0.33.12"].join("\n")).glam, {
    ours: "0.33.5",
    available: "0.33.12",
    reason: "Updating glam v0.33.5 -> v0.33.12",
  });
  const metadata = {
    workspace_members: ["app", "core", "rawler"],
    packages: [
      { id: "app", rust_version: "1.98" },
      { id: "core", rust_version: null },
      { id: "rawler", rust_version: "1.88" },
      { id: "dep", rust_version: "1.99" },
    ],
  };
  assert.equal(highestRustVersion(metadata), "1.98");
});

test("the crates.io index file mtimes tell a file cargo rewrote from one it did not", () => {
  const root = mkdtempSync(path.join(tmpdir(), "depw-mtime-"));
  const cacheDir = path.join(root, "index.crates.io-1949cf8c6b5b557f", ".cache", "3", "o");
  mkdirSync(cacheDir, { recursive: true });
  writeFileSync(path.join(cacheDir, "ort"), "");
  const times = cratesIoCacheMtimes(root, { mtimeOf: () => 1700000000000 });
  assert.equal(times.get("ort"), 1700000000000, "the file name is the crate name");
  assert.equal(cratesIoCacheMtimes(null).size, 0);
});

test("the dry run's unchanged count and the held-back row reach summary and the 'no contradiction' text", () => {
  const { behind } = parseCargoUpdates(
    ["     Locking 0 packages to latest Rust 1.88 compatible versions", "note: pass `--verbose` to see 166 unchanged dependencies behind latest"].join("\n"),
  );
  assert.equal(behind, 166);
  const data = {
    errors: [],
    npm: null,
    rust: {
      updates: [],
      rustVersion: "1.88",
      behind: 0,
      msrv: "1.98",
      direct: [
        // A prerelease move `cargo update` may take: a move, not a hold.
        row({ name: "ort", req: "=2.0.0-rc.12", pinned: true, ours: "2.0.0-rc.12", wanted: "2.0.0-rc.15", prelock: "2.0.0", latest: "2.0.0-rc.15", behind: true }),
        // In range but past the member's rust-version: cargo leaves it alone.
        row({ name: "tauri", req: "^2.12", ours: "2.12.1", wanted: "2.12.5", held: "2.12.5", heldReason: "needs Rust 1.90; cargo resolves for Rust 1.88" }),
        row({
          name: "glam",
          req: "^0.33.5",
          ours: "0.33.5",
          // The newest the requirement allows is the held one: cargo takes no
          // move at all here, so the cell names one release.
          wanted: "0.33.12",
          held: "0.33.12",
          heldReason: "not taken by cargo; it names no reason (another dependency's requirement)",
          latest: "0.34.2",
          major: true,
          behind: true,
        }),
      ],
    },
    rawler: null,
    toolchain: null,
  };
  const report = renderReport(data, new Date("2026-11-03T12:00:00Z"));
  assert.ok(!report.includes("renamed upstream"));
  assert.match(report, /\| glam \| 0\.33\.5 \| 0\.33\.12 \(held back\) \| 0\.34\.2 \| \*\*major\*\* \| in range, held back/);
  assert.match(report, /\| ort \| 2\.0\.0-rc\.12 \| 2\.0\.0-rc\.15 \(prerelease\) \|/);
  // The empty lockfile section no longer claims every in-range release is in
  // the lockfile; it names what keeps cargo from taking them.
  assert.ok(!/No lockfile update is possible now/.test(report));
  assert.match(report, /rust-version` 1\.98 is above the Rust 1\.88/);
  const items = summaryItems(data);
  // A held release is not a move: only `ort` counts as movable, and the two
  // holds (`glam`, `tauri`) a summary line of their own.
  assert.ok(items.some((item) => item.startsWith("1 direct crate(s) can move within their requirement")), items.join("\n"));
  assert.ok(items.some((item) => item.startsWith("1 direct crate(s) can move within their requirement")), items.join("\n"));
});
