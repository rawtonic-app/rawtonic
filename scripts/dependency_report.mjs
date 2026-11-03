#!/usr/bin/env node
// Weekly dependency report (the `report` job of .github/workflows/advisories.yml).
//
// Collects what is behind upstream and writes one Markdown report. It never
// changes a dependency: no lockfile update, no push, no pull request. Every
// merge goes through scripts/ship.sh with its second-model review, so this
// report is input for the coordinator's task list.
//
//   node scripts/dependency_report.mjs [--out FILE]
//   node scripts/dependency_report.mjs --out FILE --issue-body PREVIOUS_BODY_FILE
//
// Sections: npm audit of shipped code (fails the run on a finding), npm audit
// of the dev tree (listed only), npm outdated (current/wanted/latest, majors
// marked), Rust: what `cargo update --workspace` would take inside the
// existing requirements, direct crates.io dependencies behind their newest
// version or newest stable Rust, the vendored third_party/rawler against the
// upstream fork and crates.io, and rust-toolchain.toml against the latest
// stable. Without --issue-body the report alone is written; with it, the issue
// body (status, report, earlier runs from the previous body).
//
// Why not cargo-outdated (measured 2026-11-03 on the Linux PC): cargo-outdated
// 0.19.0 builds on the pinned 1.98.1 toolchain, but it copies only the
// workspace members into its scratch directory and then fails on our
// `[patch.crates-io]` copies in vendor/ ("failed to read
// /tmp/cargo-outdated…/vendor/glib-macros/Cargo.toml"), with no option for the
// scratch location. Direct dependencies are read from `cargo metadata` and the
// crates.io sparse index instead (no API token; one requester that remembers
// index paths so a crate renamed on crates.io is asked only once).
//
// Exit 0: report written (a dev-only npm finding does not fail). Exit 1: a
// security finding in shipped code, or parts could not be collected.

import { spawnSync } from "node:child_process";
import { readdirSync, readFileSync } from "node:fs";
import { access, readFile, writeFile } from "node:fs/promises";
import path from "node:path";
import { fileURLToPath, pathToFileURL } from "node:url";
import { parseArgs } from "node:util";

const ROOT = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "..");
const CRATES_IO_INDEX = "https://index.crates.io";
const CRATES_IO_SOURCE = "registry+https://github.com/rust-lang/crates.io-index";
const USER_AGENT = "rawtonic-dependency-report (+https://github.com/rawtonic-app/rawtonic)";
const STABLE_CHANNEL = "https://static.rust-lang.org/dist/channel-rust-stable.toml";
const RAW_HEADERS = { "User-Agent": USER_AGENT, Accept: "text/plain" };
// The vendored copy comes from this fork; its README frontmatter names the
// release. dnglab/dnglab (where the code started) is asked when the fork has
// none.
const RAWLER_FORK = "CyberTimon/RapidRAW-DngLab";
const RAWLER_UPSTREAM = "dnglab/dnglab";
const RAWLER_DIR = "third_party/rawler";

// An issue body over 64 KiB is rejected by the GitHub API; keep tables and
// history well below that.
const MAX_TABLE_ROWS = 80;
const MAX_EARLIER_RUNS = 8;
const MAX_HISTORY_REPORT_CHARS = 12000;
const SEVERITY_ORDER = ["info", "low", "moderate", "high", "critical"];

/** A check could not look at its source: the section says so and the run fails. */
export class CheckError extends Error {}

// MARK: versions

export function parseVersion(version) {
  const text = String(version ?? "")
    .trim()
    .replace(/^v/, "");
  // Build metadata first: "1.1.4+spec-1.1.0" is 1.1.4, not 1.1.4-spec.
  const [core, ...pre] = text.split("+")[0].split("-");
  const numbers = core.split(".").map((part) => (/^\d+$/.test(part) ? Number(part) : 0));
  return { numbers, pre: pre.length ? pre.join("-") : null };
}

/** 1.2.3 < 1.2.4, 0.8.0-alpha < 0.8.0, build metadata ignored. */
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
    if (nx && ny) {
      if (Number(xs[i]) !== Number(ys[i])) return Number(xs[i]) < Number(ys[i]) ? -1 : 1;
    } else if (nx !== ny) {
      return nx ? -1 : 1;
    } else if (xs[i] !== ys[i]) {
      return xs[i] < ys[i] ? -1 : 1;
    }
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

function parseDotted(version) {
  return String(version)
    .trim()
    .replace(/^[\^~=<>]+/, "")
    .replace(/^v/, "")
    .split(/[+-]/)[0]
    .split(".")
    .map((part) => (/^\d+$/.test(part) ? Number(part) : NaN));
}

/**
 * The requirement forms of package.json (`1.2.3`, `^1.2.3`, `~1.2.3`, `>1.2.3`,
 * `>=1.2.3`) through the same logic as cargo's requirements.
 */
export function satisfiesRange(version, range) {
  return satisfiesCargo(version, range);
}

/**
 * `cargo update --dry-run` output. The dry run leaves the lockfile alone.
 *   Locking 3 packages to latest Rust 1.88 compatible versions
 *        Updating serde v1.0.100 -> v1.0.101
 *   Downgrading foo v2.0.0 -> v1.9.0
 */
export function parseCargoUpdates(text) {
  const updates = [];
  let rustVersion = null;
  for (const line of String(text).split("\n")) {
    const locked = /^Locking (\d+) packages? to latest Rust ([\d.]+) compatible versions/.exec(line.trim());
    if (locked) rustVersion = locked[2];
    const match = /^\s*(Updating|Downgrading|Upgrading)\s+(\S+) v(\S+) -> (?:v(\S+)|\[(\d+)\])$/.exec(line);
    if (match) updates.push({ crate: match[2], from: match[3], to: match[4] ?? `[${match[5]}]` });
  }
  return { updates, rustVersion };
}

/**
 * The sparse index path of a crate: `a` -> `1/a`, `ab` -> `2/ab`,
 * `abc` -> `3/a/abc` (three-letter names are NOT `ab/c/abc`), `serde` ->
 * `se/rd/serde`.
 */
export function sparseIndexPath(name) {
  const lower = String(name).toLowerCase();
  if (lower.length === 1) return `1/${lower}`;
  if (lower.length === 2) return `2/${lower}`;
  if (lower.length === 3) return `3/${lower[0]}/${lower}`;
  return `${lower.slice(0, 2)}/${lower.slice(2, 4)}/${lower}`;
}

/**
 * The index rows of a crates.io index file (a broken row is skipped).
 * `cargo update --verbose` reports "Unchanged aes v0.9.2 (available: v0.9.4)"
 * with build metadata ("+spec-1.1.0") stripped, so the same crates arrive as
 * `0.9.2+spec-1.1.0`: accept an equal core version as equal.
 */
export function indexEntries(indexLines) {
  const entries = [];
  for (const line of indexLines) {
    let entry;
    try {
      entry = JSON.parse(line);
    } catch {
      continue;
    }
    if (entry && !entry.yanked && typeof entry.vers === "string") entries.push(entry.vers);
  }
  return entries;
}

/** Index rows built from versions (tests use this instead of the network). */
export function indexLinesFromVersions(name, versions) {
  return versions.map((vers) => JSON.stringify({ name, vers, yanked: false }));
}

function sameCore(a, b) {
  return String(a).split("+")[0] === String(b).split("+")[0];
}

/** The highest non-yanked release that is not a prerelease. */
export function newestRelease(indexLines) {
  let newest = null;
  for (const version of indexEntries(indexLines)) {
    if (parseVersion(version).pre) continue;
    if (newest === null || compareVersions(version, newest) > 0) newest = version;
  }
  return newest;
}

/**
 * The release a locked one may move to without changing a manifest: the
 * highest patch of its own version (or, for a prerelease, of its own
 * minor). Returns "" when nothing newer is published.
 */
export function newestInRange(indexLines, fromVersion) {
  const current = parseVersion(fromVersion);
  const [major, minor] = current.numbers;
  let newest = null;
  for (const version of indexEntries(indexLines)) {
    const candidate = parseVersion(version);
    // A locked prerelease moves to prereleases only; every other release to
    // releases only.
    if (Boolean(candidate.pre) !== Boolean(current.pre)) continue;
    const sameMajor = candidate.numbers[0] === (major ?? 0);
    const compatible = current.pre
      ? sameMajor && candidate.numbers[1] === (minor ?? 0)
      : sameMajor && ((major ?? 0) !== 0 || candidate.numbers[1] === (minor ?? 0));
    if (!compatible) continue;
    if (newest === null || compareVersions(version, newest) > 0) newest = version;
  }
  if (newest === null || sameCore(newest, fromVersion) || compareVersions(newest, fromVersion) <= 0) return "";
  return newest;
}

/**
 * A crate sitting on a prerelease: is the next release the first stable of
 * that line (0.42.0-rc.1 -> 0.42.0)? Then `cargo update` takes it only with a
 * requirement that allows the prerelease, and the next stable is the news.
 */
export function prereleaseAhead(indexLines, fromVersion) {
  const current = parseVersion(fromVersion);
  if (!current.pre) return "";
  let next = null;
  for (const version of indexEntries(indexLines)) {
    const candidate = parseVersion(version);
    if (candidate.pre) continue;
    const sameLine = candidate.numbers[0] === current.numbers[0] && candidate.numbers[1] === current.numbers[1];
    if (!sameLine) continue;
    if (next === null || compareVersions(version, next) < 0) next = version;
  }
  return next ?? "";
}

// MARK: fetching

const sleep = (milliseconds) => new Promise((resolve) => setTimeout(resolve, milliseconds));

/**
 * fetch with retries, a cache, and "the index does not know this name".
 * A 404 on the sparse index means the name was never published (crates.io has
 * kept every name); a crate renamed on crates.io answers 304 with no body.
 * Both read as unknown here and are reported as a note.
 */
function createRequester(fetchImpl = globalThis.fetch) {
  const cache = new Map();
  async function get(url, headers, { retries = 4 } = {}) {
    if (cache.has(url)) return cache.get(url);
    let result = { status: 0, text: "" };
    for (let attempt = 0; attempt <= retries; attempt++) {
      let response;
      try {
        response = await fetchImpl(url, { headers, redirect: "follow" });
      } catch (error) {
        if (attempt === retries) throw new CheckError(`${url}: ${error.message}`);
        await sleep(1500 * (attempt + 1));
        continue;
      }
      // 404 answers were seen under load on a busy host and never with a
      // repeated request; 429 and 5xx are rate limits and outages.
      const retryable = response.status === 429 || response.status >= 500 || response.status === 404;
      if (retryable && attempt < retries) {
        await sleep(1500 * (attempt + 1));
        continue;
      }
      result = { status: response.status, text: response.ok ? await response.text() : "" };
      break;
    }
    cache.set(url, result);
    return result;
  }
  return { get };
}

/** The lines of a crate's sparse index file; empty when the name is unknown. */
export async function cratesIoIndex(name, requester) {
  const response = await requester.get(`${CRATES_IO_INDEX}/${sparseIndexPath(name)}`, RAW_HEADERS);
  if (response.status === 200) return response.text ? response.text.split("\n").filter(Boolean) : [];
  if (response.status === 304 || response.status === 404) return [];
  throw new CheckError(`${CRATES_IO_INDEX}: HTTP ${response.status}`);
}

/** The newest published release of a GitHub repository, from its README frontmatter. */
export async function githubReadmeVersion(repo, requester) {
  for (const branch of ["main", "master"]) {
    const response = await requester.get(`https://raw.githubusercontent.com/${repo}/${branch}/README.md`, RAW_HEADERS);
    if (!response.text) continue;
    const lines = response.text.split("\n");
    if (lines[0]?.trim() !== "---") return null;
    const end = lines.indexOf("---", 1);
    if (end < 0) return null;
    for (const line of lines.slice(1, end)) {
      const match = /^\s*version:\s*["']?([^"'\s]+)["']?\s*$/.exec(line);
      if (match) return match[1];
    }
    return null;
  }
  return null;
}

/** The latest stable Rust release, from the stable channel manifest. */
export async function latestStableRust(requester) {
  const response = await requester.get(STABLE_CHANNEL, RAW_HEADERS);
  if (!response.text) throw new CheckError(`${STABLE_CHANNEL}: HTTP ${response.status || "unreachable"}`);
  const match = /^\[pkg\.rust\]\s*$[\s\S]*?^version\s*=\s*"([^"]+)"/m.exec(response.text);
  if (!match) throw new CheckError(`${STABLE_CHANNEL}: no [pkg.rust] version`);
  return match[1].split(/\s+/)[0];
}

/**
 * The index cache cargo fills on the machine (`$CARGO_HOME/registry/index/`)
 * parsed into crate → versions. Sparse-index builds keep one file per crate:
 * a header, then (length, crc32, JSON row) records. The lightbyte and git
 * index formats do not look like that and read as no rows.
 */
function readCargoIndexCache(root) {
  const crates = new Map();
  if (!root) return crates;
  const stack = [root];
  while (stack.length) {
    const dir = stack.pop();
    let entries;
    try {
      entries = readdirSync(dir, { withFileTypes: true });
    } catch {
      continue;
    }
    for (const entry of entries) {
      const file = path.join(dir, entry.name);
      if (entry.isDirectory() && entry.name !== ".cache") stack.push(file);
      else if (entry.isFile()) addCachedCrate(crates, entry.name, file);
    }
  }
  return crates;
}

function addCachedCrate(crates, name, file) {
  let data;
  try {
    data = readFileSync(file);
  } catch {
    return;
  }
  const rows = [];
  try {
    let offset = 4;
    const format = data.readUInt32LE(offset);
    offset += 4;
    if (format !== 1) return;
    const etagEnd = data.indexOf(0, offset);
    if (etagEnd < 0) return;
    offset = etagEnd + 1;
    while (offset + 8 <= data.length) {
      const length = data.readUInt32LE(offset);
      offset += 8;
      if (length === 0 || offset + length > data.length) break;
      rows.push(data.subarray(offset, offset + length).toString("utf8"));
      offset += length;
    }
  } catch {
    return;
  }
  // The file name is the crate name; the rows must agree (a stray file).
  const parsed = rows.filter((row) => row.includes(`"name":"${name}"`) || row.includes(`"name": "${name}"`));
  if (parsed.length) crates.set(name, parsed);
}

/** `$CARGO_HOME`, else `~/.cargo`. */
function cargoHome(env = process.env) {
  if (env.CARGO_HOME) return env.CARGO_HOME;
  return env.HOME ? path.join(env.HOME, ".cargo") : null;
}

// MARK: local inputs

function run(command, args, options = {}) {
  const result = spawnSync(command, args, { cwd: ROOT, encoding: "utf8", maxBuffer: 64 * 1024 * 1024, ...options });
  if (result.error) throw new CheckError(`${command} ${args.join(" ")}: ${result.error.message}`);
  return result;
}

function lastLine(text) {
  return String(text ?? "")
    .trim()
    .split("\n")
    .pop();
}

async function exists(file) {
  try {
    await access(file);
    return true;
  } catch {
    return false;
  }
}

/** The `channel` of rust-toolchain.toml (the file has one [toolchain] table). */
export function parseToolchain(text) {
  const match = /^\s*channel\s*=\s*"([^"]+)"/m.exec(String(text));
  return match ? match[1] : null;
}

/** The `version` of a crate manifest (the [package] table's version). */
export function parseManifestVersion(text) {
  const packageTable = /^\[package\]\s*$([\s\S]*?)(?=^\[|$(?![\s\S]))/m.exec(String(text));
  const match = /^\s*version\s*=\s*"([^"]+)"/m.exec(packageTable ? packageTable[1] : String(text));
  return match ? match[1] : null;
}

/**
 * The crates.io dependencies the workspace members declare themselves
 * (normal, build and dev kinds — all are ours to update), as name → [req].
 * The vendored third_party/rawler is a workspace member only because of its
 * `[patch.crates-io]`; its dependencies move with a rawler update, so they are
 * left out (the rawler section covers rawler itself). `cargo metadata
 * --no-deps` reads manifests only: no build, no index.
 */
export function directCrates(metadata) {
  const crates = new Map();
  for (const pkg of metadata.workspace_members ?? []) {
    const member = (metadata.packages ?? []).find((candidate) => candidate.id === pkg);
    if (!member || member.name === "rawler") continue;
    for (const dep of member.dependencies ?? []) {
      if (dep.source !== CRATES_IO_SOURCE) continue;
      if (!crates.has(dep.name)) crates.set(dep.name, []);
      crates.get(dep.name).push(dep.req);
    }
  }
  return [...crates.keys()].sort().map((name) => ({ name, reqs: crates.get(name) }));
}

/**
 * The newest release the manifest requirement allows (what `cargo update`
 * takes with the requirement as it is). Supports the forms cargo reports for
 * a single `version`: `^1.2`, `~1.2`, `=1.2.3-rc.1`, `>=1.2`, `1.2`. Returns
 * "" when nothing newer is published.
 */
export function newestAllowed(indexLines, req, fromVersion) {
  const requirements = Array.isArray(req) ? req : [req];
  const allowed = indexEntries(indexLines).filter(
    (version) =>
      requirements.some((requirement) => satisfiesCargo(version, requirement)) &&
      (fromVersion ? compareVersions(version, fromVersion) > 0 : true),
  );
  allowed.sort(compareVersions);
  return allowed.at(-1) ?? "";
}

function satisfiesCargo(version, requirement) {
  const text = String(requirement ?? "").trim();
  // The operator first: `|` is a literal in a regex, so `>=` has to be tried
  // before `>` (and "=2.0.0-rc.12" otherwise reads as ">=" with target
  // "2.0.0-rc.12", which a prerelease is not above).
  const operatorMatch = /^(\^|~|>=|<=|>|<|=)?\s*(.+)$/.exec(text);
  if (!operatorMatch) return false;
  const operator = operatorMatch[1] ?? "^";
  const target = operatorMatch[2].split(",")[0].trim();
  const current = parseVersion(version);
  const base = parseVersion(target);
  // Only a requirement written with a prerelease takes prereleases and the
  // stable release of its line; anything else stays on releases.
  const prereleasesAllowed = Boolean(base.pre);
  if (Boolean(current.pre) && !prereleasesAllowed) return false;
  // "2.0.0" must not read as "2" for prefix tests (a short requirement like
  // "=2" covers the whole 2.x line).
  const widths = [base.numbers.length, ...(base.pre ? [3] : [])];
  const width = Math.max(...widths);
  const prefixMatch = (length) =>
    current.numbers.slice(0, length).every((number, index) => number === (base.numbers[index] ?? 0));
  const compare = compareVersions(version, target);
  // A bare "1.2.3" in package.json is "^1.2.3" for npm; cargo metadata
  // always writes the operator out itself.
  switch (operator) {
    case "^":
    case "": {
      // Compatible with the leftmost non-zero number of the requirement.
      if (base.numbers[0] !== 0) return current.numbers[0] === base.numbers[0] && compare >= 0;
      if (base.numbers[1] !== 0) return prefixMatch(2) && compare >= 0;
      return prefixMatch(width) && compare >= 0;
    }
    case "~": {
      // Patch-level changes (minor-level when the requirement names only the
      // major).
      return prefixMatch(Math.max(width - 1, 1)) && compare >= 0;
    }
    case "=":
      // Cargo reads `=1.2.3` as 1.2.3 exactly, but a requirement written with a
      // prerelease as its line up to that prerelease and the releases after
      // it (`=1.2.3-rc.1` takes rc.5 and 1.2.3). The locked `compare` must not
      // enter here: the release whose range is asked may be any of the line.
      return base.pre
        ? prefixMatch(3) && compareVersions(version, target) >= 0
        : parseVersion(version).pre
          ? false
          : compare === 0;
    case ">":
      return compare > 0;
    case ">=":
      return compare >= 0;
    case "<":
      return compare < 0;
    case "<=":
      return compare <= 0;
    default:
      return false;
  }
}

/** Name/version/source of every [[package]] in Cargo.lock. */
export function parseLockfile(text) {
  const packages = [];
  let current = null;
  for (const raw of String(text).split("\n")) {
    const line = raw.trim();
    if (line === "[[package]]") {
      current = { name: "", version: "", source: "local" };
      packages.push(current);
      continue;
    }
    if (line.startsWith("[")) {
      current = null;
      continue;
    }
    const pair = current && /^(\w+)\s*=\s*"([^"]*)"$/.exec(line);
    if (!pair) continue;
    if (pair[1] === "name") current.name = pair[2];
    else if (pair[1] === "version") current.version = pair[2];
    else if (pair[1] === "source") current.source = pair[2];
  }
  return packages;
}

// MARK: sections

async function npmSection(errors) {
  const section = { audit: [], devAudit: [], outdated: [] };
  if (!(await exists(path.join(ROOT, "node_modules")))) {
    errors.push("node_modules is missing (npm ci has not run): npm audit and npm outdated are not covered");
    return section;
  }
  // A shell on a developer machine may carry NODE_ENV=production; npm then
  // treats an install as a production one. Neither audit nor outdated may
  // depend on the caller's environment.
  const env = { ...process.env };
  delete env.NODE_ENV;

  // npm audit exits 1 when it finds something — that is the normal case; only
  // output that is not JSON means the check did not run.
  const audit = (omit) => {
    const result = run("npm", ["audit", "--json", ...(omit ? ["--omit=dev"] : [])], { env });
    let report;
    try {
      report = JSON.parse(result.stdout);
    } catch {
      throw new CheckError(`npm audit ${omit ? "--omit=dev" : "(all)"}: ${lastLine(result.stderr)}`);
    }
    return Object.values(report.vulnerabilities ?? {})
      .filter((vulnerability) => vulnerability.severity !== "info")
      .map((vulnerability) => ({
        name: vulnerability.name,
        severity: vulnerability.severity,
        kind: vulnerability.isDirect ? "direct" : "transitive",
        advisories: (vulnerability.via ?? []).map((via) => (typeof via === "string" ? via : via.title)).join("; "),
        fix: vulnerability.fixAvailable === false ? "none" : "available",
      }))
      .sort((a, b) => SEVERITY_ORDER.indexOf(b.severity) - SEVERITY_ORDER.indexOf(a.severity) || a.name.localeCompare(b.name));
  };
  section.audit = audit(true);
  section.devAudit = audit(false);

  // npm outdated exits 1 when packages are behind, which is the usual case.
  const outdatedResult = run("npm", ["outdated", "--json"], { env });
  let outdated;
  try {
    outdated = outdatedResult.stdout.trim() ? JSON.parse(outdatedResult.stdout) : {};
  } catch {
    throw new CheckError(`npm outdated: ${lastLine(outdatedResult.stderr)}`);
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
      // The range in package.json was edited behind the install (npm outdated
      // then shows a wanted version the manifest no longer allows).
      stale: !satisfiesRange(
        value.wanted ?? value.latest,
        packageJson.dependencies?.[name] ?? packageJson.devDependencies?.[name] ?? "*",
      ),
    }))
    .sort((a, b) => Number(b.major) - Number(a.major) || a.name.localeCompare(b.name));
  return section;
}

function rustLockfileSection() {
  const section = { updates: [], rustVersion: null, crates: [] };
  const dryRun = run("cargo", ["update", "--workspace", "--dry-run"], {
    env: { ...process.env, CARGO_TERM_COLOR: "never" },
  });
  if (dryRun.status !== 0) throw new CheckError(`cargo update --workspace --dry-run: ${lastLine(dryRun.stderr)}`);
  const parsed = parseCargoUpdates(dryRun.stdout + "\n" + dryRun.stderr);
  section.updates = parsed.updates;
  section.rustVersion = parsed.rustVersion;

  // --no-deps: manifests only, no build, no index access.
  const metadata = run("cargo", ["metadata", "--locked", "--format-version", "1", "--no-deps"]);
  if (metadata.status !== 0) throw new CheckError(`cargo metadata: ${lastLine(metadata.stderr)}`);
  section.crates = directCrates(JSON.parse(metadata.stdout));
  return section;
}

/**
 * Index rows per direct dependency: the local cargo cache first (no request
 * pays for a crate cargo already knows), the sparse index when the cache has
 * the crate no newer, and the network when it has none.
 */
async function indexRowsFor(names, requester, cache) {
  const rows = new Map();
  for (const name of names) {
    const cached = cache.get(name);
    const fetched = await cratesIoIndex(name, requester);
    if (!fetched.length) {
      rows.set(name, cached ?? []);
      continue;
    }
    const newest = (list) => newestRelease(list);
    const fromCache = cached && newest(cached);
    const fromNetwork = newest(fetched);
    rows.set(name, fromCache && (!fromNetwork || compareVersions(fromCache, fromNetwork) >= 0) ? cached : fetched);
  }
  return rows;
}

async function rustDirectSection(section, lockfile, requester, cache = new Map()) {
  const locked = new Map();
  for (const pkg of lockfile) {
    if (!pkg.name || !pkg.version || pkg.source === "local") continue;
    const current = locked.get(pkg.name);
    if (!current || compareVersions(pkg.version, current) > 0) locked.set(pkg.name, pkg.version);
  }
  section.direct = [];
  const rows = await indexRowsFor(section.crates.map((crate) => crate.name), requester, cache);
  for (const { name, reqs } of section.crates) {
    const ours = locked.get(name);
    if (!ours) continue;
    const index = rows.get(name) ?? [];
    // Whether the rows still describe the locked release: a crates.io request
    // can fail and leave cargo's (older) cache for the row, and then both the
    // line and the latest version would describe a different crate state
    // (cargo update --verbose said "behind latest" for such a row). Only a
    // major reading is kept from an incomplete row — by definition it lies
    // above the lock — and the latest cell says so.
    const covers = indexEntries(index).includes(ours);
    const incomplete = index.length > 0 && !covers;
    const latest = covers ? newestRelease(index) : null;
    section.direct.push({
      name,
      req: [...new Set(reqs)].join(", "),
      // An `=` requirement moves only with a manifest edit.
      pinned: reqs.length > 0 && reqs.every((req) => /^\s*=/.test(String(req))),
      ours,
      // The newest the manifest requirement allows: what `cargo update` takes.
      wanted: newestAllowed(index, reqs, ours),
      // The first stable release while we sit on a prerelease.
      prelock: prereleaseAhead(index, ours),
      // The newest release of the requirement's own line (the stable after a
      // prerelease counts; a major upgrade shows in `latest`).
      lineNewest:
        [newestRelease(index), newestInRange(index, ours), prereleaseAhead(index, ours)]
          .filter((version) => version && !isMajorChange(ours, version))
          .sort(compareVersions)
          .at(-1) ?? "",
      latest,
      behind: latest ? compareVersions(latest, ours) > 0 : false,
      major: latest ? isMajorChange(ours, latest) : false,
      // An unknown name is the only reason `latest` is missing (a failed
      // request throws instead): the name moved on crates.io.
      note: latest ? "" : incomplete ? "index rows older than the lock; rerun" : "renamed upstream",
      incomplete,
    });
  }
  section.direct.sort(
    (a, b) => Number(b.major) - Number(a.major) || Number(b.behind) - Number(a.behind) || a.name.localeCompare(b.name),
  );
  return section;
}

async function rawlerSection(requester) {
  const manifest = path.join(ROOT, RAWLER_DIR, "Cargo.toml");
  if (!(await exists(manifest))) throw new CheckError(`${RAWLER_DIR}/Cargo.toml is missing`);
  const section = { local: parseManifestVersion(await readFile(manifest, "utf8")), commit: null, cratesIo: null, fork: null, upstream: null };
  const notes = path.join(ROOT, RAWLER_DIR, "RAWTONIC_PATCHES.md");
  if (await exists(notes)) {
    const match = /commit\s*\n?`([0-9a-f]{7,40})`/i.exec(await readFile(notes, "utf8"));
    section.commit = match ? match[1] : null;
  }
  section.cratesIo = newestRelease(await cratesIoIndex("rawler", requester));
  // Asked anyway: the fork's release record is what a vendoring update follows.
  const [fork, upstream] = await Promise.all([
    githubReadmeVersion(RAWLER_FORK, requester),
    githubReadmeVersion(RAWLER_UPSTREAM, requester),
  ]);
  section.fork = fork;
  section.upstream = upstream;
  const newer = (version) => version && section.local && compareVersions(version, section.local) > 0;
  section.behind = Boolean(newer(section.cratesIo) || newer(fork) || newer(upstream));
  return section;
}

async function toolchainSection(requester) {
  const section = { pinned: null, stable: null, behind: false };
  const file = path.join(ROOT, "rust-toolchain.toml");
  if (await exists(file)) section.pinned = parseToolchain(await readFile(file, "utf8"));
  section.stable = await latestStableRust(requester);
  if (section.pinned && /^\d+\.\d+(\.\d+)?$/.test(section.pinned)) {
    // "1.98" is today's stable while no 1.99.0 exists; "1.98.1" must match.
    const stable = parseVersion(section.stable).numbers;
    const same = section.pinned.split(".").every((part, index) => stable[index] === Number(part));
    section.behind = !same && compareVersions(section.stable, section.pinned) > 0;
  }
  return section;
}

// MARK: report

function cell(value) {
  return String(value ?? "")
    .replace(/\|/g, "\\|")
    .replace(/\s+/g, " ")
    .trim();
}

function table(header, rows) {
  const lines = [`| ${header.join(" | ")} |`, `|${header.map(() => "---").join("|")}|`];
  for (const row of rows.slice(0, MAX_TABLE_ROWS)) lines.push(`| ${row.map(cell).join(" | ")} |`);
  if (rows.length > MAX_TABLE_ROWS) {
    lines.push(`| _… and ${rows.length - MAX_TABLE_ROWS} more (see the logs of the run)_ ${"|".repeat(header.length - 1)}`);
  }
  lines.push("");
  return lines;
}

function severityCounts(rows) {
  const parts = SEVERITY_ORDER.filter((severity) => rows.some((row) => row.severity === severity)).map(
    (severity) => `${rows.filter((row) => row.severity === severity).length} ${severity}`,
  );
  return parts.length ? parts.join(", ") : "none";
}

function marks(row) {
  return row.major ? "**major**" : "";
}

/** Direct dependencies whose manifest requirement allows a newer release. */
function inRange(row) {
  return Boolean(row.wanted);
}

/**
 * Rows of the direct-dependency table: what waits first (a newer release in
 * range, the first stable after a prerelease, a major on crates.io). Once the
 * lockfile section above lists what can move, every other in-line dependency
 * is known current and stays out — unless nothing can move at all, when a list
 * (capped) is better than an empty section. Rows whose index answer was older
 * than the lock are only listed as a major.
 */
function directRows(direct, lockfileHasUpdates) {
  const relevant = (row) => inRange(row) || Boolean(row.prelock) || row.major || Boolean(row.note);
  // Without a lockfile update every in-range release is in the lockfile, so
  // "newest in range" equals ours and the table is only worth its rows of
  // moves that need a manifest edit or reach beyond the range.
  const actionable = (row) => Boolean(row.prelock) || row.major || Boolean(row.note) || (row.pinned && inRange(row));
  const shown = direct.filter((row) =>
    lockfileHasUpdates ? relevant(row) : actionable(row) && !row.incomplete,
  );
  return shown
    .sort(
      (a, b) => Number(relevant(b)) - Number(relevant(a)) || Number(b.major) - Number(a.major) || a.name.localeCompare(b.name),
    )
    .map((row) => {
      const note = row.note
        ? row.note
        : row.prelock && !row.wanted
          ? `next stable ${row.prelock}; the requirement allows the prerelease only`
          : row.pinned && row.wanted
            ? `\`${row.req}\` pinned; a manifest edit`
            : "";
      const wanted = row.wanted && row.prelock ? `${row.wanted} (prerelease)` : row.wanted || "—";
      return [row.name, row.ours, wanted, row.latest ?? "—", marks(row), note];
    });
}

/** directRows for the tests (the renderer keeps it private). */
export const directRowsForTest = (direct, lockfileHasUpdates) => directRows(direct, lockfileHasUpdates);

/** What waits for the coordinator, one clause per finding (also the issue's status line). */
export function summaryItems(data) {
  const items = [];
  const npm = data.npm;
  if (npm) {
    if (npm.audit.length) items.push(`${npm.audit.length} npm security finding(s) in shipped code (${severityCounts(npm.audit)})`);
    if (npm.devAudit.length) items.push(`${npm.devAudit.length} npm dev-dependency finding(s) (${severityCounts(npm.devAudit)})`);
    if (npm.outdated.length) {
      items.push(`${npm.outdated.length} npm package(s) behind, ${npm.outdated.filter((row) => row.major).length} with a major bump`);
    }
  }
  if (data.rust) {
    if (data.rust.updates.length) {
      items.push(`${data.rust.updates.length} Rust lockfile update(s) \`cargo update --workspace\` would take`);
    }
    const direct = data.rust.direct ?? [];
    const updatable = direct.filter(inRange);
    const majors = direct.filter((row) => row.major);
    if (updatable.length) {
      const pinned = updatable.filter((row) => row.pinned);
      items.push(
        `${updatable.length} direct crate(s) can move within their requirement` +
          (pinned.length ? ` (${pinned.length} needs a manifest edit: \`=\` requirement)` : ""),
      );
    }
    if (majors.length) items.push(`${majors.length} direct crate(s) across a major version`);
    const stable = direct.filter((row) => row.prelock && !row.wanted);
    for (const row of stable) {
      items.push(`\`${row.name}\` ${row.prelock} is the next stable release (we lock ${row.ours})`);
    }
  }
  if (data.rawler?.behind) items.push("vendored rawler behind upstream");
  if (data.toolchain?.behind) items.push(`Rust toolchain ${data.toolchain.pinned} behind stable ${data.toolchain.stable}`);
  return items;
}

/** The Markdown report of one run. */
export function renderReport(data, at = new Date()) {
  const { npm, rust, rawler, toolchain } = data;
  const out = [];
  const when = at.toISOString().replace(/\.\d+Z$/, "Z");
  out.push(
    `_Generated ${when}` + (data.branch ? ` on \`${data.branch}\`` : "") + (data.runUrl ? ` ([run](${data.runUrl}))` : "") + "._",
    "",
    "### Summary",
    "",
  );
  if (!data.errors.length) {
    out.push("Nothing automatic changes a dependency: every update below goes through `scripts/ship.sh` with its second-model review.", "");
  }
  const items = summaryItems(data);
  if (!items.length) out.push("Nothing behind: every check reports the newest state.");
  for (const item of items) out.push(`- ${item}`);
  out.push("");

  if (data.errors.length) {
    out.push("**Not collected** — the run fails and keeps the previous report:", "");
    for (const error of data.errors) out.push(`- ${error}`);
    out.push("");
  }

  out.push("### npm security", "");
  if (npm) {
    out.push(
      `Shipped code (\`npm audit --omit=dev\`): ${severityCounts(npm.audit)}.` +
        (npm.audit.length ? " **This fails the run.**" : ""),
      "",
      `Dev tree (\`npm audit\`, listed only): ${severityCounts(npm.devAudit)}.`,
      "",
    );
    if (npm.audit.length) out.push("Shipped code:", "", ...table(["Package", "Severity", "Where", "Advisory", "Fix"], npm.audit.map((row) => [row.name, row.severity, row.kind, row.advisories, row.fix])));
    if (npm.devAudit.length) out.push("Dev tree:", "", ...table(["Package", "Severity", "Where", "Advisory", "Fix"], npm.devAudit.map((row) => [row.name, row.severity, row.kind, row.advisories, row.fix])));
    if (npm.audit.length || npm.devAudit.length) {
      out.push("> npm lists a withdrawn advisory inside an older package's own bundled advisory index (for example eslint's), so an old entry can stay visible after the fix; a package with no fix moves only with its consumer.", "");
    }
  } else {
    out.push("Not collected.", "");
  }

  out.push("### npm outdated (current / wanted / latest)", "");
  if (npm?.outdated.length) {
    out.push(
      ...table(
        ["Package", "Current", "Wanted", "Latest", "", "Used by"],
        npm.outdated.map((row) => [
          row.name,
          row.current,
          row.stale ? `${row.wanted} (package.json edited)` : row.wanted,
          row.latest,
          marks(row),
          row.usedBy,
        ]),
      ),
    );
    out.push("`wanted` is the newest version the range in `package.json` allows; `latest` lying beyond it means a major update.", "");
  } else {
    out.push(npm ? "Nothing behind." : "Not collected.", "");
  }

  out.push("### Rust: lockfile updates inside existing requirements", "");
  out.push("`cargo update --workspace --dry-run`: what a plain `cargo update` takes without editing a manifest.", "");
  if (rust) {
    if (rust.updates.length) {
      out.push(...table(["Crate", "Locked", "New"], rust.updates.map((row) => [row.crate, row.from, row.to])));
      const forced = rust.updates.filter((row) => row.to.startsWith("["));
      if (forced.length) {
        out.push(`${forced.length} of these pass a major version because the requirement already allows it (\`[N]\`); review them like a manual bump.`, "");
      }
    } else {
      out.push(`Nothing to update${rust.rustVersion ? ` (latest Rust ${rust.rustVersion} compatible versions)` : ""}.`, "");
    }
  } else {
    out.push("Not collected.", "");
  }

  out.push("### Rust: direct crates.io dependencies", "");
  out.push(
    "Per direct dependency of a workspace member, from `cargo metadata` and the crates.io sparse index (no API token): the newest version in its range (what `cargo update` may take) and the newest published version.",
    "",
  );
  if (rust?.direct?.length) {
    const rows = directRows(rust.direct, Boolean(rust.updates.length));
    out.push(...table(["Crate", "Ours", "Newest in range", "Newest", "", "Note"], rows));
    if (rows.some((row) => row[5] === "renamed upstream")) {
      out.push(
        "`renamed upstream`: the crates.io name answers 304/404 (the crate moved); the requirement here points at the new name.",
        "",
      );
    }
    if (!rust.updates.length) {
      out.push(
        "No lockfile update is possible now: every in-range release is in the lockfile, so the table lists only what needs a manifest edit or a new requirement (the next in-range release shows up in the section above).",
        "",
      );
    }
  } else {
    out.push(rust ? "No direct crates.io dependencies found." : "Not collected.", "");
  }

  out.push("### Vendored rawler and Rust toolchain", "");
  if (rawler || toolchain) {
    const rawlerUpstream = [
      rawler?.fork ? `fork ${rawler.fork}` : null,
      rawler?.upstream ? `dnglab ${rawler.upstream}` : null,
      rawler?.cratesIo ? `crates.io ${rawler.cratesIo}` : null,
    ].filter(Boolean);
    out.push(
      ...table(
        ["Item", "Ours", "Upstream", "Note"],
        [
          [
            `\`${RAWLER_DIR}\``,
            rawler?.local ?? "not collected",
            rawlerUpstream.length ? rawlerUpstream.join(", ") : rawler ? "none found" : "not collected",
            rawler?.commit ? `vendored from ${RAWLER_FORK} @ ${rawler.commit.slice(0, 12)}` : "",
          ],
          [
            "`rust-toolchain.toml`",
            toolchain?.pinned ?? "not collected",
            toolchain?.stable ? `stable ${toolchain.stable}` : "not collected",
            toolchain?.behind ? "stable is newer" : "",
          ],
        ],
      ),
    );
    out.push(
      "A vendoring update follows the fork's release and carries its rawler patch list into `RAWTONIC_PATCHES.md`; crates.io publishes the fork slower than the fork's own repository, so the fork's README is the reference.",
      "",
    );
  }

  out.push(
    "Inputs: the npm results read `node_modules` from `npm ci` on this lockfile; the Rust lockfile section runs `cargo update --dry-run`, the direct-dependency rows come from `cargo metadata` and the crates.io sparse index on `index.crates.io` (rows cargo already cached are reused), the rawler and toolchain rows from `raw.githubusercontent.com`, `api.github.com` and `static.rust-lang.org`.",
    "",
  );
  return out.join("\n");
}

export function renderStatus(data) {
  const items = summaryItems(data);
  return items.length ? items.join("; ") : "nothing behind";
}

/** Fold earlier runs from an existing issue body. */
export function parseHistory(body) {
  const markdown = String(body ?? "");
  const history = [];
  // Until the end of the line: an earlier attempt read two ISO dates ("2026-10-27
  // T05:17:00Z") as one number and folded runs from 2001.
  const dateLine = /^Last run (\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2}Z)(?: \(\[run\]\((\S+?)\)\))?/m.exec(markdown);
  const statusLine = /^\*\*Status:\*\* (.*)$/m.exec(markdown);
  const current = /## Current report\n([\s\S]*?)\n---\n/.exec(markdown) ?? /## Current report\n([\s\S]*)$/.exec(markdown);
  if (current && dateLine) {
    history.push({
      at: new Date(dateLine[1]),
      runUrl: dateLine[2] ?? "",
      status: statusLine ? statusLine[1] : "",
      report: current[1],
    });
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
    "A red run means the report could not be collected and the previous one stands.",
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
    "---",
    "",
  ];
  const earlier = history.filter((run) => run.report.trim()).slice(0, MAX_EARLIER_RUNS);
  if (earlier.length) {
    out.push("## Earlier runs", "");
    for (const run of earlier) {
      const text = run.report.trim();
      const clipped =
        text.length > MAX_HISTORY_REPORT_CHARS
          ? `${text.slice(0, MAX_HISTORY_REPORT_CHARS).trimEnd()}\n\n_Report clipped; the full text is in the run._`
          : text;
      out.push(
        "<details>",
        `<summary>${run.at.toISOString().replace(/\.\d+Z$/, "Z")} — ${cell(run.status || "no status")}${run.runUrl ? ` ([run](${run.runUrl}))` : ""}</summary>`,
        "",
        clipped,
        "",
        "</details>",
        "",
      );
    }
  }
  return out.join("\n");
}

// MARK: main

async function collect(fetchImpl) {
  const requester = createRequester(fetchImpl);
  const data = { errors: [], branch: process.env.GITHUB_REF_NAME ?? null, runUrl: null };
  const { GITHUB_SERVER_URL, GITHUB_REPOSITORY, GITHUB_RUN_ID } = process.env;
  if (GITHUB_SERVER_URL && GITHUB_REPOSITORY && GITHUB_RUN_ID) {
    data.runUrl = `${GITHUB_SERVER_URL}/${GITHUB_REPOSITORY}/actions/runs/${GITHUB_RUN_ID}`;
  }
  const guard = async (key, work) => {
    try {
      data[key] = await work();
    } catch (error) {
      if (!(error instanceof CheckError)) throw error;
      data.errors.push(error.message);
      data[key] = null;
    }
  };
  await guard("npm", () => npmSection(data.errors));
  const lockfile = parseLockfile(await readFile(path.join(ROOT, "Cargo.lock"), "utf8"));
  await guard("rust", async () => {
    const section = rustLockfileSection();
    const cache = readCargoIndexCache(cargoHome() ? path.join(cargoHome(), "registry", "index") : null);
    return rustDirectSection(section, lockfile, requester, cache);
  });
  await guard("rawler", () => rawlerSection(requester));
  await guard("toolchain", () => toolchainSection(requester));
  return data;
}

/** Collect and render; `options.fetch` replaces fetch (tests). */
export async function buildReport(options = {}) {
  const data = await collect(options.fetch);
  const at = options.now ?? new Date();
  return { data, at, report: renderReport(data, at), status: renderStatus(data) };
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
  }
  const out = path.resolve(values.out);
  await writeFile(out, text.endsWith("\n") ? text : `${text}\n`);
  console.log(`dependency report: ${out}`);
  console.log(`status: ${status}`);
  for (const error of data.errors) console.error(`error: ${error}`);
  if (data.errors.length) process.exit(1);
  if (data.npm?.audit.length) {
    console.error("dependency report: npm audit found a security problem in shipped code; the run fails");
    process.exit(1);
  }
}

if (process.argv[1] && pathToFileURL(path.resolve(process.argv[1])).href === import.meta.url) {
  main().catch((error) => {
    console.error(`dependency report: ${error.stack ?? error}`);
    process.exit(1);
  });
}
