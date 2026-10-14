import { describe, expect, it } from 'vitest';
import {
  defaultSelection,
  planIsEmpty,
  rowKey,
  shownPath,
  sortPlan,
  syncActions,
  unresolved,
  type ContestedChoice,
  type SyncPlan,
} from './folderSync';
import { identity, samplePlan } from './folderSync.fixture';

describe('Synchronize Folder review', () => {
  it('ticks verified moves and new files only', () => {
    const plan = samplePlan();
    const selected = defaultSelection(plan);
    expect(selected.has(rowKey('verifiedMoves', plan.verifiedMoves[0]))).toBe(true);
    expect(selected.has(rowKey('newFiles', plan.newFiles[0]))).toBe(true);
    expect(selected.has(rowKey('probableMoves', plan.probableMoves[0]))).toBe(false);
    expect(selected.has(rowKey('copies', plan.copies[0]))).toBe(false);
    expect(selected.has(rowKey('removed', plan.removed[0]))).toBe(false);
    expect(selected.size).toBe(2);
  });

  it('makes actions from the ticked rows and the contested choices', () => {
    const plan = samplePlan();
    const choices = new Map<string, ContestedChoice>();
    expect(unresolved(plan, choices)).toBe(1);
    expect(syncActions(plan, defaultSelection(plan), choices)).toEqual([
      {
        kind: 'move',
        assetId: 'a',
        from: '/lib/Capture/A.ARW',
        to: '/lib/Capture/Day 2/A.ARW',
        identity: identity(1),
      },
      { kind: 'add', path: '/lib/Capture/N.ARW', identity: identity(2) },
    ]);
    choices.set('/lib/Capture/X.ARW', { move: 'x' });
    expect(unresolved(plan, choices)).toBe(0);
    const selected = new Set([rowKey('removed', plan.removed[0])]);
    expect(syncActions(plan, selected, choices)).toEqual([
      { kind: 'remove', assetId: 'r', path: '/lib/Capture/R.ARW' },
      { kind: 'move', assetId: 'x', from: '/lib/Capture/old/X.ARW', to: '/lib/Capture/X.ARW', identity: identity(1) },
    ]);
    choices.set('/lib/Capture/X.ARW', 'add');
    expect(syncActions(plan, new Set(), choices)).toEqual([
      { kind: 'add', path: '/lib/Capture/X.ARW', identity: identity(4) },
    ]);
    choices.set('/lib/Capture/X.ARW', 'skip');
    expect(syncActions(plan, new Set(), choices)).toEqual([]);
  });

  it('knows a plan with nothing to do', () => {
    const plan = samplePlan();
    expect(planIsEmpty(plan)).toBe(false);
    expect(
      planIsEmpty({
        ...plan,
        verifiedMoves: [],
        probableMoves: [],
        newFiles: [],
        copies: [],
        contested: [],
        removed: [],
      }),
    ).toBe(true);
  });
});

describe('shownPath', () => {
  it('shows a path relative to the innermost scanned folder that holds it', () => {
    expect(shownPath(['/lib/Capture'], '/lib/Capture/Day 2/A.ARW')).toBe('Day 2/A.ARW');
    expect(shownPath(['/lib', '/lib/Capture/'], '/lib/Capture/A.ARW')).toBe('A.ARW');
    expect(shownPath(['/lib/Capture'], '/lib/Captured/A.ARW')).toBe('/lib/Captured/A.ARW');
    expect(shownPath([], '/Volumes/Photos/U.ARW')).toBe('/Volumes/Photos/U.ARW');
    expect(shownPath(['C:\\Photos'], 'C:\\Photos\\Day\\A.ARW')).toBe('Day\\A.ARW');
  });
});

describe('sortPlan', () => {
  it('lists rows in natural order', () => {
    const plan = samplePlan();
    const names = ['copy_10', 'copy_2', 'Copy_1', 'copy_100', 'copy_9'];
    const file = (name: string, n: number) => ({ path: `/lib/Capture/${name}.ARW`, identity: identity(n), copyOf: [] });
    const sorted = sortPlan({
      ...plan,
      newFiles: names.map(file),
      removed: names.map((name, n) => ({ assetId: `${n}`, path: `/lib/Capture/${name}.ARW`, volume: null })),
      verifiedMoves: names.map((name, n) => ({ ...plan.verifiedMoves[0], assetId: `${n}`, to: `/lib/Capture/${name}.ARW` })),
    });
    const order = ['Copy_1', 'copy_2', 'copy_9', 'copy_10', 'copy_100'].map((name) => `/lib/Capture/${name}.ARW`);
    expect(sorted.newFiles.map((f) => f.path)).toEqual(order);
    expect(sorted.removed.map((f) => f.path)).toEqual(order);
    expect(sorted.verifiedMoves.map((m) => m.to)).toEqual(order);
    // Folders before deeper names the same way: Day 2 before Day 10.
    expect(
      sortPlan({ ...plan, newFiles: [file('Day 10/A', 1), file('Day 2/A', 2)] }).newFiles.map((f) => f.path),
    ).toEqual(['/lib/Capture/Day 2/A.ARW', '/lib/Capture/Day 10/A.ARW']);
    // The scan's plan is left as it was.
    expect(plan.newFiles[0].path).toBe('/lib/Capture/N.ARW');
  });
});
