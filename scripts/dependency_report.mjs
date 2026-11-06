#!/usr/bin/env node
// Weekly dependency report (the `report` job of .github/workflows/advisories.yml).
//
// Collects what is behind and writes one Markdown report. It never changes a
// dependency: every update goes through scripts/ship.sh with its second-model
// review, so the report is input for the coordinator's task list.
//
//   node scripts/dependency_report.mjs [--out FILE]
//   node scripts/dependency_report.mjs --out FILE --issue-body PREVIOUS_BODY_FILE
//
// Sections: npm security (a finding in shipped code fails the run), npm
// outdated (current/wanted/latest, majors marked), the Rust lockfile and the
// Rust toolchain. Without --issue-body the report alone is written; with it,
// the issue body (status, report, earlier runs of the previous body), kept
// under GitHub's 65,536-character limit.
//
// Exit 0: report written (a dev-only npm finding does not fail). Exit 1: a
// security finding, an `npm audit` of shipped code that did not run, or a part
// that could not be checked: the report is still written and names it under
// "Could not check".

import { spawnSync } from "node:child_process";
import { existsSync, readFileSync } from "node:fs";
import { readFile, writeFile } from "node:fs/promises";
import path from "node:path";
import { fileURLToPath, pathToFileURL } from "node:url";
import { parseArgs } from "node:util";

const ROOT = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "..");
const STABLE_CHANNEL = "https://static.rust-lang.org/dist/channel-rust-stable.toml";
const USER_AGENT = "rawtonic-dependency-report (+https://github.com/rawtonic-app/rawtonic)";
export const MAX_ISSUE_BODY = 65536;
const MAX_EARLIER_RUNS = 8;
const MAX_TABLE_ROWS = 100;
const SEVERITY_ORDER = ["info", "low", "moderate", "high", "critical"];

/** A check could not look at its source: the report says so and the run fails. */
export class CheckError extends Error {}

// MARK: versions

/** 1.2.3-alpha.1+build; build metadata ignored from here on. */
export function parseVersion(version) {
  const text = String(version ?? "")
    .trim()
    .replace(/^v/, "");
  const [core, ...pre] = text.split("+")[0].split("-");
  const numbers = core.split(".").map((part) => (/^\d+$/.test(part) ? Number(part) : 0));
  return { numbers, pre: pre.length ? pre.join("-") : null };
}

/** Semver order: 1.2.3 < 1.2.4, 0.8.0-alpha < 0.8.0, "1.1.4+spec" == "1.1.4". */
export function compareVersions(a, b) {
  const pa = parseVersion(a);
  const pb = parseVersion(b);
  for (let i = 0; i < Math.max(pa.numbers.length, pb.numbers.length); i++) {
    const x = pa.numbers[i] ?? 0;
    const y = pb.numbers[i] ?? 0;
    if (x !== y) return x < y ? -1 : 1;
  }
  if (pa.pre === pb.pre) return 0;
  if (pa.pre === null) return 1;
  if (pb.pre === null) return -1;
  const xs = pa.pre.split(".");
  const ys = pb.pre.split(".");
  for (let i = 0; i < Math.max(xs.length, ys.length); i++) {
    if (xs[i] === undefined) return -1;
    if (ys[i] === undefined) return 1;
    const nx = /^\d+$/.test(xs[i]);
    const ny = /^\d+$/.test(ys[i]);
    if (nx && ny && Number(xs[i]) !== Number(ys[i])) return Number(xs[i]) < Number(ys[i]) ? -1 : 1;
    if (nx !== ny) return nx ? -1 : 1;
    if (xs[i] !== ys[i]) return xs[i] < ys[i] ? -1 : 1;
  }
  return 0;
}

/** A major change; for 0.x crates the minor counts as the major (semver). */
export function isMajorChange(from, to) {
  const a = parseVersion(from).numbers;
  const b = parseVersion(to).numbers;
  if ((a[0] ?? 0) !== (b[0] ?? 0)) return true;
  return (a[0] ?? 0) === 0 && (a[1] ?? 0) !== (b[1] ?? 0);
}

/** The `channel` of rust-toolchain.toml (the file has one [toolchain] table). */
export function parseToolchainChannel(text) {
  const match = /^\s*channel\s*=\s*"([^"]+)"/m.exec(String(text));
  return match ? match[1] : null;
}

// MARK: cargo output

/**
 * A change line of a dry run:
 *   `    Updating addr2line v0.25.1 -> v0.27.1`
 *   `      Adding bit-set v0.11.1`
 *   `    Removing png v0.17.16`
 *   ` Downgrading ordered-float v5.5.0 -> v5.4.0 (available: v5.5.1)`
 * After the arrow comes nothing or `(available: vX)`, a note only on a move
 * down; a line without a version is no change (an `Updating` of an index).
 */
export function parseChange(line) {
  const tokens = line.trim().split(/\s+/);
  const [kind, name, from, ...after] = tokens;
  if (!["Adding", "Removing", "Updating", "Downgrading"].includes(kind) || !name || !from || !from.startsWith("v")) return null;
  const arrow = after[0] === "->";
  const to = arrow ? (after[1] ?? "") : "";
  const note = arrow ? after.slice(2) : after;
  return {
    kind,
    name,
    from: from.slice(1),
    to: to.replace(/^v/, ""),
    available: /^\(available: v(\S+)\)$/.exec(note.join(" "))?.[1] ?? "",
  };
}

/**
 * `cargo update --dry-run` as cargo prints it (also with `--verbose`). Colors
 * are stripped; a `--verbose` list line reads
 * `Unchanged NAME vOURS (available: vNEWER)`. An `error:` or `warning:` line
 * is kept for the report (the dry run's own "not updating lockfile" line is not).
 */
export function parseCargoReport(text) {
  const report = { changes: [], behind: {}, rustVersion: null, msrv: null, errors: [] };
  for (const raw of String(text ?? "").split("\n")) {
    const trimmed = raw.replace(/\x1b\[[0-9;]*m/g, "").trim();
    const locked = /^Locking [0-9]+ packages? to latest Rust ([\d.]+) compatible versions/.exec(trimmed);
    if (locked) report.rustVersion = locked[1];
    // An old cargo prints the MSRV its resolver used with `-vv`.
    const msrv = /get_rust_version: Some\(([\d.]+)\)/.exec(trimmed);
    if (msrv) report.msrv = msrv[1];
    if ((trimmed.startsWith("error: ") || trimmed.startsWith("warning: ")) && !trimmed.startsWith("warning: not updating lockfile")) report.errors.push(trimmed);
    const unchanged = /^Unchanged (\S+) v(\S+) \(available: v(\S+)\)$/.exec(trimmed);
    if (unchanged) report.behind[unchanged[1]] = { ours: unchanged[2], available: unchanged[3] };
    const change = parseChange(trimmed);
    if (change) report.changes.push(change);
  }
  return report;
}

/** A change as the lockfile table's row. */
export function updateRow(change) {
  if (change.kind === "Adding") return { crate: change.name, from: "—", to: change.from, kind: "added" };
  if (change.kind === "Removing") return { crate: change.name, from: change.from, to: "—", kind: "removed" };
  return { crate: change.name, from: change.from, to: change.to, kind: change.available ? "downgrade" : "updated" };
}

/**
 * The requirement line a release sits on: 0.17.16 is in `^0.17.16` (0.17),
 * 1.2.3 in `^1.2` (1).
 */
export function releaseLine(version) {
  const [major, minor] = parseVersion(version).numbers;
  return `${major}.${major === 0 ? minor : 0}`;
}

/**
 * The lockfile section's rows from the workspace dry run:
 *  - `takes`: moves inside the existing requirements (a plain `cargo update`).
 *  - `needsRequirement`: moves past a requirement (`^0.17.16` never yields
 *    0.27, `^1.0.1` never yields 3.0): Cargo.toml was re-resolved for them —
 *    a `[patch.crates-io]` copy, a feature, a dependency's own range.
 *  - `added`: crates the run adds to the lockfile.
 */
export function lockfileSections(report) {
  const rows = report.changes.filter((change) => change.kind !== "Adding").map(updateRow);
  const sameRequirement = (row) =>
    row.kind !== "updated" ||
    // ^0.17.16 yields only 0.17.*; ^1.2 yields 1.*; both print their major.
    releaseLine(row.from) === releaseLine(row.to);
  return {
    takes: rows.filter(sameRequirement),
    needsRequirement: rows.filter((row) => !sameRequirement(row)),
    added: report.changes.filter((change) => change.kind === "Adding").map((change) => ({ name: change.name, version: change.from })),
  };
}

// MARK: network

const sleep = (milliseconds) => new Promise((resolve) => setTimeout(resolve, milliseconds));

/** fetch with retries; a failed request throws a CheckError, never an empty answer. */
export function createRequester(fetchImpl = globalThis.fetch) {
  const cache = new Map();
  async function get(url, headers, { retries = 3 } = {}) {
    const cached = cache.get(url);
    if (cached instanceof CheckError) throw cached;
    if (cached !== undefined) return cached;
    let failure;
    for (let attempt = 0; attempt <= retries; attempt++) {
      if (attempt) await sleep(1000 * attempt);
      let response;
      try {
        response = await fetchImpl(url, { headers, redirect: "follow" });
      } catch (error) {
        failure = new CheckError(`${url}: ${error.message}`);
        continue;
      }
      // 404 was seen under load on a busy host; 429 and 5xx are limits and outages.
      if ((response.status === 404 || response.status === 429 || response.status >= 500) && attempt < retries) continue;
      if (response.status !== 200) failure = new CheckError(`${url}: HTTP ${response.status}`);
      else {
        cache.set(url, await response.text());
        return cache.get(url);
      }
    }
    cache.set(url, failure);
    throw failure;
  }
  return { get };
}

/** The latest stable Rust release, from the stable channel manifest. */
export async function latestStableRust(requester) {
  const text = await requester.get(STABLE_CHANNEL, { "User-Agent": USER_AGENT, Accept: "text/plain" }).catch((error) => {
    // "fetch failed" names nothing: the cause does. The report names the
    // source instead of repeating the URL in a list item.
    const cause = error.cause;
    const reason = cause && !error.message.includes(String(cause.message)) ? `${error.message}: ${cause.message}` : error.message;
    throw new CheckError(reason.split(STABLE_CHANNEL).join("static.rust-lang.org (Rust stable channel)"));
  });
  const match = /^\[pkg\.rust\]\s*$[\s\S]*?^version\s*=\s*"([^"]+)"/m.exec(text);
  if (!match) throw new CheckError("static.rust-lang.org (Rust stable channel): no [pkg.rust] version");
  return match[1].split(/\s+/)[0];
}

// MARK: local inputs

function lastLine(text) {
  return String(text ?? "")
    .trim()
    .split("\n")
    .pop();
}

function run(command, args, options = {}) {
  const result = spawnSync(command, args, { cwd: ROOT, encoding: "utf8", maxBuffer: 64 * 1024 * 1024, ...options });
  if (result.error) throw new CheckError(`${command} ${args.join(" ")}: ${result.error.message}`);
  return result;
}

/** The dry run, which leaves the lockfile alone; its output is parsed. */
export function cargoDryRun(runCommand = run) {
  const args = ["update", "--workspace", "--dry-run", "--verbose"];
  const result = runCommand("cargo", args, { env: { ...process.env, CARGO_TERM_COLOR: "never" } });
  if (result.status !== 0) throw new CheckError(`cargo ${args.join(" ")}: ${lastLine(result.stderr)}`);
  return parseCargoReport(`${result.stdout}\n${result.stderr}`);
}

/**
 * An `npm audit --json` result: its findings, or a CheckError when the audit
 * did not run. A non-JSON answer, an `{"error": …}` body (npm could not reach
 * the registry, or reported a problem) and a report without a
 * `vulnerabilities` table are failures — never a clean result.
 */
export function parseAudit(stdout, stderr, status) {
  let report;
  try {
    report = JSON.parse(stdout);
  } catch {
    throw new CheckError(`no JSON in its output (${lastLine(stderr) || `exit ${status}`})`);
  }
  if (report?.error) throw new CheckError(String(report.error.summary ?? report.error.message ?? JSON.stringify(report.error)));
  if (typeof report?.vulnerabilities !== "object" || report.vulnerabilities === null) {
    throw new CheckError("its JSON has no vulnerabilities table");
  }
  return Object.values(report.vulnerabilities)
    .filter((vulnerability) => vulnerability.severity !== "info")
    .map((vulnerability) => ({
      name: vulnerability.name,
      severity: vulnerability.severity,
      kind: vulnerability.isDirect ? "direct" : "transitive",
      advisories: (vulnerability.via ?? []).map((via) => (typeof via === "string" ? via : via.title)).join("; "),
      fix: vulnerability.fixAvailable === false ? "none" : "available",
    }))
    .sort((a, b) => SEVERITY_ORDER.indexOf(b.severity) - SEVERITY_ORDER.indexOf(a.severity) || a.name.localeCompare(b.name));
}

/**
 * npm security and versions. A shipped-tree audit that did not run is an error
 * (its unknown state is not clean). The dev tree's findings are listed only,
 * so its audit's failure is a note.
 */
export async function npmSection(runCommand = run) {
  const section = { audit: [], devAudit: [], outdated: [], errors: [], notes: [] };
  if (!existsSync(path.join(ROOT, "node_modules"))) {
    section.errors.push("node_modules is missing (npm ci has not run): npm audit and npm outdated were not checked");
    return section;
  }
  // A shell may carry NODE_ENV=production; npm then treats the tree as a
  // production install. Neither audit nor outdated may depend on that.
  const env = { ...process.env };
  delete env.NODE_ENV;
  const audit = (omit) => {
    const result = runCommand("npm", ["audit", "--json", ...(omit ? ["--omit=dev"] : [])], { env });
    try {
      return parseAudit(result.stdout, result.stderr, result.status);
    } catch (error) {
      if (!(error instanceof CheckError)) throw error;
      if (omit) throw new CheckError(`npm audit --omit=dev: ${error.message}`);
      section.notes.push(`npm audit (all, dev tree included) did not run: ${error.message}`);
      return [];
    }
  };
  section.audit = audit(true);
  section.devAudit = audit(false);

  const result = runCommand("npm", ["outdated", "--json"], { env });
  let outdated;
  try {
    outdated = result.stdout.trim() ? JSON.parse(result.stdout) : {};
  } catch {
    throw new CheckError(`npm outdated: ${lastLine(result.stderr) || `exit ${result.status}`}`);
  }
  const packageJson = JSON.parse(await readFile(path.join(ROOT, "package.json"), "utf8"));
  const prod = new Set(Object.keys(packageJson.dependencies ?? {}));
  section.outdated = Object.entries(outdated)
    .filter(([, value]) => value && value.current && value.latest)
    .map(([name, value]) => ({
      name,
      current: value.current,
      wanted: value.wanted ?? "",
      latest: value.latest,
      major: isMajorChange(value.current, value.latest),
      usedBy: prod.has(name) ? "prod" : "dev",
    }))
    .sort((a, b) => Number(b.major) - Number(a.major) || a.name.localeCompare(b.name));
  return section;
}

/** rust-toolchain.toml against the latest stable Rust. */
export async function toolchainSection(requester) {
  const section = { pinned: null, stable: null, behind: false };
  const file = path.join(ROOT, "rust-toolchain.toml");
  if (existsSync(file)) section.pinned = parseToolchainChannel(await readFile(file, "utf8"));
  section.stable = await latestStableRust(requester);
  // "1.98" is today's stable while no 1.99.0 exists; "1.98.1" must match.
  section.behind = Boolean(section.pinned) && compareVersions(section.stable, section.pinned) > 0;
  return section;
}

// MARK: report

function cell(value) {
  return String(value ?? "")
    .replace(/\|/g, "\\|")
    .replace(/\s+/g, " ")
    .trim();
}

/**
 * A Markdown table: header, divider, rows, one `… N more rows` line in place of
 * the rows past `maxRows` (or past `maxChars`, keeping the first two), a blank
 * line. The issue-body fitter shortens tables again, in the issue body.
 */
export function table(header, rows, { maxRows = MAX_TABLE_ROWS, maxChars = Infinity } = {}) {
  const head = [`| ${header.join(" | ")} |`, `|${header.map(() => "---").join("|")}|`];
  const kept = [];
  let used = head.join("\n").length;
  for (const row of rows) {
    const line = `| ${row.map(cell).join(" | ")} |`;
    if (kept.length >= maxRows || (kept.length >= 2 && used + line.length > maxChars)) break;
    kept.push(line);
    used += line.length + 1;
  }
  if (kept.length < rows.length) kept.push(moreRow(header.length, rows.length - kept.length));
  return [...head, ...kept, ""];
}

function severityCounts(rows) {
  const parts = SEVERITY_ORDER.filter((severity) => rows.some((row) => row.severity === severity)).map(
    (severity) => `${rows.filter((row) => row.severity === severity).length} ${severity}`,
  );
  return parts.length ? parts.join(", ") : "none";
}

/** What waits for the coordinator, one clause per finding (also the status line). */
export function summaryItems(data) {
  const items = [];
  const npm = data.npm;
  if (npm) {
    if (npm.audit.length) items.push(`${npm.audit.length} npm security finding(s) in shipped code (${severityCounts(npm.audit)}) — the run fails`);
    if (npm.devAudit.length) items.push(`${npm.devAudit.length} npm dev-dependency finding(s) (${severityCounts(npm.devAudit)})`);
    if (npm.outdated.length) {
      items.push(`${npm.outdated.length} npm package(s) behind, ${npm.outdated.filter((row) => row.major).length} with a major bump`);
    }
    for (const note of npm.notes ?? []) items.push(note);
  }
  const lockfile = data.rust?.lockfile;
  if (lockfile) {
    if (lockfile.takes.length) items.push(`${lockfile.takes.length} Rust lockfile change(s) \`cargo update --workspace\` would take`);
    if (lockfile.needsRequirement.length) items.push(`${lockfile.needsRequirement.length} lockfile change(s) a requirement in Cargo.toml does not allow (a re-resolved dependency)`);
    if (lockfile.added.length) items.push(`${lockfile.added.length} crate(s) a dry run would add to the lockfile`);
  }
  if (data.toolchain?.behind) items.push(`Rust toolchain ${data.toolchain.pinned} behind stable ${data.toolchain.stable}`);
  return items;
}

/** The Markdown report of one run. */
export function renderReport(data, at = new Date(), { maxRows = MAX_TABLE_ROWS } = {}) {
  const { npm, rust, toolchain } = data;
  const out = [];
  const when = at.toISOString().replace(/\.\d+Z$/, "Z");
  out.push(`_Generated ${when}` + (data.branch ? ` on \`${data.branch}\`` : "") + (data.runUrl ? ` ([run](${data.runUrl}))` : "") + "._", "", "### Summary", "");
  const items = summaryItems(data);
  if (!items.length) out.push("Nothing behind: every check reports the newest state.");
  for (const item of items) out.push(`- ${item}`);
  out.push("");
  out.push("**Could not check** (the run fails, and the rest below is what could be collected):", "");
  if (data.errors.length) for (const error of data.errors) out.push(`- ${error}`);
  else out.push("- nothing");
  out.push("");

  out.push("### npm security", "");
  if (npm) {
    out.push(
      `Shipped code (\`npm audit --omit=dev\`): ${severityCounts(npm.audit)}.` + (npm.audit.length ? " **This fails the run.**" : ""),
      "",
      `Dev tree (\`npm audit\`, listed only): ${severityCounts(npm.devAudit)}.`,
      "",
    );
    for (const note of npm.notes ?? []) out.push(`- ${note}`);
    if (npm.notes?.length) out.push("");
    if (npm.audit.length) out.push("Shipped code:", "", ...table(["Package", "Severity", "Where", "Advisory", "Fix"], npm.audit.map((row) => [row.name, row.severity, row.kind, row.advisories, row.fix]), { maxRows }));
    if (npm.devAudit.length) out.push("Dev tree:", "", ...table(["Package", "Severity", "Where", "Advisory", "Fix"], npm.devAudit.map((row) => [row.name, row.severity, row.kind, row.advisories, row.fix]), { maxRows }));
    if (npm.audit.length || npm.devAudit.length) {
      out.push("> npm lists a withdrawn advisory from an older package's own bundled advisory index (eslint's, say), so an old entry can stay after the fix; a package with no fix moves only with its consumer.", "");
    }
  } else out.push("Not collected (npm audit did not run).", "");

  out.push("### npm outdated (current / wanted / latest)", "");
  if (npm?.outdated.length) {
    out.push(
      ...table(
        ["Package", "Current", "Wanted", "Latest", "", "Used by"],
        npm.outdated.map((row) => [row.name, row.current, row.wanted, row.latest, row.major ? "**major**" : "", row.usedBy]),
      ),
      "`wanted` is the newest version the range in `package.json` allows; `latest` beyond it means a major update.", "",
    );
  } else out.push(npm ? "Nothing behind." : "Not collected (npm outdated did not run).", "");

  out.push("### Rust: lockfile", "");
  const lockfile = rust?.lockfile;
  if (lockfile) {
    out.push(
      "`cargo update --workspace --dry-run` (it leaves the lockfile alone): what a plain `cargo update` takes inside the existing requirements —",
      "",
    );
    if (lockfile.takes.length) {
      out.push(...table(["Crate", "Locked", "New", "Note"], lockfile.takes.map((row) => [row.crate, row.from, row.to, row.kind === "removed" ? "removed" : row.kind === "downgrade" ? "downgrade (newer versions yanked?)" : ""])));
    } else out.push("nothing.", "");
    if (lockfile.needsRequirement.length) {
      out.push(
        "It also moves these past their requirement in Cargo.toml — a re-resolved dependency (`[patch.crates-io]` copies, features), so a requirement edit goes with them —",
        "",
        ...table(["Crate", "Locked", "New"], lockfile.needsRequirement.map((row) => [row.crate, row.from, row.to])),
      );
    }
    if (lockfile.added.length) {
      out.push(`It adds ${lockfile.added.length} crate(s): ${listNames(lockfile.added.map((row) => `\`${row.name}\` ${row.version}`))}.`, "");
    }
  } else out.push("Not collected (the cargo dry run did not finish).", "");

  out.push("### Rust toolchain", "");
  if (toolchain) {
    out.push(...table(["Item", "Ours", "Upstream", "Note"], [["`rust-toolchain.toml`", toolchain.pinned, `stable ${toolchain.stable}`, toolchain.behind ? "stable is newer" : ""]]));
  } else out.push("Not collected (`static.rust-lang.org` was not read).", "");
  out.push(
    "Inputs: npm results read `node_modules` from `npm ci` on this lockfile; the Rust rows from `cargo update --workspace --dry-run --verbose` and `rust-toolchain.toml`; the stable release from `static.rust-lang.org`.", "",
  );
  return out.join("\n");
}

function listNames(names) {
  return names.length <= 8 ? names.join(", ") : `${names.slice(0, 8).join(", ")} and ${names.length - 8} more`;
}

/** The issue's status line. */
export function renderStatus(data) {
  const items = summaryItems(data);
  return items.length ? items.join("; ") : "nothing behind";
}

/** Fold earlier runs from an existing issue body. */
export function parseHistory(body) {
  const markdown = String(body ?? "");
  const history = [];
  const dateLine = /^Last run (\d\d\d\d-\d\d-\d\dT\d\d:\d\d:\d\dZ)(?: \(\[run\]\((\S+?)\)\))?/m.exec(markdown);
  const statusLine = /^\*\*Status:\*\* (.*)$/m.exec(markdown);
  const current = /## Current report\n([\s\S]*?)\n---\n/.exec(markdown) ?? /## Current report\n([\s\S]*)$/.exec(markdown);
  if (current && dateLine) {
    history.push({ at: new Date(dateLine[1]), runUrl: dateLine[2] ?? "", status: statusLine ? statusLine[1] : "", report: current[1] });
  }
  const entry = /<summary>(\S+) — (.*?)(?: \(\[run\]\((\S+?)\)\))?<\/summary>\n\n([\s\S]*?)\n\n<\/details>/g;
  let match;
  while ((match = entry.exec(markdown))) {
    history.push({ at: new Date(match[1]), status: match[2], runUrl: match[3] ?? "", report: match[4] });
  }
  return history.filter((run) => !Number.isNaN(run.at.getTime())).sort((a, b) => b.at - a.at);
}

/** The issue body: the current report, with the earlier runs folded below it. */
export function issueBody({ report, status, at, runUrl, history = [] }) {
  const when = at.toISOString().replace(/\.\d+Z$/, "Z");
  const out = [
    "<!-- dependency-report -->",
    "Maintained by the `report` job of [advisories.yml](.github/workflows/advisories.yml):",
    "weekly on Mondays and on demand (**Actions → Dependency advisories → Run workflow**).",
    "Nothing in it updates a dependency; the coordinator turns the items into tasks.",
    '"Could not check" above the report means that part was not read, and the run fails.',
    "",
    `**Status:** ${status}`,
    "",
    `Last run ${when}` + (runUrl ? ` ([run](${runUrl}))` : "") + ".",
    "",
    "---",
    "",
    "## Current report",
    "",
    report.trim(),
    "",
    "---",
    "",
  ];
  const earlier = history.filter((run) => run.report.trim()).slice(0, MAX_EARLIER_RUNS);
  if (earlier.length) {
    out.push("## Earlier runs", "");
    for (const run of earlier) {
      out.push(
        "<details>",
        `<summary>${run.at.toISOString().replace(/\.\d+Z$/, "Z")} — ${cell(run.status || "no status")}${run.runUrl ? ` ([run](${run.runUrl}))` : ""}</summary>`,
        "",
        run.report.trim(),
        "",
        "</details>",
        "",
      );
    }
  }
  return out.join("\n");
}

/**
 * Keep the issue body under GitHub's 65,536-character limit. The earlier runs
 * go first (the current report is what the issue is for): each loses its rows,
 * back to its table headers; then the oldest runs drop. If the current report
 * alone is too long, the longest table keeps its first rows and says
 * `… N more rows`; a body still over is cut. Returns what was cut, for the log.
 */
export function fitIssueBody(body, limit = MAX_ISSUE_BODY) {
  const notes = [];
  let text = String(body);
  const over = () => text.length - limit;
  /** A table block without its rows: header, divider, one `… N more rows` line. */
  const withoutRows = (block) => {
    const [head, divider, ...rows] = block.trimEnd().split("\n");
    return `${head}\n${divider}\n${moreRow((head.match(/\|/g) ?? []).length - 1, rows.length)}`;
  };
  // The earlier runs go first: their rows, then the runs themselves.
  const earlier = text.indexOf("## Earlier runs");
  if (earlier >= 0 && over() > 0) {
    const before = text.slice(0, earlier);
    const cut = text.slice(earlier).replace(/^\| [^\n]* \|\n\|[-|]+\|\n(?:^\| [^\n]* \|\n)+/gm, withoutRows);
    if (cut !== text.slice(earlier)) notes.push("earlier runs' tables cut to their headers");
    text = before + cut;
  }
  if (over() > 0 && text.includes("## Earlier runs")) {
    text = `${text.replace(/^## Earlier runs\n[\s\S]*$/, "").trimEnd()}\n`;
    notes.push("earlier runs dropped");
  }
  // The current report: the longest table keeps its first rows.
  if (over() > 0) {
    const kept = Math.max(1, Math.floor(limit / 160));
    const cut = text.replace(/^\| [^\n]* \|\n\|[-|]+\|\n(?:^\| [^\n]* \|\n)+/gm, (block) => {
      if (over() <= 0) return block;
      const [head, divider, ...rows] = block.trimEnd().split("\n");
      if (rows.length <= kept + 1) return block;
      return `${head}\n${divider}\n${rows.slice(0, kept).join("\n")}\n${moreRow((head.match(/\|/g) ?? []).length - 1, rows.length - kept)}`;
    });
    if (cut !== text) notes.push("the report's longest table cut");
    text = cut;
  }
  if (over() > 0) {
    text = `${text.slice(0, Math.max(0, limit - 160)).trimEnd()}\n\n_Cut to fit the issue's ${limit}-character limit; the run has the full report._`;
    notes.push("the tail cut");
  }
  return { body: text, notes };
}

/** The one line a shortened table keeps: how many rows it does not list. */
function moreRow(columns, count) {
  return `|${` _… ${count} more rows_ |`.padEnd(columns * 2 + 1, " ")}`;
}

// MARK: main

/**
 * The report's data. Each part that fails is named in `errors` — the report
 * says `could not check: …` for it — and the run still writes what it has.
 */
export async function collect({ fetchImpl = globalThis.fetch, runCommand = run } = {}) {
  const data = { errors: [], branch: process.env.GITHUB_REF_NAME ?? null, runUrl: null, npm: null, rust: null, toolchain: null };
  const { GITHUB_SERVER_URL, GITHUB_REPOSITORY, GITHUB_RUN_ID } = process.env;
  if (GITHUB_SERVER_URL && GITHUB_REPOSITORY && GITHUB_RUN_ID) data.runUrl = `${GITHUB_SERVER_URL}/${GITHUB_REPOSITORY}/actions/runs/${GITHUB_RUN_ID}`;
  const requester = createRequester(fetchImpl);
  const check = async (part, work) => {
    try {
      await work();
    } catch (error) {
      data[part] = null;
      data.errors.push(error instanceof CheckError ? error.message : `${part}: ${error.stack ?? error}`);
    }
  };
  await check("npm", async () => {
    data.npm = await npmSection(runCommand);
    data.errors.push(...data.npm.errors);
  });
  await check("rust", async () => {
    const workspace = cargoDryRun(runCommand);
    // The dry run's non-fatal lines: a resolver warning is news for the person
    // reading the report, never silence.
    data.rust = { lockfile: lockfileSections(workspace) };
    data.errors.push(...workspace.errors.map((line) => `cargo update: ${line}`));
  });
  await check("toolchain", async () => {
    data.toolchain = await toolchainSection(requester);
  });
  return data;
}

/** Collect and render; the options replace fetch, the command runner and the clock (tests). */
export async function buildReport(options = {}) {
  const data = await collect(options);
  const at = options.now ?? new Date();
  return { data, at, report: renderReport(data, at), status: renderStatus(data) };
}

/** 1 when the run must fail: a finding in shipped code, or a part that could not be checked. */
export function exitCode(data) {
  return data.errors.length || data.npm?.audit.length ? 1 : 0;
}

async function main() {
  const { values } = parseArgs({
    options: {
      out: { type: "string", default: process.env.DEPENDENCY_REPORT_FILE ?? "dependency-report.md" },
      "issue-body": { type: "string" },
    },
  });
  const { data, at, report, status } = await buildReport();
  let text = report;
  if (values["issue-body"]) {
    const previous = await readFile(path.resolve(values["issue-body"]), "utf8").catch(() => "");
    text = issueBody({ report, status, at, runUrl: data.runUrl, history: parseHistory(previous) });
    const fitted = fitIssueBody(text);
    text = fitted.body;
    for (const note of fitted.notes) console.error(`dependency report: issue body over ${MAX_ISSUE_BODY} characters: ${note}`);
  }
  const out = path.resolve(values.out);
  await writeFile(out, text.endsWith("\n") ? text : `${text}\n`);
  console.log(`dependency report: ${out} (${text.length} characters)`);
  console.log(`status: ${status}`);
  for (const error of data.errors) console.error(`error: could not check: ${error}`);
  if (data.npm?.audit.length) console.error("error: npm audit found a security problem in shipped code; the run fails");
  if (exitCode(data)) process.exit(1);
}

if (process.argv[1] && pathToFileURL(path.resolve(process.argv[1])).href === import.meta.url) {
  main().catch((error) => {
    console.error(`dependency report: ${error.stack ?? error}`);
    process.exit(1);
  });
}
