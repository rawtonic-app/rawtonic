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
import { readdirSync, readFileSync, statSync } from "node:fs";
import { access, readFile, writeFile } from "node:fs/promises";
import path from "node:path";
import { fileURLToPath, pathToFileURL } from "node:url";
import { parseArgs } from "node:util";

const ROOT = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "..");
const CRATES_IO_INDEX = "https://index.crates.io";
const CRATES_IO_SOURCE = "registry+https://github.com/rust-lang/crates.io-index";
// The crates.io sparse index as a source id: cargo's index cache directory
// (`$CARGO_HOME/registry/index/index.crates.io-<hash>/`) keeps only its files.
const SPARSE_INDEX_SOURCE = "sparse+https://index.crates.io/";
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
const SPAN_ENTRY = "(span)";

/** A check could not look at its source: the section says so and the run fails. */
export class CheckError extends Error {}

/** The crates.io index has no such name (HTTP 404). */
export class IndexNotFoundError extends CheckError {}

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
  let behind = null;
  for (const line of String(text).split("\n")) {
    const locked = /^Locking (\d+) packages? to latest Rust ([\d.]+) compatible versions/.exec(line.trim());
    if (locked) rustVersion = locked[2];
    const unchanged = /(\d+) unchanged dependencies? behind latest/.exec(line);
    if (unchanged) behind = Number(unchanged[1]);
    const match = /^\s*(Updating|Downgrading|Upgrading)\s+(\S+) v(\S+) -> (?:v(\S+)|\[(\d+)\])$/.exec(line);
    if (match) updates.push({ crate: match[2], from: match[3], to: match[4] ?? `[${match[5]}]` });
  }
  return { updates, rustVersion, behind };
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
 * The index rows of a crates.io index file (a broken row is skipped), newest
 * last. `cargo update --verbose` reports "Unchanged aes v0.9.2 (available:
 * v0.9.4)" with build metadata ("+spec-1.1.0") stripped, so the same crates
 * arrive as `0.9.2+spec-1.1.0`: accept an equal core version as equal.
 */
export function indexRows(indexLines) {
  const rows = [];
  for (const line of indexLines) {
    let entry;
    if (typeof line === "object" && line !== null) entry = line;
    else {
      try {
        entry = JSON.parse(line);
      } catch {
        continue;
      }
    }
    if (entry && !entry.yanked && typeof entry.vers === "string") rows.push(entry);
  }
  return rows;
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

/** The `version` of a crate manifest (the [package] table's version). */
export function parseManifestVersion(text) {
  const packageTable = /^\[package\]\s*$([\s\S]*?)(?=^\[|$(?![\s\S]))/m.exec(String(text));
  const match = /^\s*version\s*=\s*"([^"]+)"/m.exec(packageTable ? packageTable[1] : String(text));
  return match ? match[1] : null;
}

/** The `channel` of rust-toolchain.toml (the file has one [toolchain] table). */
export function parseToolchain(text) {
  const match = /^\s*channel\s*=\s*"([^"]+)"/m.exec(String(text));
  return match ? match[1] : null;
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

/** The versions of the index rows (unreadable rows skipped, yanked left out). */
export function indexEntries(indexLines) {
  return indexRows(indexLines).map((entry) => entry.vers);
}

/**
 * Index rows built from versions (tests use this instead of the network).
 * A version with `rustVersion`/`publishedAt` carries those fields.
 */
export function indexLinesFromVersions(name, versions) {
  return versions.map((entry) => {
    const { vers, rustVersion, publishedAt, yanked = false } = typeof entry === "string" ? { vers: entry } : entry;
    return JSON.stringify({
      name,
      vers,
      deps: [],
      ...(rustVersion ? { rust_version: rustVersion } : {}),
      ...(publishedAt ? { pubtime: publishedAt } : {}),
      yanked,
    });
  });
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
 * minor). Returns "" when nothing newer is published. A requirement pinned to
 * a prerelease with `=` also allows the stable release of that line
 * (`=2.0.0-rc.12` takes 2.0.0), so there the stable line counts as in range.
 */
export function newestInRange(indexLines, fromVersion, stableCounts = false) {
  const current = parseVersion(fromVersion);
  const [major, minor] = current.numbers;
  let newest = null;
  for (const version of indexEntries(indexLines)) {
    const candidate = parseVersion(version);
    if (Boolean(candidate.pre) !== Boolean(current.pre) && !(stableCounts && !candidate.pre)) continue;
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

/** Parse "1.92" / "1.92.0" into numbers; null when it is not a version. */
export function parseRustVersion(text) {
  const numbers = parseDotted(text);
  return numbers.length && numbers.every((number) => Number.isFinite(number)) ? numbers : null;
}

/**
 * The newest release the manifest requirement allows (what `cargo update`
 * takes with the requirement as it is). Supports the forms cargo reports for
 * a single `version`: `^1.2`, `~1.2`, `=1.2.3-rc.1`, `>=1.2`, `1.2`. Returns
 * "" when nothing newer is published.
 */
export function newestAllowed(indexLines, req, fromVersion) {
  const requirements = Array.isArray(req) ? req : [req];
  const allowed = indexRows(indexLines).filter(
    (row) =>
      // A span's member is the release the range resolves to (it is in the
      // lockfile, or `cargo update` takes it).
      row.vers !== SPAN_ENTRY &&
      !row.member &&
      requirements.some((requirement) => satisfiesCargo(row.vers, requirement)) &&
      (fromVersion ? compareVersions(row.vers, fromVersion) > 0 : true),
  );
  allowed.sort((a, b) => compareVersions(a.vers, b.vers));
  return allowed.at(-1)?.vers ?? "";
}

/**
 * Why a release in the manifest's range is not in the lockfile.
 *
 * A release above cargo's resolved Rust version is above every member's
 * rust-version (cargo resolves for the highest member MSRV) and would turn a
 * weekly `cargo update` red, so the resolver leaves it out: held back by its
 * rust-version.
 *
 * Every other reason is a resolver constraint no field names (mostly another
 * dependency's requirement; cargo names it only in the failure log), and the
 * report repeats that verdict only from cargo's own `--verbose` list for this
 * lockfile: the dependency listed as `Unchanged NAME vOURS (available:
 * vNEWER)` with the newest in range — a resolver that leaves a newer in-range
 * release alone did not take it — or listed with a reason line while it holds
 * a span's own release. Without the list (an older cargo) no reason is
 * claimed.
 */
export function heldBackReason(context, release) {
  if (!release || !context) return "";
  const rust = parseRustVersion(release.rustVersion);
  const msrv = parseRustVersion(context.cargoRustVersion);
  if (rust && msrv && compareVersions(rust.join("."), msrv.join(".")) > 0) {
    return `needs Rust ${release.rustVersion}; cargo resolves for Rust ${context.cargoRustVersion}`;
  }
  if (!release.name) return "";
  const behind = context.behind ?? {};
  if (behind.unavailable) return "";
  const entry = behind[release.name];
  if (!entry || !sameCore(entry.available, release.vers)) return "";
  return `cargo leaves it out ("${release.name} v${entry.ours} (available: v${entry.available})" unchanged)`;
}

/** cargo's list is known for this dependency (not an older cargo, not stale). */
function heldVersions(context) {
  return Boolean(context?.behind) && !context.behind.unavailable;
}

/** The in-range release a requirement allows that `cargo update` does not take. */
export function heldBack(context, indexLines, req, fromVersion) {
  const behind = context?.behind ?? {};
  const requirements = Array.isArray(req) ? req : [req];
  const candidates = indexRows(indexLines)
    .filter((row) => row.vers !== SPAN_ENTRY)
    .map(({ name, vers, rust_version: rustVersion, member, covered, evidence, epoch }) => ({ name, vers, rustVersion, member, covered, evidence, epoch }))
    .filter(({ vers }) => requirements.some((requirement) => satisfiesCargo(vers, requirement)))
    .filter(({ vers }) => (fromVersion ? compareVersions(vers, fromVersion) > 0 : true))
    .sort((a, b) => compareVersions(a.vers, b.vers));
  const newest = candidates.at(-1);
  if (!newest) return null;
  // A span resolves to exactly one release: the newest in its range is the
  // member cargo takes, so a release below it is no refusal (cargo never
  // considers it), and a covered row behind the lock is a move the lockfile
  // section lists. Only cargo's own list makes a row above the member a hold.
  if (newest.evidence || newest.covered) return null;
  // The release cargo's own list says it holds for this dependency.
  const entry = behind[newest.name];
  if (entry && sameCore(entry.ours, newest.vers)) {
    // The resolver holds the newest in range: news the lockfile has not
    // seen, not a refusal.
    if (!entry.reason) return null;
    // It moved that release and the lockfile still lacks it (a file change
    // only): cargo could not get it to resolve here.
    return { version: newest.vers, reason: "in range; a dependency could not be updated" };
  }
  const reason = heldBackReason(context, newest);
  return reason ? { version: newest.vers, reason } : null;
}

// MARK: fetching

const sleep = (milliseconds) => new Promise((resolve) => setTimeout(resolve, milliseconds));

/**
 * fetch with retries and a cache. An answer is a status plus the body (a
 * cancelled 304 has none). Nothing here sends a conditional header: the
 * crates.io index is behind a cache that answers 304 to a conditional request
 * it cannot fulfil from its store (seen 2026-11-03 with an unconditional
 * cargo run and If-None-Match, GIT_CONFIG_* overrides), and a 304 is "Not
 * Modified" whatever anyone reads into it.
 */
export function createRequester(fetchImpl = globalThis.fetch) {
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

/**
 * The lines of a crate's sparse index file. A 404 means the index has no such
 * name (IndexNotFoundError); a 304 without a cached body is the status alone
 * (a CheckError) — proof of nothing.
 */
export async function cratesIoIndex(name, requester) {
  const response = await requester.get(`${CRATES_IO_INDEX}/${sparseIndexPath(name)}`, RAW_HEADERS);
  if (response.status === 200) return response.text ? response.text.split("\n").filter(Boolean) : [];
  if (response.status === 404) throw new IndexNotFoundError(`${name}: HTTP 404 on the crates.io index`);
  if (response.status === 304) return null;
  throw new CheckError(`${CRATES_IO_INDEX}/${sparseIndexPath(name)}: HTTP ${response.status}`);
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

/** `$CARGO_HOME`, else `~/.cargo`. */
function cargoHome(env = process.env) {
  if (env.CARGO_HOME) return env.CARGO_HOME;
  return env.HOME ? path.join(env.HOME, ".cargo") : null;
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

/** The highest `rust-version` among the workspace members (cargo's resolver MSRV). */
export function highestRustVersion(metadata) {
  const members = new Set(metadata.workspace_members ?? []);
  let highest = null;
  for (const pkg of metadata.packages ?? []) {
    if (members.size && !members.has(pkg.id)) continue;
    const version = parseRustVersion(pkg.rust_version ?? pkg.rustVersion);
    if (!version) continue;
    if (!highest || compareVersions(version.join("."), highest.join(".")) > 0) highest = version;
  }
  return highest ? highest.join(".") : null;
}

/**
 * The index cache cargo fills on the machine (`$CARGO_HOME/registry/index/`)
 * parsed into crate → index rows, and the crates whose file carries a span
 * entry (the resolver's record of a range it resolved; the receiving side of
 * a download never writes one). A sparse-index build keeps one file per
 * crate: a 4-byte version, the cache key (`etag: "…"`), then per version the
 * version string and the JSON row (a span entry's row names no crate). The
 * git index format keeps other layouts and is not read.
 */
export function readCargoIndexCache(root) {
  const crates = new Map();
  const spanned = new Set();
  if (!root) return { crates, spanned };
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
      if (entry.isDirectory()) stack.push(file);
      else if (entry.isFile()) {
        const parsed = cachedCrateRows(readFileSync(file, null));
        if (!parsed) continue;
        // The rows name the crate (a version row does; a span entry may not);
        // file names follow the index path, not the crate name, so the rows
        // decide.
        if (!parsed.name) continue;
        crates.set(parsed.name, parsed.rows);
        if (parsed.span) spanned.add(parsed.name);
      }
    }
  }
  return { crates, spanned };
}

function cachedCrateRows(data) {
  try {
    if (data.readUInt32LE(0) !== 4) return null;
    let offset = 4;
    // The cache key is a NUL-terminated string (`etag: "…"`).
    const etagEnd = data.indexOf(0, offset);
    if (etagEnd < 0) return null;
    offset = etagEnd + 1;
    const rows = [];
    let name = null;
    // Per version: the version string and its JSON row, each NUL-terminated.
    // The row after the versions is the span entry of the crate; without a
    // span the file was filled by the receiving (unresolving) side.
    let span = false;
    while (offset < data.length) {
      const versEnd = data.indexOf(0, offset);
      if (versEnd < 0) return null;
      const vers = data.subarray(offset, versEnd).toString("utf8");
      offset = versEnd + 1;
      const rowEnd = data.indexOf(0, offset);
      if (rowEnd < 0) return null;
      const row = data.subarray(offset, rowEnd).toString("utf8");
      offset = rowEnd + 1;
      // Only JSON rows are releases; a span entry's row may name none.
      if (!row.startsWith("{")) continue;
      try {
        const entry = JSON.parse(row);
        name ??= entry.name ?? null;
        if (entry.vers === SPAN_ENTRY) span = true;
      } catch {
        // A row the parser cannot read is no release.
      }
      rows.push(row);
    }
    return { name, rows, span };
  } catch {
    return null;
  }
}

/**
 * Cargo skips the releases inside a span the lockfile already covers (a
 * semver-compatible range): only the newest release in the range is a
 * candidate, and the rest are releases it did not take. A span entry in the
 * cached index file therefore says two things: every row covered by the span
 * is `covered` (in the requirement's range; cargo's own list then says
 * whether it holds a newer one back), and the span's member — the release
 * cargo resolved the range to — is `member`. The member is the release cargo
 * lists as its own ("Unchanged NAME vX") when the lockfile takes one, else
 * the newest stable row on the span's line; a row that ships as a new crate
 * (`features.force`) is no member (npm takes it, cargo does not).
 *
 * Without a span entry (no dry run, a cache without spans) only the frozen
 * lockfile is known: the report then claims neither releases cargo skipped
 * nor reasons for them.
 */
export function indexRowsWithSpans(indexLines, lockVersions, behind = {}) {
  const rows = indexRows(indexLines).filter((row) => row.vers !== SPAN_ENTRY);
  const spans = indexRows(indexLines)
    .filter((row) => row.vers === SPAN_ENTRY && typeof row.deps === "string" && row.features?.force !== undefined)
    .map((row) => JSON.parse(row.deps));
  const cover = (version) =>
    spans.find(([req, from]) => satisfiesCargo(from, req) && satisfiesCargo(version, req));
  // Without a span the rows are cargo's own; a covered range is news (the
  // file also changes when cargo rewrites it).
  if (!spans.length) return rows.map((row) => ({ ...row, epoch: row.vers }));
  // A span holds exactly one release in its range: the newest one, or the
  // lockfile's release when cargo's list holds it. So either the lockfile is
  // that member (cargo does not move it; the newest in range above it is the
  // next move) or the newest in range is (cargo moves the lockfile up).
  // A stable lockfile release inside the span is the member cargo holds (a
  // prerelease is not: cargo needs `--force` for it); with none, the span's
  // line decides.
  const locked = (lockVersions ?? []).filter((version) => cover(version) && !parseVersion(version).pre);
  const memberOf = (span) => {
    const [major, minor] = parseVersion(span[1]).numbers;
    const onLine = (version) => {
      const candidate = parseVersion(version);
      return !candidate.pre && candidate.numbers[0] === major && candidate.numbers[1] === minor;
    };
    // A release cargo moves is no member (its requirement took it, and the
    // lockfile is behind): the candidate it moves to is the line's newest.
    const moving = behind && !behind.unavailable ? Object.values(behind).some((entry) => entry.reason) : false;
    const onLockedLine = locked.filter(onLine);
    if (onLockedLine.length && !moving) return onLockedLine.sort(compareVersions).at(-1);
    return [rows.map((row) => row.vers).filter(onLine), locked.filter(onLine)].flat().sort(compareVersions).at(-1) ?? "";
  };
  const members = new Map(spans.map((span) => [span, memberOf(span)]));
  return rows.map((row) => {
    const span = cover(row.vers);
    if (!span) return { ...row, epoch: row.vers };
    const member = members.get(span);
    if (member === row.vers && !row.features?.force) return { ...row, member: true, evidence: true, epoch: row.vers };
    return { ...row, covered: true, epoch: row.vers };
  });
}

/**
 * `cargo update --verbose`'s "Unchanged NAME vOURS (available: vNEWER)" lines:
 * cargo's own record of what the resolver left alone, and every other change
 * line as it printed it. A source with neither says `{ unavailable: true }`
 * and no reason is claimed from it.
 */
export function behindList(text) {
  const behind = {};
  for (const line of String(text ?? "").split("\n")) {
    const unchanged = /^\s*Unchanged (\S+) v(\S+) \(available: v(\S+)\)/.exec(line);
    if (unchanged) {
      behind[unchanged[1]] = { ours: unchanged[2], available: unchanged[3], listed: true };
      continue;
    }
    const change = /^\s*(Updating|Adding) (\S+) v(\S+)(?: -> v(\S+))?/.exec(line);
    if (change) behind[change[2]] = { ours: change[3], available: change[4] ?? change[3], reason: line.trim() };
  }
  return Object.keys(behind).length ? behind : { unavailable: true };
}

/**
 * The mtimes of the crates.io index files cargo keeps in `$CARGO_HOME`'s
 * index cache, by crate name. Taken before the first cargo command: a first
 * response arrives at least a round trip later, so a file newer than that
 * start was rewritten during the run (a lockfile update in another worktree,
 * a concurrent cargo) and describes an index other than the answers' one.
 */
export function cratesIoCacheMtimes(root, { mtimeOf = (file) => statSync(file).mtimeMs } = {}) {
  const times = new Map();
  if (!root) return times;
  let indexDirs;
  try {
    indexDirs = readdirSync(root).filter((name) => name.startsWith("index.crates.io-"));
  } catch {
    return times;
  }
  const stack = indexDirs.map((dir) => path.join(root, dir, ".cache"));
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
      if (entry.isDirectory()) stack.push(file);
      else if (entry.isFile()) {
        try {
          times.set(entry.name, mtimeOf(file));
        } catch {
          continue;
        }
      }
    }
  }
  return times;
}

/**
 * Index rows per direct dependency, keyed by name: the crates.io sparse index
 * for the rows, cargo's cache for the span entry (the resolver's decision, in
 * the file cargo filled for this crate). A 304 (Not Modified) reuses the body
 * cargo already has. A name answered only by 404 is `notFound`; a request that
 * failed leaves `indexKnown` false (the row then says the index was not read,
 * never "renamed").
 */
export async function indexRowsFor(names, lockVersions, requester, cache = new Map(), behind = {}, stale = new Set()) {
  const rows = new Map();
  const pending = names.map(async (name) => {
    const locked = lockVersions.get(name);
    const cached = cache.get(name) ?? [];
    const cachedLines = Array.isArray(cached) ? cached : (cached.rows ?? []);
    // A span entry is the resolver's record of a range. Only this run's own
    // `cargo update --dry-run` writes one for the lockfile on disk, into a file
    // that held none before; a file that already carried a span, or one this
    // run rewrote, was written while resolving another lockfile (a CI cache, an
    // earlier run): its spans are skipped and cargo's own list is the evidence.
    const trustSpans = !Array.isArray(cached) && cached.trustSpans !== false && !stale.has(name);
    // A 304 (Not Modified) reuses the body cargo already has; only a request
    // that fails says the index was not read.
    const lines = async () => {
      const fetched = await cratesIoIndex(name, requester);
      if (fetched !== null) return { lines: fetched, from: "crates.io index" };
      const cachedRows = cachedLines.filter((line) => !line.includes(SPAN_ENTRY) && !line.includes("cargo3-index-span"));
      if (!cachedRows.length) {
        throw new CheckError("HTTP 304 (Not Modified) without a cached body");
      }
      return { lines: cachedRows, from: "cached cargo index (HTTP 304 Not Modified)" };
    };
    try {
      const { lines: fetched, from } = await lines();
      const withSpans = trustSpans ? withCachedSpans(fetched, cachedLines) : fetched;
      rows.set(name, {
        rows: indexRowsWithSpans(withSpans, locked, trustSpans ? behind : { unavailable: true }),
        notFound: false,
        indexKnown: true,
        from,
        reused: from !== "crates.io index",
      });
    } catch (error) {
      if (!(error instanceof CheckError)) throw error;
      rows.set(name, {
        rows: [],
        notFound: error instanceof IndexNotFoundError,
        indexKnown: false,
        error: error.message,
      });
    }
  });
  await Promise.all(pending);
  return rows;
}

/** The cached span entry (a resolver decision the index file does not hold). */
function withCachedSpans(lines, cached) {
  return lines.concat(cached.filter((line) => line.includes(SPAN_ENTRY) || line.includes("cargo3-index-span")));
}

/**
 * The crates whose index file cargo rewrote since the run started: the
 * snapshot (taken before the run) differs from the file a repeated `cargo
 * update --workspace --dry-run --offline --verbose` finds now. A span entry
 * only shows up (written by the resolver while it runs, not by the receiving
 * cache) or changes when cargo updates the crate; either way cargo's verdict
 * then belongs to another index than the answers'.
 */
export function indexRewrittenSince(before, after) {
  const rewritten = new Set();
  if (!before || !before.size || !after) return rewritten;
  for (const [name, mtime] of after) {
    if (before.has(name) && before.get(name) !== mtime) rewritten.add(name);
  }
  return rewritten;
}

// MARK: local inputs

async function exists(file) {
  try {
    await access(file);
    return true;
  } catch {
    return false;
  }
}

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

/** The cargo run's verdicts and the workspace's manifests, before any request. */
function rustLockfileSection(direct = {}) {
  const section = { updates: [], rustVersion: null, behind: null, crates: [], msrv: null };
  const dryRun = run("cargo", ["update", "--workspace", "--dry-run", "--verbose"], {
    env: { ...process.env, CARGO_TERM_COLOR: "never" },
  });
  if (dryRun.status !== 0) throw new CheckError(`cargo update --workspace --dry-run: ${lastLine(dryRun.stderr)}`);
  const parsed = parseCargoUpdates(dryRun.stdout + "\n" + dryRun.stderr);
  section.updates = parsed.updates;
  section.rustVersion = parsed.rustVersion;
  // `--verbose` adds the "Unchanged X (available: Y)" list: cargo's own record
  // of what the resolver left alone for this lockfile.
  section.behind = behindList(dryRun.stdout + "\n" + dryRun.stderr);

  // --no-deps: manifests only, no build, no index access.
  const metadata = run("cargo", ["metadata", "--locked", "--format-version", "1", "--no-deps"]);
  if (metadata.status !== 0) throw new CheckError(`cargo metadata: ${lastLine(metadata.stderr)}`);
  const parsedMetadata = JSON.parse(metadata.stdout);
  section.crates = directCrates(parsedMetadata);
  section.msrv = highestRustVersion(parsedMetadata);

  Object.assign(direct, {
    crates: section.crates,
    msrv: section.msrv,
    updates: section.updates,
    rustVersion: section.rustVersion,
    behind: section.behind,
  });
  return section;
}

/**
 * The cargo runs of one report: the index cache as it stood before them (a
 * file's mtime says whether a run left it alone), the dry runs' verdicts and
 * the workspace's manifests.
 *
 * Cargo's `--dry-run` never moves the lockfile, so it writes the span entry of
 * the ranges it resolved and nothing else. Another span — one the receiving
 * side of a cache download saw, from a resolver that did move a lockfile on
 * another machine — or a rewrite in place means the file belongs to another
 * lockfile: its releases would read as "skipped by cargo" here. That is the
 * CI case, where `~/.cargo` comes out of the cache.
 */
function cargoSnapshot(cache) {
  const before = cratesIoCacheMtimes(cargoHome() ? path.join(cargoHome(), "registry", "index") : null);
  return { before, after: null, behind: {}, cache };
}

/**
 * The cache for `indexRowsFor`, built before any cargo command runs: cargo's
 * rows per crate, and whether the file's span entry belongs to the lockfile on
 * disk. A span is written by the resolver that resolved the range, and only
 * the run's own `cargo update --dry-run` (which cannot move the lockfile)
 * writes one for this lockfile — into a file that held none before. A file
 * that already carried a span, or one this run rewrites, was written while
 * resolving another lockfile (a CI cache, an earlier run): its spans are
 * skipped and cargo's own list is the evidence.
 */
function cargoIndexCache(root) {
  const { crates, spanned } = readCargoIndexCache(root);
  const entries = new Map();
  for (const [name, rows] of crates) entries.set(name, { rows, trustSpans: false });
  return { entries, spanned, root };
}


/** Runs the report's cargo commands once; the answers fill `snapshot`. */
function collectRustInputs(snapshot) {
  const direct = {};
  const section = rustLockfileSection(direct);
  return section;
}

/** Takes the after-run index snapshot, as the same function as the before one. */
function finishCargoSnapshot(snapshot) {
  snapshot.after = cratesIoCacheMtimes(cargoHome() ? path.join(cargoHome(), "registry", "index") : null);
  return snapshot;
}
async function rustDirectSection(section, lockfile, requester, snapshot = {}) {
  const locked = new Map();
  const lockedVersions = new Map();
  for (const pkg of lockfile) {
    if (!pkg.name || !pkg.version || pkg.source === "local") continue;
    if (!lockedVersions.has(pkg.name)) lockedVersions.set(pkg.name, []);
    lockedVersions.get(pkg.name).push(pkg.version);
    const current = locked.get(pkg.name);
    if (!current || compareVersions(pkg.version, current) > 0) locked.set(pkg.name, pkg.version);
  }
  section.direct = [];
  section.errors = [];
  const behind = section.behind ?? behindList("");
  // The cache files as read before the runs; a file cargo rewrote since, or
  // one that already carried another resolver's span, holds spans this
  // lockfile did not get.
  const staleFiles = indexRewrittenSince(snapshot.before, snapshot.after);
  // A file that already held a span, or one this run rewrote, came from
  // another lockfile's resolver: only its plain rows are this lockfile's.
  const trusted = new Map(
    [...(snapshot.cache?.entries ?? new Map())].map(([name, entry]) => [
      name,
      { rows: entry.rows, trustSpans: entry.trustSpans && !staleFiles.has(name) },
    ]),
  );
  const index = await indexRowsFor(section.crates.map((crate) => crate.name), lockedVersions, requester, trusted, behind);
  // A crates.io index file cargo rewrote since this run started describes an
  // index other than the one the dry run resolved against (or the answer
  // came from): cargo's verdict for it is not this lockfile's.
  const staleNames = staleFiles;
  const heldBackContext = {
    behind,
    cargoRustVersion: section.rustVersion ?? section.msrv ?? null,
  };
  for (const { name, reqs } of section.crates) {
    const ours = locked.get(name);
    if (!ours) continue;
    const { rows: indexLines, notFound, indexKnown, error, reused } = index.get(name) ?? { rows: [], indexKnown: false };
    if (error) section.errors.push(`direct dependency ${name}: ${error}`);
    // The rows still describe the locked release: a request can fail and
    // leave no rows, and then the line and the latest version would describe
    // another crate state. A locked release that ships as a new crate
    // (`features.force`) was taken by npm, not cargo, and is only known from
    // cargo's own list.
    const listed = !behind?.unavailable && sameCore(behind?.[name]?.ours ?? "", ours);
    const forcedOnly = indexLines.some((entry) => sameCore(entry.vers, ours) && entry.features?.force);
    const covers = indexKnown && (indexLines.some((entry) => sameCore(entry.vers, ours)) || (forcedOnly && listed));
    // The index answer is older than the lockfile there is nothing to say
    // about: a crate whose index rows stop at a release at or below the
    // lockfile's own (the lockfile came from a newer index than this run's
    // answer).
    const rowNewest = indexLines
      .map((entry) => entry.vers)
      .filter((vers) => vers !== SPAN_ENTRY)
      .sort(compareVersions)
      .at(-1);
    // This name's own answer: its rows stop below the locked release (the
    // lockfile came from an index newer than this run's answer).
    const stale = Boolean(rowNewest) && compareVersions(rowNewest, ours) < 0;
    const incomplete = indexKnown && indexLines.length > 0 && !covers;
    const latest = covers ? newestRelease(indexLines) : null;
    const stableCounts = reqs.some((req) => /^\s*=/.test(String(req)) && parseVersion(req.replace(/^\s*=\s*/, "")).pre);
    const context = { ...heldBackContext, behind: staleNames.has(name) ? { unavailable: true } : heldBackContext.behind };
    const held = covers && !stale ? heldBack(context, indexLines, reqs, ours) : null;
    section.direct.push({
      name,
      req: [...new Set(reqs)].join(", "),
      // An `=` requirement moves only with a manifest edit.
      pinned: reqs.length > 0 && reqs.every((req) => /^\s*=/.test(String(req))),
      ours,
      // The newest the manifest requirement allows: what `cargo update` takes.
      wanted: covers && !stale ? newestAllowed(indexLines, reqs, ours) : "",
      // In the requirement's range, but cargo does not take it (see heldBack).
      held: held?.version ?? "",
      heldReason: held?.reason ?? "",
      // The first stable release while we sit on a prerelease.
      prelock: covers && !stale ? prereleaseAhead(indexLines, ours) : "",
      // The newest release of the requirement's own line (the stable after a
      // prerelease counts; a major upgrade shows in `latest`).
      lineNewest:
        [newestRelease(indexLines), newestInRange(indexLines, ours, stableCounts), prereleaseAhead(indexLines, ours)]
          .filter((version) => version && !isMajorChange(ours, version))
          .sort(compareVersions)
          .at(-1) ?? "",
      latest,
      behind: latest ? compareVersions(latest, ours) > 0 : false,
      major: latest ? isMajorChange(ours, latest) : false,
      // A name answered only by 404 is not on crates.io. A request that failed
      // says so: no reading, not a rename (a 304 is "Not Modified").
      note: !indexKnown
        ? notFound
          ? "not found on crates.io (HTTP 404)"
          : "crates.io index not read this run"
        : stale
          ? "index rows older than the lock; rerun"
          : reused
            ? "index read from cargo's cache (HTTP 304 Not Modified)"
          : indexLines.length
            ? ""
            : "crates.io index listed no release this run",
      incomplete: incomplete || !indexKnown,
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
  let index;
  try {
    index = await cratesIoIndex("rawler", requester);
  } catch (error) {
    if (!(error instanceof CheckError)) throw error;
  }
  section.cratesIo = index ? newestRelease(index) : null;
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
 * Rows of the direct-dependency table: what waits first (a newer release
 * `cargo update` takes, a release in range cargo holds back, the first stable
 * after a prerelease, a major on crates.io). A dependency whose in-range
 * release the lockfile already holds is current and stays out; a row whose
 * index answer was older than the lock is only listed as a major. The table
 * never takes what cargo would not: `data.rust.updates` is the take list.
 */
function directRows(direct) {
  const relevant = (row) => inRange(row) || Boolean(row.held) || Boolean(row.prelock) || row.major || Boolean(row.note);
  // A row whose index answer is older than the lock says nothing about the
  // crate (a stale cache in CI); it stays out until the run reads a newer one.
  const shown = direct.filter((row) => relevant(row) && !row.incomplete);
  return shown
    .sort(
      (a, b) => Number(relevant(b)) - Number(relevant(a)) || Number(b.major) - Number(a.major) || a.name.localeCompare(b.name),
    )
    .map((row) => {
      const note = row.note
        ? row.note
        : row.held
          ? `in range, held back (${row.heldReason})`
          : row.prelock && !row.wanted
            ? `next stable ${row.prelock}; the requirement allows the prerelease only`
            : row.pinned && row.wanted
              ? `\`${row.req}\` pinned; a manifest edit`
              : "";
      // The table agrees with cargo: a release cargo does not take is marked
      // held back in the cell, not shown as the "newest in range".
      // A hold names a release other than the newest cargo takes; one cell
      // never repeats the same version.
      const wanted = row.held
        ? row.wanted && row.wanted !== row.held
          ? `${row.wanted}, then ${row.held} (held back)`
          : `${row.held} (held back)`
        : row.wanted && row.prelock
          ? `${row.wanted} (prerelease)`
          : row.wanted || "—";
      return [row.name, row.ours, wanted, row.latest ?? "—", marks(row), note];
    });
}

/** directRows for the tests (the renderer keeps it private). */
export const directRowsForTest = (direct) => directRows(direct);

/**
 * Why a lockfile update section can be empty although crates.io is newer: a
 * member's rust-version above what `cargo update --workspace` resolves for
 * takes the in-range releases out of the resolver at once (cargo prints the
 * package that keeps it back; this line counts the rust-version instead).
 */
function resolveWitness(rust) {
  if (rust.behind !== null && rust.behind > 0) return "";
  const msrv = rust.msrv;
  if (!msrv || !rust.rustVersion || compareVersions(msrv, rust.rustVersion) <= 0) return "";
  return `Why this section can be empty: a workspace member's \`rust-version\` ${msrv} is above the Rust ${rust.rustVersion} that \`cargo update --workspace\` resolves for, so cargo leaves in-range releases above ${rust.rustVersion} out (\`cargo update -p NAME\` names the package that keeps it back); every member has to declare ${msrv} for it to go up.`;
}

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
    // Only a row the table shows is a move: an in-range release cargo does
    // not take is held back, not news, and the summary may not count it.
    const updatable = direct.filter((row) => inRange(row) && !row.held);
    const held = direct.filter((row) => row.held);
    const majors = direct.filter((row) => row.major);
    if (updatable.length) {
      const pinned = updatable.filter((row) => row.pinned);
      items.push(
        `${updatable.length} direct crate(s) can move within their requirement` +
          (pinned.length ? ` (${pinned.length} needs a manifest edit: \`=\` requirement)` : ""),
      );
    }
    if (held.length) {
      const names = held.map((row) => `\`${row.name}\``).sort();
      const listed = names.length > 8 ? `${names.slice(0, 8).join(", ")} and ${names.length - 8} more` : names.join(", ");
      items.push(`${held.length} direct crate(s) in range that \`cargo update\` does not take (${listed})`);
    }
    if (majors.length) items.push(`${majors.length} direct crate(s) across a major version`);
    const stable = direct.filter((row) => row.prelock && !row.wanted && !row.held);
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
      // "Nothing to update" is about the lockfile alone; the direct-dependency
      // table below names the in-range releases cargo leaves out of it, so the
      // sentence may not claim those do not exist.
      out.push(
        `Nothing to update${rust.rustVersion ? `: \`cargo update\` resolves for Rust ${rust.rustVersion} compatible versions` : ""}, and no lockfile release moves inside the existing requirements.`,
        "",
      );
    }
  } else {
    out.push("Not collected.", "");
  }

  out.push("### Rust: direct crates.io dependencies", "");
  out.push(
    "Per direct dependency of a workspace member, from `cargo metadata` and the crates.io sparse index (no API token): the newest version its requirement allows (what `cargo update` takes) and the newest published version.",
    "",
  );
  if (rust?.direct?.length) {
    const rows = directRows(rust.direct);
    out.push(...table(["Crate", "Ours", "Newest in range", "Newest", "", "Note"], rows));
    const held = rust.direct.filter((row) => row.held);
    if (held.length) {
      out.push(
        "`held back`: a release the requirement allows that `cargo update` does not take — its rust-version is above cargo's resolved Rust, or cargo's own `Unchanged … (available: …)` list names it and still updated nothing.",
        "",
      );
    }
    if (rust.direct.some((row) => /not found on crates\.io/.test(row.note ?? ""))) {
      out.push(
        "`not found on crates.io`: the sparse index answered 404, the only answer that says a name is unknown (a 304 is \"Not Modified\", not a rename).",
        "",
      );
    }
    const witness = resolveWitness(rust);
    if (witness) out.push(witness, "");
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
    "Inputs: the npm results read `node_modules` from `npm ci` on this lockfile; the Rust lockfile section runs `cargo update --dry-run`, the direct-dependency rows come from `cargo metadata`, `cargo update --dry-run --verbose` and the crates.io sparse index on `index.crates.io`, the rawler and toolchain rows from `raw.githubusercontent.com`, `api.github.com` and `static.rust-lang.org`.",
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

export async function collect(fetchImpl, { cache: cacheOption, indexMtimes } = {}) {
  // The index files are read before any request or cargo run (the runner
  // starts slower than the requests), so a crate's file missing here is
  // rewritten during the run, never by it.
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
  // Before any cargo runs refresh its index cache: what the first cargo
  // command finds on disk is the index from before the run.
  const indexCache = cacheOption ?? cargoIndexCache(cargoHome() ? path.join(cargoHome(), "registry", "index") : null);
  await guard("npm", () => npmSection(data.errors));
  const lockfile = parseLockfile(await readFile(path.join(ROOT, "Cargo.lock"), "utf8"));
  await guard("rust", async () => {
    const snapshot = cargoSnapshot(indexCache);
    if (indexMtimes) snapshot.after = indexMtimes;
    const section = collectRustInputs(snapshot);
    finishCargoSnapshot(snapshot);
    // Failed requests do not fail the run: each row says the index was not
    // read this run, and a 404 says the name is unknown.
    return rustDirectSection(section, lockfile, requester, {
      ...snapshot,
      after: snapshot.after ?? indexMtimes ?? null,
    });
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
