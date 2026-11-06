// Tests for scripts/dependency_report.mjs (node --test, part of scripts/check.sh).
// No network and no npm runs: cargo's and npm's output is taken as literals
// from this file (recorded runs on the laptop, 2026-11-06), and the report and
// issue-body rendering are pure.

import assert from 'node:assert/strict';
import { test } from 'node:test';

import {
  buildReport,
  cargoDryRun,
  collect,
  compareVersions,
  exitCode,
  fitIssueBody,
  isMajorChange,
  issueBody,
  lockfileRows,
  MAX_ISSUE_BODY,
  npmSection,
  renderStatus,
  parseAudit,
  parseCargoReport,
  parseHistory,
  parseOutdated,
  parseToolchainChannel,
  renderReport,
  summaryItems,
  table
} from './dependency_report.mjs';

test('versions compare like cargo: numerics, prereleases, build metadata', () => {
  assert.ok(compareVersions('1.2.3', '1.2.4') < 0);
  assert.ok(compareVersions('1.10.0', '1.9.9') > 0);
  assert.ok(compareVersions('0.8.0-alpha', '0.8.0') < 0);
  assert.ok(compareVersions('2.0.0-rc.2', '2.0.0-rc.10') < 0);
  assert.equal(compareVersions('1.1.4+spec-1.1.0', '1.1.4'), 0);
  assert.equal(isMajorChange('1.2.0', '2.0.0'), true);
  // For 0.x crates the minor is the breaking position.
  assert.equal(isMajorChange('0.14.0', '0.15.0'), true);
  assert.equal(parseToolchainChannel('[toolchain]\nchannel = "1.98.1"\n'), '1.98.1');
});

// MARK: cargo output (the lines are copied from a real plain dry run, colors included)

// A plain `cargo update --dry-run --verbose` in this workspace (cargo 1.98.1).
// It says nothing about why a crate stays where it is; the only reason cargo
// gives, `requires Rust`, sits inside `(available: …)`.
const DRY_RUN = [
  '\u001b[1m\u001b[92m    Updating\u001b[0m crates.io index',
  '\u001b[1m\u001b[92m     Locking\u001b[0m 170 packages to latest Rust 1.88 compatible versions',
  '\u001b[1m\u001b[92m    Updating\u001b[0m addr2line v0.25.1 -> v0.27.1',
  '\u001b[1m\u001b[92m    Updating\u001b[0m glam v0.33.5 -> v0.33.12 (available: v0.34.2)',
  '\u001b[1m\u001b[92m      Adding\u001b[0m bit-set v0.11.1',
  '\u001b[1m\u001b[92m    Updating\u001b[0m uuid v1.25.0 -> v1.26.1 (available: v1.29.0, requires Rust 1.89.0)',
  '\u001b[1m\u001b[92m    Updating\u001b[0m daachorse v1.0.1 -> v3.0.3',
  '\u001b[1m\u001b[92m Downgrading\u001b[0m ordered-float v5.5.0 -> v5.4.0 (available: v5.5.1)',
  '\u001b[1m\u001b[92m    Removing\u001b[0m png v0.17.16',
  '   Unchanged moxcms v0.8.1 (available: v0.9.1, requires Rust 1.89.0)',
  '   Unchanged gtk v0.18.2 (available: v0.19.0)',
  '   Unchanged serde v1.0.229 (available: v1.0.230)',
  'note: to see how you depend on a package, run `cargo tree --invert <dep>@<ver>`',
  'warning: not updating lockfile due to dry run'
].join('\n');

test("the plain dry run's lines parse: what cargo takes, and what it leaves with cargo's own reason", () => {
  const report = parseCargoReport(DRY_RUN);
  assert.equal(report.rustVersion, '1.88');
  assert.deepEqual(
    report.changes.map(({ kind, name, from, to, available }) => [kind, name, from, to, available]),
    [
      ['Updating', 'addr2line', '0.25.1', '0.27.1', ''],
      ['Updating', 'glam', '0.33.5', '0.33.12', '0.34.2'],
      ['Adding', 'bit-set', '0.11.1', '', ''],
      ['Updating', 'uuid', '1.25.0', '1.26.1', '1.29.0'],
      ['Updating', 'daachorse', '1.0.1', '3.0.3', ''],
      ['Downgrading', 'ordered-float', '5.5.0', '5.4.0', '5.5.1'],
      ['Removing', 'png', '0.17.16', '', '']
    ]
  );
  // `requires Rust` is cargo's reason, kept word for word; without one there is none.
  assert.equal(report.changes.find((change) => change.name === 'uuid').reason, 'requires Rust 1.89.0');
  assert.equal(report.changes.find((change) => change.name === 'glam').reason, '');
  // In the order cargo prints them.
  assert.deepEqual(report.behind, [
    {
      name: 'moxcms',
      ours: '0.8.1',
      available: '0.9.1',
      reason: 'requires Rust 1.89.0'
    },
    { name: 'gtk', ours: '0.18.2', available: '0.19.0', reason: '' },
    { name: 'serde', ours: '1.0.229', available: '1.0.230', reason: '' }
  ]);
  assert.equal(report.rustVersionNote, 'Locking 170 packages to latest Rust 1.88 compatible versions');
  // The dry run's own lines are no news; a real warning or error is.
  assert.deepEqual(report.errors, []);
  assert.deepEqual(parseCargoReport('error: failed to get `serde` as a dependency').errors, [
    'error: failed to get `serde` as a dependency'
  ]);
});

test('the dry run is a plain `cargo update --dry-run` (no --workspace, no lockfile change)', () => {
  const calls = [];
  const report = cargoDryRun((command, args) => {
    calls.push([command, args]);
    return { stdout: DRY_RUN, stderr: '', status: 0 };
  });
  assert.deepEqual(calls, [['cargo', ['update', '--dry-run', '--verbose']]]);
  assert.equal(report.changes.length, 7);
  // A dry run that failed is reported with cargo's last line, not parsed as empty.
  assert.throws(
    () =>
      cargoDryRun(() => ({
        stdout: '',
        stderr: 'error: failed to load source',
        status: 101
      })),
    /^Error: cargo update --dry-run --verbose: error: failed to load source$/
  );
});

test('the lockfile section lists both kinds of crate and infers no requirement', () => {
  const rows = lockfileRows(parseCargoReport(DRY_RUN));
  // What cargo would take: a move, an add, a removal, a downgrade.
  assert.deepEqual(
    rows.updates.map((row) => [row.crate, row.from, row.to, row.kind]),
    [
      ['addr2line', '0.25.1', '0.27.1', 'updated'],
      ['bit-set', '—', '0.11.1', 'added'],
      ['daachorse', '1.0.1', '3.0.3', 'updated'],
      ['glam', '0.33.5', '0.33.12', 'updated'],
      ['ordered-float', '5.5.0', '5.4.0', 'downgrade'],
      ['png', '0.17.16', '—', 'removed'],
      ['uuid', '1.25.0', '1.26.1', 'updated']
    ]
  );
  // `glam` moves to 0.33.12 with 0.34.2 available: cargo would not take more.
  assert.equal(rows.updates.find((row) => row.crate === 'glam').reason, '0.34.2 available');
  assert.equal(rows.updates.find((row) => row.crate === 'uuid').reason, 'requires Rust 1.89.0');
  // What cargo leaves, with cargo's reason when it gives one.
  assert.deepEqual(
    rows.held.map((row) => [row.crate, row.ours, row.available, row.reason]),
    [
      ['gtk', '0.18.2', '0.19.0', ''],
      ['moxcms', '0.8.1', '0.9.1', 'requires Rust 1.89.0'],
      ['serde', '1.0.229', '1.0.230', '']
    ]
  );
});

// MARK: npm

/** A fake npm: `outdated` answers with versions, the audit with `auditJson`. */
const fakeNpm = (auditJson, outdated) => (_command, args) => {
  // node_modules exists in this checkout, so npmSection does not stop early.
  if (args[0] === 'audit') return { stdout: auditJson, stderr: '', status: 1 };
  return typeof outdated === 'string' ? { stdout: outdated, stderr: '', status: 1 } : outdated;
};

const CLEAN_AUDIT = JSON.stringify({
  vulnerabilities: {},
  metadata: {
    vulnerabilities: {
      info: 0,
      low: 0,
      moderate: 0,
      high: 0,
      critical: 0,
      total: 0
    }
  }
});
const FOUND_AUDIT = JSON.stringify({
  vulnerabilities: {
    glob: {
      name: 'glob',
      severity: 'moderate',
      isDirect: false,
      fixAvailable: true,
      via: [{ title: 'Glob: AbortSignal' }]
    },
    linkify: {
      name: 'linkify',
      severity: 'info',
      isDirect: true,
      fixAvailable: true,
      via: []
    },
    // npm reports a withdrawn advisory under the id of the advisory it replaced.
    'static-eval': {
      name: 'static-eval',
      severity: 'high',
      isDirect: false,
      fixAvailable: false,
      via: [
        {
          title: 'Static Eval: Code injection',
          url: 'https://github.com/advisories/GHSA-1'
        }
      ]
    }
  }
});

test("an npm audit that errored is 'audit failed', never clean", () => {
  // The shapes npm answers when it could not audit: its own error object, a
  // registry answer in place of JSON, and no output at all.
  for (const output of [
    '{\n  "error": {\n    "summary": "cb() never called!",\n    "detail": ""\n  }\n}',
    '<html>503</html>',
    ''
  ]) {
    assert.throws(() => parseAudit(output, 'npm ERR! network', 1), /cb\(\) never called|no JSON/);
  }
  // A JSON report without a vulnerabilities table is no clean result either.
  assert.throws(() => parseAudit('{"metadata":{"vulnerabilities":{"total":0}}}', '', 0), /no vulnerabilities table/);
  // A real audit: its findings, severities ordered, info left out, listed as npm reports them.
  assert.deepEqual(
    parseAudit(FOUND_AUDIT, '', 1).map((row) => [row.name, row.severity, row.fix]),
    [
      ['static-eval', 'high', 'none'],
      ['glob', 'moderate', 'available']
    ]
  );
  assert.deepEqual(parseAudit(CLEAN_AUDIT, '', 0), []);
});

test('npmSection names a failed shipped audit as an error and a failed dev audit as a note', async () => {
  // The message starts with the audit that failed, so the report says what broke.
  await assert.rejects(
    npmSection(fakeNpm('{"error":{"summary":"audio"}}', '{}')),
    (error) => error.message === 'npm audit --omit=dev: audio'
  );
  await assert.rejects(npmSection(fakeNpm('not json', '{}')), (error) =>
    error.message.startsWith('npm audit --omit=dev: no JSON')
  );
  // The same failure on the dev tree (the second call) is listed, and the
  // shipped result stands.
  let call = 0;
  const section = await npmSection((command, args) => {
    if (args[0] === 'audit')
      return {
        stdout: call++ === 0 ? CLEAN_AUDIT : '{"error":{"summary":"EBUSY, scandir"}}',
        stderr: '',
        status: 1
      };
    return { stdout: '{}', stderr: '', status: 0 };
  });
  assert.deepEqual(section.audit, []);
  assert.match(section.notes[0], /npm audit \(all, dev tree included\) did not run: EBUSY, scandir/);
  assert.ok(
    renderReport({
      errors: [],
      npm: section,
      rust: null,
      toolchain: null
    }).includes('npm audit (all, dev tree included) did not run')
  );
});

test("an npm outdated answer that is no package list is 'could not check', never 'nothing behind'", () => {
  // npm's answers when it could not list (npm 11.20.0, offline probe, 2026-11-06).
  const ECONNREFUSED =
    '{\n  "error": {\n    "code": "ECONNREFUSED",\n    "summary": "FetchError: request to http://127.0.0.1:9/left-pad failed, reason: connect ECONNREFUSED 127.0.0.1:9",\n    "detail": ""\n  }\n}';
  const ENOTCACHED =
    '{\n  "error": {\n    "code": "ENOTCACHED",\n    "summary": "request to https://registry.npmjs.org/left-pad failed: cache mode is \'only-if-cached\' but no cached response is available.",\n    "detail": ""\n  }\n}';
  // Each shape is a failure with the npm reason, in the error channel.
  assert.deepEqual(parseOutdated('{"error":{"code":"ENOTCACHED","summary":"request failed"}}', '', 1), {
    rows: [],
    error: 'request failed'
  });
  assert.deepEqual(
    parseOutdated(ECONNREFUSED, '', 1).error,
    'FetchError: request to http://127.0.0.1:9/left-pad failed, reason: connect ECONNREFUSED 127.0.0.1:9'
  );
  assert.match(parseOutdated(ENOTCACHED, '', 1).error, /^request to .* failed/);
  // A non-zero exit with no output at all says only its exit status.
  assert.deepEqual(parseOutdated('', 'npm error code E404\nnpm error 404 Not Found', 1), { rows: [], error: 'exit 1' });
  assert.deepEqual(parseOutdated('\n  \n', '', 1), {
    rows: [],
    error: 'exit 1'
  });
  assert.match(parseOutdated('', '', 1).error, /exit 1/);
  // An answer that is no JSON, and one that is no package list.
  assert.match(parseOutdated('<html>503</html>', '', 1).error, /no JSON/);
  assert.match(parseOutdated('[]', '', 0).error, /no package list/);
  assert.deepEqual(
    parseOutdated('{"vite":"8.3.5"}', '', 1),
    { rows: [], error: '' },
    'a value that is no row is npm saying nothing'
  );
  // Empty stdout with exit 0 is npm's own "nothing": an empty list, no error.
  assert.deepEqual(parseOutdated('', '', 0), { rows: [], error: '' });
  assert.deepEqual(parseOutdated('{}', '', 1), { rows: [], error: '' });
  // A row without `current` is not listed either (npm names it only when installed).
  assert.deepEqual(parseOutdated('{"a":{"wanted":"1.0.0","latest":"1.0.0"}}', '', 1).rows, []);
});

test("npmSection: an npm outdated that could not run fails the report and writes no 'nothing behind'", async () => {
  const section = await npmSection(
    fakeNpm(
      CLEAN_AUDIT,
      '{"error":{"code":"ECONNREFUSED","summary":"FetchError: request to https://registry.npmjs.org/x failed"}}'
    )
  );
  assert.deepEqual(section.outdated, []);
  assert.match(section.errors[0], /^npm outdated: FetchError/);
  const report = renderReport({
    errors: section.errors,
    npm: section,
    rust: null,
    toolchain: null
  });
  assert.ok(!report.includes('Nothing behind'), report);
  assert.match(
    report,
    /### npm outdated[^\n]*\n\nNot collected \(npm outdated could not run: FetchError: request to https:\/\/registry\.npmjs\.org\/x failed\)\.\n/
  );
  const items = summaryItems({ errors: [], npm: section });
  assert.ok(
    items.some((item) => item.startsWith('npm outdated could not run: FetchError')),
    items.join('\n')
  );
  // A non-zero exit with no output is the same, never "nothing behind".
  const empty = await npmSection(fakeNpm(CLEAN_AUDIT, ''));
  assert.match(empty.errors[0], /^npm outdated: exit 1$/);
});

test('npm outdated marks majors and the tree they are used from', async () => {
  const section = await npmSection(
    fakeNpm(
      CLEAN_AUDIT,
      JSON.stringify({
        'framer-motion': {
          current: '13.1.1',
          wanted: '13.5.1',
          latest: '14.9.0'
        },
        vite: { current: '7.1.0', wanted: '7.1.2', latest: '7.1.2' }
      })
    )
  );
  assert.deepEqual(section.errors, []);
  assert.deepEqual(
    section.outdated.map((row) => [row.name, row.major, row.usedBy]),
    [
      ['framer-motion', true, 'prod'],
      ['vite', false, 'dev']
    ]
  );
});

// MARK: the report and its exit code

test('a failed part leaves the report written, names what failed, and still fails the run', async () => {
  // cargo: a dry run that failed; npm: a security finding; rust-lang.org: down.
  const runCommand = (command, args) => {
    if (command === 'cargo')
      return {
        stdout: '',
        stderr: 'error: failed to load source for dependency `x`',
        status: 101
      };
    return fakeNpm(FOUND_AUDIT, '{}')(command, args);
  };
  // A 503 answers at once too, so the retries do not wait.
  const fetchImpl = async () => ({ status: 503, text: async () => '' });
  const { data, report } = await buildReport({
    runCommand,
    fetchImpl,
    now: new Date('2026-11-06T05:00:00Z')
  });
  // The report exists although two parts failed, and says which.
  assert.match(report, /### Rust: lockfile[\s\S]*Not collected \(the cargo dry run did not finish\)/);
  assert.match(report, /### Rust toolchain[\s\S]*Not collected/);
  assert.match(report, /\*\*Could not check\*\*/);
  for (const error of data.errors) assert.ok(report.includes(`- ${error}`), error);
  assert.ok(
    data.errors.some(
      (error) => error === 'cargo update --dry-run --verbose: error: failed to load source for dependency `x`'
    ),
    data.errors.join('\n')
  );
  assert.ok(
    data.errors.some((error) => error === 'static.rust-lang.org (Rust stable channel): HTTP 503'),
    data.errors.join('\n')
  );
  // It is a complete npm report, and the run fails.
  assert.match(report, /2 npm security finding\(s\) in shipped code/);
  assert.equal(exitCode(data), 1);
});

/** npm answers as `fakeNpm`, cargo with the recorded dry run (no registry is read). */
const fakeCommands = (outdated) => (command, args) =>
  command === 'cargo' ? { stdout: DRY_RUN, stderr: '', status: 0 } : fakeNpm(CLEAN_AUDIT, outdated)(command, args);

/** No network: every request is refused at once (no retry waits). */
const offline = async (url) => {
  throw new Error(`connect ECONNREFUSED ${url}`);
};

test('a failed request is listed by the run', async () => {
  const data = await collect({
    runCommand: fakeCommands('{}'),
    fetchImpl: offline
  });
  assert.ok(data.rust && data.rust.lockfile.updates.length > 0, JSON.stringify(data.errors));
  assert.equal(data.toolchain, null);
  assert.equal(data.errors.length, 1);
  assert.match(data.errors[0], /^static\.rust-lang\.org \(Rust stable channel\): connect ECONNREFUSED/);
  // An npm outdated that could not check is in the report's failure list too.
  const withOutdated = await collect({
    runCommand: fakeCommands('{"error":{"summary":"ENOTCACHED: no cached response"}}'),
    fetchImpl: offline
  });
  assert.deepEqual(withOutdated.errors, [
    'npm outdated: ENOTCACHED: no cached response',
    'static.rust-lang.org (Rust stable channel): connect ECONNREFUSED'
  ]);
  assert.equal(exitCode(withOutdated), 1);
  // The failed part is listed in the report, not hidden.
  const report = renderReport(data, new Date('2026-11-06T05:00:00Z'));
  assert.match(report, /^- static\.rust-lang\.org \(Rust stable channel\): connect ECONNREFUSED/m);
  assert.equal(exitCode(data), 1);
});

/** A complete report's data: one npm package behind, the DRY_RUN lockfile, the toolchain current. */
function cleanData() {
  return {
    errors: [],
    branch: 'main',
    runUrl: null,
    npm: {
      audit: [],
      devAudit: [],
      outdated: [
        {
          name: 'vite',
          current: '7.1.0',
          wanted: '7.1.2',
          latest: '7.1.2',
          major: false,
          usedBy: 'dev'
        }
      ],
      notes: [],
      errors: []
    },
    rust: {
      lockfile: lockfileRows(parseCargoReport(DRY_RUN)),
      rustVersionNote: 'Locking 170 packages to latest Rust 1.88 compatible versions'
    },
    toolchain: { pinned: '1.98.1', stable: '1.98.1', behind: false }
  };
}

test('a clean report passes, and the rendered report keeps the sections', () => {
  const data = cleanData();
  const report = renderReport(data, new Date('2026-11-06T05:00:00Z'));
  for (const heading of [
    '### Summary',
    '**Could not check**',
    '### npm security',
    '### npm outdated',
    '### Rust: lockfile',
    '### Rust toolchain'
  ]) {
    assert.ok(report.includes(heading), heading);
  }
  assert.match(report, /- nothing\n/);
  // Rust: what cargo would take, and what it leaves with cargo's own reason.
  assert.match(report, /\| addr2line \| 0\.25\.1 \| 0\.27\.1 \| update possible \(updated\) \|  \|/);
  assert.match(report, /\| bit-set \| — \| 0\.11\.1 \| update possible \(added\) \|  \|/);
  assert.match(
    report,
    /\| ordered-float \| 5\.5\.0 \| 5\.4\.0 \| update possible \(downgrade\) \| 5\.5\.1 available \|/
  );
  assert.match(report, /\| moxcms \| 0\.8\.1 \| 0\.9\.1 \| available, held back \| requires Rust 1\.89\.0 \|/);
  assert.match(report, /\| gtk \| 0\.18\.2 \| 0\.19\.0 \| available, held back \|  \|/);
  // Cargo's own reason, repeated once, and said to be thin for the rest.
  assert.match(
    report,
    /Cargo gives a reason for one of the 3 held crates: requires Rust 1\.89\.0 \(1 crate\); it gives none for the rest\./
  );
  assert.ok(!/\n\n\n/.test(report), 'no triple newline');
  assert.equal(exitCode(data), 0);
  const items = summaryItems(data);
  assert.ok(items.includes('7 Rust lockfile change(s) `cargo update` would take'), items.join('\n'));
  assert.ok(items.includes('3 crate(s) update possible but held back'), items.join('\n'));
  // Nothing about requirements, Cargo.toml edits or withdrawn advisories: the
  // report prints only what cargo and npm said.
  assert.ok(!/requirement|Cargo\.toml edit|withdrawn|bundled advisory/i.test(report), report);
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
          name: `package-${String(index).padStart(4, '0')}`,
          severity: 'moderate',
          kind: 'transitive',
          advisories: 'A long advisory title that fills the row to about 90 characters',
          fix: 'available'
        })),
        outdated: [],
        notes: [],
        errors: []
      },
      rust: { lockfile: lockfileRows(parseCargoReport(DRY_RUN)) },
      toolchain: { pinned: '1.98.1', stable: '1.99.0', behind: true }
    },
    new Date('2026-11-06T05:00:00Z'),
    tableOptions
  );
}

test("a body over GitHub's 65,536-character limit is cut, names the cut rows, and stays Markdown", () => {
  const report = bigReport(900);
  const history = parseHistory(
    issueBody({
      report: bigReport(700),
      status: 'an older run',
      at: new Date('2026-10-30T05:00:00Z')
    })
  );
  const body = issueBody({
    report,
    status: '1 npm package behind',
    at: new Date('2026-11-06T05:00:00Z'),
    history
  });
  const fitted = fitIssueBody(body, MAX_ISSUE_BODY);
  assert.ok(fitted.body.length <= MAX_ISSUE_BODY, `${fitted.body.length} characters`);
  // Just under the limit the first step (the earlier run's rows) is enough.
  const slight = fitIssueBody(body, body.length - 1000);
  assert.ok(slight.body.length <= body.length - 1000, `${slight.body.length} characters`);
  assert.deepEqual(slight.notes, ["earlier runs' tables cut to their headers"]);
  assert.ok(
    slight.body.includes('| _… 101 more rows_ |'),
    "the earlier run's table says how many rows went (the table holds 100)"
  );
  // The current report is whole, and its 100-row table says how many it hides.
  assert.equal((slight.body.match(/^\| package-\d\d\d\d \|/gm) ?? []).length, 100);
  assert.ok(slight.body.includes('| _… 800 more rows_ |'), 'the table names the rows it does not list');
  // Header and divider lines are untouched, so the tables stay tables.
  assert.match(slight.body, /^\| Package \| Severity \| Where \| Advisory \| Fix \|$/m);
  assert.match(parseHistory(slight.body)[0].report, /Rust toolchain 1\.98\.1 behind stable 1\.99\.0/);
  // Far over (a test limit): the earlier runs go entirely and the tail is cut.
  const hard = fitIssueBody(body, 8000);
  assert.ok(hard.body.length <= 8000, `${hard.body.length} characters`);
  assert.ok(!hard.body.includes('## Earlier runs'));
  assert.ok(hard.body.includes("Cut to fit the issue's 8000-character limit"));
});

test('a body that fits is left exactly as it is', () => {
  const body = issueBody({
    report: bigReport(3),
    status: 'nothing behind',
    at: new Date('2026-11-06T05:00:00Z')
  });
  assert.deepEqual(fitIssueBody(body), { body, notes: [] });
});

test('table() keeps at most maxRows and names the rest', () => {
  const rows = Array.from({ length: 106 }, (_, index) => [`crate-${index}`, '1.0.0', '1.0.1']);
  const lines = table(['Crate', 'Locked', 'New'], rows);
  assert.equal(lines.filter((line) => /^\| crate-/.test(line)).length, 100);
  assert.deepEqual(
    lines.filter((line) => line.includes('more rows')),
    ['| _… 6 more rows_ |']
  );
});

test("a check that could not run is not 'nothing behind' in the status line", () => {
  const data = {
    errors: ['cargo update --dry-run --verbose: error: failed to load source'],
    npm: null,
    rust: null,
    toolchain: null
  };
  const status = renderStatus(data);
  assert.ok(!status.includes('nothing behind'), status);
  assert.match(status, /1 check\(s\) could not run/);
  assert.equal(renderStatus({ ...data, errors: [] }), 'nothing behind');
});

test("a dev-tree audit that did not run is 'not collected', never 'none'", async () => {
  // The shipped-tree audit answers; the all-trees one gets a registry error.
  const run = (command, args) =>
    args[0] === 'audit' && !args.includes('--omit=dev')
      ? { stdout: '{"error":{"summary":"ENOTFOUND registry"}}', stderr: '', status: 1 }
      : fakeNpm(CLEAN_AUDIT, '{}')(command, args);
  const section = await npmSection(run);
  assert.equal(section.devAuditFailed, true);
  const report = renderReport({
    errors: section.errors,
    npm: section,
    rust: null,
    toolchain: null
  });
  assert.match(report, /Dev tree \(`npm audit`, listed only\): not collected\./);
  assert.ok(!/Dev tree[^\n]*: none/.test(report), report);
});
