// Tests for scripts/dependency_report.mjs (node --test, part of scripts/check.sh).
// No network and no npm/cargo runs: cargo's and npm's output is taken as
// literals from this file (recorded runs on the laptop, 2026-11-06), and the
// report and issue-body rendering are pure.

import assert from "node:assert/strict";
import { test } from "node:test";

import {
  buildReport,
  collect,
  compareVersions,
  exitCode,
  fitIssueBody,
  isMajorChange,
  issueBody,
  lockfileSections,
  MAX_ISSUE_BODY,
  npmSection,
  parseAudit,
  parseCargoReport,
  parseHistory,
  parseToolchainChannel,
  renderReport,
  summaryItems,
  table,
} from "./dependency_report.mjs";

test("versions compare like cargo: numerics, prereleases, build metadata", () => {
  assert.ok(compareVersions("1.2.3", "1.2.4") < 0);
  assert.ok(compareVersions("1.10.0", "1.9.9") > 0);
  assert.ok(compareVersions("0.8.0-alpha", "0.8.0") < 0);
  assert.ok(compareVersions("2.0.0-rc.2", "2.0.0-rc.10") < 0);
  assert.equal(compareVersions("1.1.4+spec-1.1.0", "1.1.4"), 0);
  assert.equal(isMajorChange("1.2.0", "2.0.0"), true);
  // For 0.x crates the minor is the breaking position.
  assert.equal(isMajorChange("0.14.0", "0.15.0"), true);
  assert.equal(parseToolchainChannel('[toolchain]\nchannel = "1.98.1"\n'), "1.98.1");
});

// MARK: cargo output (the lines are copied from real dry runs, colors included)

const DRY_RUN = [
  "\u001b[1m\u001b[92m    Updating\u001b[0m crates.io index",
  "\u001b[1m\u001b[92m    Updating\u001b[0m git repository `https://github.com/CyberTimon/RapidRAW-DngLab.git`",
  "\u001b[1m\u001b[92m     Locking\u001b[0m 170 packages to latest Rust 1.88 compatible versions",
  "\u001b[1m\u001b[92m    Updating\u001b[0m addr2line v0.25.1 -> v0.27.1",
  "\u001b[1m\u001b[92m      Adding\u001b[0m bit-set v0.11.1",
  "\u001b[1m\u001b[92m    Updating\u001b[0m daachorse v1.0.1 -> v3.0.3",
  "\u001b[1m\u001b[92m Downgrading\u001b[0m ordered-float v5.5.0 -> v5.4.0 (available: v5.5.1)",
  "\u001b[1m\u001b[92m    Removing\u001b[0m png v0.17.16",
  "note: pass `--verbose` to see 21 unchanged dependencies behind latest",
  "warning: not updating lockfile due to dry run",
].join("\n");

test("the dry run's changes parse: index lines and warnings are no changes", () => {
  const report = parseCargoReport(DRY_RUN);
  assert.equal(report.rustVersion, "1.88");
  assert.deepEqual(
    report.changes.map(({ kind, name, from, to }) => [kind, name, from, to]),
    [
      ["Updating", "addr2line", "0.25.1", "0.27.1"],
      ["Adding", "bit-set", "0.11.1", ""],
      ["Updating", "daachorse", "1.0.1", "3.0.3"],
      ["Downgrading", "ordered-float", "5.5.0", "5.4.0"],
      ["Removing", "png", "0.17.16", ""],
    ],
  );
  assert.equal(report.changes.find((change) => change.name === "ordered-float").available, "5.5.1");
});

test("--verbose's Unchanged list and an old cargo's resolver MSRV", () => {
  const verbose = parseCargoReport(
    [
      "     Locking 0 packages to latest Rust 1.88 compatible versions",
      "   Unchanged glam v0.33.5 (available: v0.33.12)",
      "   Unchanged toml v0.8.2 (available: v1.1.4+spec-1.1.0)",
      "warning: not updating lockfile due to dry run",
    ].join("\n"),
  );
  assert.deepEqual(verbose.behind.glam, { ours: "0.33.5", available: "0.33.12" });
  assert.deepEqual(verbose.changes, []);
  const old = parseCargoReport("debug get_rust_version: Some(1.92.0)\n");
  assert.equal(old.msrv, "1.92.0");
  // A line cargo prints as an error is news, not silence.
  assert.deepEqual(parseCargoReport("error: failed to get `serde` as a dependency").errors, ["error: failed to get `serde` as a dependency"]);
});

test("the lockfile section splits moves inside a requirement from re-resolved ones", () => {
  const lockfile = lockfileSections(parseCargoReport(DRY_RUN));
  assert.deepEqual(
    lockfile.takes.map((row) => [row.crate, row.from, row.to, row.kind]),
    [
      ["ordered-float", "5.5.0", "5.4.0", "downgrade"],
      ["png", "0.17.16", "—", "removed"],
    ],
  );
  // `^0.17.16` never yields 0.27 and `^1.0.1` never yields 3.0: cargo
  // re-resolved both, so the move goes with a requirement edit.
  assert.deepEqual(
    lockfile.needsRequirement.map((row) => [row.crate, row.from, row.to]),
    [
      ["addr2line", "0.25.1", "0.27.1"],
      ["daachorse", "1.0.1", "3.0.3"],
    ],
  );
  assert.deepEqual(lockfile.added, [{ name: "bit-set", version: "0.11.1" }]);
});

// MARK: npm

/** A fake npm: `outdated` answers with versions, the audit with `auditJson`. */
const fakeNpm = (auditJson, outdatedJson = "{}") => (_command, args) => ({
  // node_modules exists in this checkout, so npmSection does not stop early.
  stdout: args[0] === "audit" ? auditJson : outdatedJson,
  stderr: "",
  status: args[0] === "audit" ? 1 : 0,
});

const CLEAN_AUDIT = JSON.stringify({
  vulnerabilities: {},
  metadata: { vulnerabilities: { info: 0, low: 0, moderate: 0, high: 0, critical: 0, total: 0 } },
});
const FOUND_AUDIT = JSON.stringify({
  vulnerabilities: {
    glob: { name: "glob", severity: "moderate", isDirect: false, fixAvailable: true, via: [{ title: "Glob: AbortSignal" }] },
    linkify: { name: "linkify", severity: "info", isDirect: true, fixAvailable: true, via: [] },
  },
});

test("an npm audit that errored is 'audit failed', never clean (finding 4)", () => {
  // The shapes npm answers when it could not audit: its own error object, a
  // registry answer in place of JSON, and no output at all.
  for (const output of ['{\n  "error": {\n    "summary": "cb() never called!",\n    "detail": ""\n  }\n}', "<html>503</html>", ""]) {
    assert.throws(() => parseAudit(output, "npm ERR! network", 1), /cb\(\) never called|no JSON/);
  }
  // A JSON report without a vulnerabilities table is no clean result either.
  assert.throws(() => parseAudit('{"metadata":{"vulnerabilities":{"total":0}}}', "", 0), /no vulnerabilities table/);
  // A real audit: its findings, severities ordered, info left out.
  assert.deepEqual(
    parseAudit(FOUND_AUDIT, "", 1).map((row) => [row.name, row.severity]),
    [["glob", "moderate"]],
  );
  assert.deepEqual(parseAudit(CLEAN_AUDIT, "", 0), []);
});

test("npmSection names a failed shipped audit as an error and a failed dev audit as a note", async () => {
  // The message starts with the audit that failed, so the report says what broke.
  await assert.rejects(npmSection(fakeNpm('{"error":{"summary":"audio"}}')), (error) => error.message === "npm audit --omit=dev: audio");
  await assert.rejects(npmSection(fakeNpm("not json")), (error) => error.message.startsWith("npm audit --omit=dev: no JSON"));
  // The same failure on the dev tree (the second call) is listed, and the
  // shipped result stands.
  let call = 0;
  const section = await npmSection((command, args) => {
    if (args[0] === "audit") return { stdout: call++ === 0 ? CLEAN_AUDIT : '{"error":{"summary":"EBUSY, scandir"}}', stderr: "", status: 1 };
    return { stdout: "{}", stderr: "", status: 0 };
  });
  assert.deepEqual(section.audit, []);
  assert.match(section.notes[0], /npm audit \(all, dev tree included\) did not run: EBUSY, scandir/);
  assert.ok(renderReport({ errors: [], npm: section, rust: null, toolchain: null }).includes("npm audit (all, dev tree included) did not run"));
});

test("npm outdated marks majors and the tree they are used from", async () => {
  const section = await npmSection(
    fakeNpm(
      CLEAN_AUDIT,
      JSON.stringify({
        "framer-motion": { current: "13.1.1", wanted: "13.5.1", latest: "14.9.0" },
        vite: { current: "7.1.0", wanted: "7.1.2", latest: "7.1.2" },
      }),
    ),
  );
  assert.deepEqual(
    section.outdated.map((row) => [row.name, row.major, row.usedBy]),
    [
      ["framer-motion", true, "prod"],
      ["vite", false, "dev"],
    ],
  );
});

// MARK: the report and its exit code (finding 1)

test("a failed part leaves the report written, names what failed, and still fails the run", async () => {
  // cargo: a dry run that failed; npm: a security finding; rust-lang.org: down.
  const runCommand = (command, args) => {
    if (command === "cargo") return { stdout: "", stderr: "error: failed to load source for dependency `x`", status: 101 };
    return fakeNpm(FOUND_AUDIT)(command, args);
  };
  const fetchImpl = async () => ({ status: 503, text: async () => "" });
  const { data, report } = await buildReport({ runCommand, fetchImpl, now: new Date("2026-11-06T05:00:00Z") });
  // The report exists although two parts failed, and says which.
  assert.match(report, /### Rust: lockfile[\s\S]*Not collected \(the cargo dry run did not finish\)/);
  assert.match(report, /### Rust toolchain[\s\S]*Not collected/);
  assert.match(report, /\*\*Could not check\*\*/);
  for (const error of data.errors) assert.ok(report.includes(`- ${error}`), error);
  assert.ok(data.errors.some((error) => error === "cargo update --workspace --dry-run --verbose: error: failed to load source for dependency `x`"));
  assert.ok(data.errors.some((error) => error === "static.rust-lang.org (Rust stable channel): HTTP 503"), data.errors.join("\n"));
  // It is a complete npm report, and the run fails.
  assert.match(report, /1 npm security finding\(s\) in shipped code/);
  assert.equal(exitCode(data), 1);
});

test("could not check: a failed request is listed by the run (finding 5)", async () => {
  const data = await collect({
    runCommand: fakeNpm(CLEAN_AUDIT),
    fetchImpl: async (url) => {
      throw new Error(`connect ECONNREFUSED ${url}`);
    },
  });
  assert.equal(data.toolchain, null);
  assert.equal(data.errors.length, 1);
  assert.match(data.errors[0], /^static\.rust-lang\.org \(Rust stable channel\): connect ECONNREFUSED/);
  // The failed part is listed in the report, not hidden.
  const report = renderReport(data, new Date("2026-11-06T05:00:00Z"));
  assert.match(report, /^- static\.rust-lang\.org \(Rust stable channel\): connect ECONNREFUSED/m);
  assert.equal(exitCode(data), 1);
});

test("a clean report passes, and the rendered report keeps the sections", () => {
  const lockfile = lockfileSections(parseCargoReport(DRY_RUN));
  const data = {
    errors: [],
    branch: "main",
    runUrl: null,
    npm: { audit: [], devAudit: [], outdated: [{ name: "vite", current: "7.1.0", wanted: "7.1.2", latest: "7.1.2", major: false, usedBy: "dev" }], notes: [] },
    rust: { lockfile },
    toolchain: { pinned: "1.98.1", stable: "1.98.1", behind: false },
  };
  const report = renderReport(data, new Date("2026-11-06T05:00:00Z"));
  for (const heading of ["### Summary", "**Could not check**", "### npm security", "### npm outdated", "### Rust: lockfile", "### Rust toolchain"]) {
    assert.ok(report.includes(heading), heading);
  }
  assert.match(report, /- nothing\n/);
  assert.match(report, /\| daachorse \| 1\.0\.1 \| 3\.0\.3 \|/);
  assert.match(report, /\| ordered-float \| 5\.5\.0 \| 5\.4\.0 \| downgrade/);
  assert.ok(!/\n\n\n/.test(report), "no triple newline");
  assert.equal(exitCode(data), 0);
  // ordered-float's downgrade and png's removal are moves inside the
  // requirements; daachorse and addr2line were re-resolved, not counted there.
  assert.equal(summaryItems(data).filter((item) => item.startsWith("2 Rust lockfile change")).length, 1);
  assert.ok(summaryItems(data).some((item) => item.startsWith("2 lockfile change(s) a requirement")));
});

// MARK: the issue body under GitHub's limit

/** A rendered report with `rows` dev-audit rows (each names a long advisory). */
function bigReport(rows, tableOptions = {}) {
  return renderReport(
    {
      errors: [],
      npm: {
        audit: [],
        devAudit: Array.from({ length: rows }, (_, index) => ({
          name: `package-${String(index).padStart(4, "0")}`,
          severity: "moderate",
          kind: "transitive",
          advisories: "A long advisory title that fills the row to about 90 characters",
          fix: "available",
        })),
        outdated: [],
        notes: [],
      },
      rust: { lockfile: lockfileSections(parseCargoReport(DRY_RUN)) },
      toolchain: { pinned: "1.98.1", stable: "1.99.0", behind: true },
    },
    new Date("2026-11-06T05:00:00Z"),
    tableOptions,
  );
}

test("a body over GitHub's 65,536-character limit is cut, names the cut rows, and stays Markdown", () => {
  const report = bigReport(900);
  const history = parseHistory(issueBody({ report: bigReport(700), status: "an older run", at: new Date("2026-10-30T05:00:00Z") }));
  const body = issueBody({ report, status: "1 npm package behind", at: new Date("2026-11-06T05:00:00Z"), history });
  const fitted = fitIssueBody(body, MAX_ISSUE_BODY);
  assert.ok(fitted.body.length <= MAX_ISSUE_BODY, `${fitted.body.length} characters`);
  // Just under the limit the first step (the earlier run's rows) is enough.
  const slight = fitIssueBody(body, body.length - 1000);
  assert.ok(slight.body.length <= body.length - 1000, `${slight.body.length} characters`);
  assert.deepEqual(slight.notes, ["earlier runs' tables cut to their headers"]);
  assert.ok(slight.body.includes("| _… 101 more rows_ |"), "the earlier run's table says how many rows went (the table holds 100)");
  // The current report is whole, and its 100-row table says how many it hides.
  assert.equal((slight.body.match(/^\| package-\d\d\d\d \|/gm) ?? []).length, 100);
  assert.ok(slight.body.includes("| _… 800 more rows_ |"), "the table names the rows it does not list");
  // Header and divider lines are untouched, so the tables stay tables.
  assert.match(slight.body, /^\| Package \| Severity \| Where \| Advisory \| Fix \|$/m);
  assert.match(parseHistory(slight.body)[0].report, /Rust toolchain 1\.98\.1 behind stable 1\.99\.0/);
  // Far over (a test limit): the earlier runs go entirely and the tail is cut.
  const hard = fitIssueBody(body, 8000);
  assert.ok(hard.body.length <= 8000, `${hard.body.length} characters`);
  assert.ok(!hard.body.includes("## Earlier runs"));
  assert.ok(hard.body.includes("Cut to fit the issue's 8000-character limit"));
});

test("a body that fits is left exactly as it is", () => {
  const body = issueBody({ report: bigReport(3), status: "nothing behind", at: new Date("2026-11-06T05:00:00Z") });
  assert.deepEqual(fitIssueBody(body), { body, notes: [] });
});

test("table() keeps at most maxRows and names the rest", () => {
  const rows = Array.from({ length: 106 }, (_, index) => [`crate-${index}`, "1.0.0", "1.0.1"]);
  const lines = table(["Crate", "Locked", "New"], rows);
  assert.equal(lines.filter((line) => /^\| crate-/.test(line)).length, 100);
  assert.deepEqual(lines.filter((line) => line.includes("more rows")), ["| _… 6 more rows_ |"]);
});
