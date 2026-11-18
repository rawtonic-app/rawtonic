// Accessibility audit of rendered views with axe-core (MPL-2.0, dev dependency
// only; not shipped). Each view's test renders it in jsdom, runs axe over the
// document and fails on any violation that is not on that view's allow-list.
//
// jsdom has no layout and the tests load no Tailwind CSS, so colour contrast
// cannot be measured here: `color-contrast` is off and contrast is recorded as
// a design question in the audit report (notes: benchmark-results/a11y2-2026-11-18/).
// `A11Y_REPORT=<new dir>` writes every violation (allowed or not) as JSON for that
// report; it refuses to replace a file an earlier run wrote there.
import axe from 'axe-core';
import { expect } from 'vitest';

export interface Allowed {
  /** axe rule id. */
  rule: string;
  /** Substring of the node's CSS target or HTML; omitted allows every node of the rule. */
  node?: string;
  /** Why this one stays for now. */
  reason: string;
}

export interface Finding {
  view: string;
  rule: string;
  impact: string | null | undefined;
  help: string;
  target: string;
  html: string;
  allowed?: string;
}

const RULES_OFF: Record<string, { enabled: boolean }> = {
  // Needs real layout and the compiled stylesheet; jsdom has neither.
  'color-contrast': { enabled: false },
  'color-contrast-enhanced': { enabled: false },
  // jsdom documents have no <title>; the app's index.html has one ("Rawtonic").
  'document-title': { enabled: false }
};

/** Shared allow-list entries for structures that several views render. */
export const KNOWN: Record<'toolHeader' | 'gridTile', Allowed> = {
  // A tool group's header is the <summary> of its <details> and holds the
  // eye, presets, copy and reset buttons. Moving them out of the summary
  // changes the header's layout, drag and Option-click behaviour: a design
  // change recorded in the A11Y2 report, not done here.
  toolHeader: {
    rule: 'nested-interactive',
    node: 'summary',
    reason: 'tool group header buttons inside <summary> (A11Y2 report: design change)'
  },
  // A photograph tile is a listbox option holding its open button; the grid's
  // one-Tab-stop keyboard model is on the parked branch a11-restore-focus-ring.
  gridTile: {
    rule: 'nested-interactive',
    node: 'studio-photo',
    reason: 'photo tile option holds a button (grid keyboard model: parked a11-restore-focus-ring)'
  }
};

export interface AuditOptions {
  /** The whole workspace, not one component: also checks that content sits in landmarks. */
  page?: boolean;
}

const findings: Finding[] = [];

/** Runs axe over the document and returns the violations as one row per node. */
export async function auditDocument(view: string, allowed: Allowed[] = [], options: AuditOptions = {}) {
  const rules = { ...RULES_OFF };
  // A component rendered on its own sits outside the app's landmarks; `region`
  // is checked where the whole workspace renders.
  if (!options.page) rules.region = { enabled: false };
  const result = await axe.run(document, { rules, resultTypes: ['violations'] });
  const rows: Finding[] = [];
  for (const violation of result.violations)
    for (const node of violation.nodes) {
      const target = node.target.join(' ');
      const allow = allowed.find(
        (a) => a.rule === violation.id && (!a.node || target.includes(a.node) || node.html.includes(a.node))
      );
      rows.push({
        view,
        rule: violation.id,
        impact: violation.impact,
        help: violation.help,
        target,
        html: node.html.slice(0, 240),
        ...(allow ? { allowed: allow.reason } : {})
      });
    }
  findings.push(...rows);
  return rows;
}

/** Fails with a readable list when a view has a violation not on its allow-list. */
export async function expectNoNewViolations(view: string, allowed: Allowed[] = [], options: AuditOptions = {}) {
  const rows = await auditDocument(view, allowed, options);
  if (await writeReport(view, rows)) return;
  const unexpected = rows
    .filter((row) => !row.allowed)
    .map((row) => `${row.rule} (${row.impact}): ${row.target}\n    ${row.html}`);
  expect(unexpected, `${view}: new accessibility violations`).toEqual([]);
}

async function writeReport(view: string, rows: Finding[]) {
  const dir = (globalThis as { process?: { env: Record<string, string | undefined> } }).process?.env.A11Y_REPORT;
  if (!dir) return false;
  // @ts-expect-error node:fs is untyped in the frontend tsconfig
  const fs = await import('node:fs');
  fs.mkdirSync(dir, { recursive: true });
  const name = view.replace(/[^a-z0-9]+/gi, '-').toLowerCase();
  // Never over an earlier report (a before/after pair lives in two directories):
  // `wx` refuses an existing file, so a reused directory fails instead of
  // replacing its baseline.
  fs.writeFileSync(`${dir}/${name}.json`, JSON.stringify(rows, null, 1), { flag: 'wx' });
  // Report mode collects every view instead of stopping at the first.
  return true;
}

export const allFindings = () => findings;
