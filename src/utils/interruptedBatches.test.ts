import '../i18n';
import { beforeEach, describe, expect, it, vi } from 'vitest';
import { invoke } from '@tauri-apps/api/core';
import { useActivityStore } from '../store/useActivityStore';
import { useLibraryStore } from '../store/useLibraryStore';
import { offerInterruptedBatches, resetInterruptedBatches, type InterruptedBatch } from './interruptedBatches';

vi.mock('@tauri-apps/api/core', () => ({ invoke: vi.fn() }));
vi.mock('../activity/notify', () => ({ notify: { error: vi.fn(), success: vi.fn(), warn: vi.fn() } }));

const paste: InterruptedBatch = {
  id: 'run-1-0',
  batch: { kind: 'paste' },
  command: 'apply_adjustments_to_paths',
  args: { adjustments: { exposure: 1 }, paste: { source: '/job/Capture/x.ARW' }, style: null },
  startedMs: 1,
  total: 4,
  applied: 2,
  undo: [
    { path: '/job/Capture/a.ARW', before: { exposure: 0 } },
    { path: '/job/Capture/b.ARW', before: null },
  ],
  rest: ['/job/Capture/c.ARW', '/job/Capture/d.ARW'],
};

function answer(batches: InterruptedBatch[], calls: [string, unknown][]) {
  vi.mocked(invoke).mockImplementation(async (command: string, args?: unknown) => {
    calls.push([command, args]);
    if (command === 'interrupted_batches') return batches;
    if (command === 'apply_adjustments_to_paths' || command === 'replace_recipes') return { changes: [] };
    return null;
  });
}

const entry = () => useActivityStore.getState().entries.find((e) => e.id.startsWith('batch:interrupted:'));
const action = (label: string) => entry()!.actions!.find((a) => a.label === label)!;

beforeEach(() => {
  vi.clearAllMocks();
  resetInterruptedBatches();
  useActivityStore.setState({ entries: [] });
  useLibraryStore.setState({ sessionDirectory: '/job' });
});

describe('interrupted batches', () => {
  it('says how far a batch got, under Needs attention, once per run', async () => {
    const calls: [string, unknown][] = [];
    answer([paste], calls);
    await offerInterruptedBatches('/job');
    expect(calls[0]).toEqual(['interrupted_batches', { directory: '/job' }]);
    expect(entry()?.status).toBe('warning');
    expect(entry()?.title).toBe('Interrupted: Paste Settings applied to 2 of 4 photos');
    expect(entry()?.actions?.map((a) => a.label)).toEqual(['Apply to the rest', 'Undo the applied part', 'Dismiss']);
    useActivityStore.setState({ entries: [] });
    await offerInterruptedBatches('/job');
    expect(entry()).toBeUndefined();
  });

  it('applies the batch to the photographs it did not reach, then forgets it', async () => {
    const calls: [string, unknown][] = [];
    answer([paste], calls);
    const refresh = vi.fn();
    await offerInterruptedBatches('/job', refresh);
    await action('Apply to the rest').run();
    expect(calls).toContainEqual(['apply_adjustments_to_paths', { ...paste.args, paths: paste.rest }]);
    expect(calls.at(-1)).toEqual(['dismiss_interrupted_batch', { id: paste.id }]);
    expect(refresh).toHaveBeenCalled();
  });

  it('writes back the recipes it changed', async () => {
    const calls: [string, unknown][] = [];
    answer([paste], calls);
    await offerInterruptedBatches('/job');
    await action('Undo the applied part').run();
    expect(calls).toContainEqual([
      'replace_recipes',
      {
        changes: [
          { path: '/job/Capture/a.ARW', adjustments: { exposure: 0 } },
          { path: '/job/Capture/b.ARW', adjustments: null },
        ],
      },
    ]);
    expect(calls.at(-1)).toEqual(['dismiss_interrupted_batch', { id: paste.id }]);
  });

  it('gives each photograph its rating back, grouped by value', async () => {
    const calls: [string, unknown][] = [];
    const rating: InterruptedBatch = {
      ...paste,
      id: 'run-1-1',
      batch: { kind: 'rating', rating: 4 },
      command: 'set_rating_for_paths',
      args: { rating: 4, directory: '/job' },
      undo: [
        { path: '/a', before: 1 },
        { path: '/b', before: 0 },
        { path: '/c', before: 1 },
      ],
    };
    answer([rating], calls);
    await offerInterruptedBatches('/job');
    expect(entry()?.title).toBe('Interrupted: Rating 4 applied to 2 of 4 photos');
    await action('Undo the applied part').run();
    const rated = calls.filter(([command]) => command === 'set_rating_for_paths').map(([, args]) => args);
    expect(rated).toEqual([
      { paths: ['/a', '/c'], rating: 1, directory: '/job' },
      { paths: ['/b'], rating: 0, directory: '/job' },
    ]);
  });

  it('keeps the batch on offer when its repair fails', async () => {
    const calls: [string, unknown][] = [];
    answer([paste], calls);
    await offerInterruptedBatches('/job');
    vi.mocked(invoke).mockImplementation(async (command: string) => {
      calls.push([command, null]);
      if (command === 'apply_adjustments_to_paths') return { changes: [], failure: { failed: ['/job/Capture/c.ARW'] } };
      return null;
    });
    await action('Apply to the rest').run();
    expect(calls.map(([command]) => command)).not.toContain('dismiss_interrupted_batch');
    expect(entry()?.status).toBe('warning');
  });

  it('forgets the batch when its entry is closed, as Dismiss does', async () => {
    const calls: [string, unknown][] = [];
    answer([paste], calls);
    await offerInterruptedBatches('/job');
    useActivityStore.getState().dismiss(entry()!.id);
    await vi.waitFor(() => expect(calls.at(-1)).toEqual(['dismiss_interrupted_batch', { id: paste.id }]));
    expect(entry()).toBeUndefined();
  });

  it('forgets a batch that reached every photograph before the kill', async () => {
    const calls: [string, unknown][] = [];
    answer([{ ...paste, rest: [], applied: 4 }], calls);
    await offerInterruptedBatches('/job');
    expect(entry()).toBeUndefined();
    expect(calls).toContainEqual(['dismiss_interrupted_batch', { id: paste.id }]);
  });

  it('offers nothing of a library that is no longer open', async () => {
    const calls: [string, unknown][] = [];
    answer([paste], calls);
    const listed = offerInterruptedBatches('/job');
    useLibraryStore.setState({ sessionDirectory: '/other' });
    await listed;
    expect(entry()).toBeUndefined();
  });
});
