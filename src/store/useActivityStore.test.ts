import { afterEach, beforeEach, expect, it, vi } from 'vitest';
import {
  activityDetails,
  activitySections,
  DONE_KEPT,
  DONE_KEPT_MS,
  resetActivityStore,
  useActivityStore,
} from './useActivityStore';

const store = () => useActivityStore.getState();
const entry = (id: string) => store().entries.find((e) => e.id === id);

beforeEach(() => {
  vi.useFakeTimers();
  resetActivityStore();
});
afterEach(() => {
  resetActivityStore();
  vi.useRealTimers();
});

it('keeps a failure until it is resolved or dismissed, however long', () => {
  store().report('failure', { id: 'f', source: 'export', title: '2 photos did not export' });
  store().report('warning', { id: 'w', source: 'xmp', title: '3 XMP not updated' });
  vi.advanceTimersByTime(DONE_KEPT_MS * 10);
  store().expire();
  expect(entry('f')?.status).toBe('failure');
  expect(entry('w')?.status).toBe('warning');
  store().dismiss('f');
  expect(entry('f')).toBeUndefined();
  store().resolve('w');
  expect(entry('w')).toBeUndefined();
});

it('lets results expire after their time under Done', () => {
  store().report('done', { id: 'r', source: 'app', title: 'Renamed 3 photos' });
  vi.advanceTimersByTime(DONE_KEPT_MS - 1000);
  expect(entry('r')).toBeTruthy();
  // The store's own timer drops it.
  vi.advanceTimersByTime(2000);
  expect(entry('r')).toBeUndefined();
});

it('keeps at most DONE_KEPT results, dropping the oldest', () => {
  for (let i = 0; i <= DONE_KEPT; i++) {
    vi.advanceTimersByTime(1);
    store().report('done', { id: `r${i}`, source: 'app', title: `${i}` });
  }
  expect(activitySections(store().entries).done).toHaveLength(DONE_KEPT);
  expect(entry('r0')).toBeUndefined();
  expect(entry(`r${DONE_KEPT}`)).toBeTruthy();
});

it('runs a task with progress and Cancel; it cannot be dismissed, only cancelled or finished', () => {
  const cancel = vi.fn();
  const id = store().start({ source: 'export', title: 'Exporting', progress: { done: 0, total: 4 }, cancel });
  store().update(id, { progress: { done: 2, total: 4 } });
  expect(entry(id)).toMatchObject({ status: 'running', progress: { done: 2, total: 4 } });
  store().dismiss(id);
  expect(entry(id)).toBeTruthy();
  entry(id)!.cancel!();
  expect(cancel).toHaveBeenCalledOnce();
  // Finished without a result: it goes; with one, it is under Done.
  store().finish(id);
  expect(entry(id)).toBeUndefined();
  const other = store().start({ source: 'import', title: 'Importing' });
  store().finish(other, { title: 'Imported 12 photos' });
  expect(entry(other)).toMatchObject({ status: 'done', source: 'import', title: 'Imported 12 photos' });
});

it('replaces an entry reported again under its id, keeping when it started', () => {
  vi.setSystemTime(1000);
  store().start({ id: 'export:j', source: 'export', title: 'Exporting' });
  vi.setSystemTime(5000);
  store().report('failure', { id: 'export:j', source: 'export', title: '1 photo did not export' });
  expect(store().entries).toHaveLength(1);
  expect(entry('export:j')).toMatchObject({ status: 'failure', createdAt: 1000, updatedAt: 5000 });
});

it('sorts the sections: failures before warnings, running oldest first, done newest first', () => {
  vi.setSystemTime(1);
  store().report('warning', { id: 'w', source: 'app', title: 'w' });
  vi.setSystemTime(2);
  store().report('failure', { id: 'f', source: 'app', title: 'f' });
  store().start({ id: 'a', source: 'app', title: 'a' });
  vi.setSystemTime(3);
  store().start({ id: 'b', source: 'app', title: 'b' });
  store().report('done', { id: 'd1', source: 'app', title: 'd1' });
  vi.setSystemTime(4);
  store().report('done', { id: 'd2', source: 'app', title: 'd2' });
  const { attention, running, done } = activitySections(store().entries);
  expect(attention.map((e) => e.id)).toEqual(['f', 'w']);
  expect(running.map((e) => e.id)).toEqual(['a', 'b']);
  expect(done.map((e) => e.id)).toEqual(['d2', 'd1']);
});

it('copies the title, detail and the photographs it is about', () => {
  const id = store().report('failure', {
    source: 'write',
    title: 'Changes to 1 photo were not saved',
    detail: 'disk full',
    paths: ['/a/B.dng?vc=2'],
  });
  expect(activityDetails(entry(id)!)).toBe('Changes to 1 photo were not saved\ndisk full\n\n/a/B.dng');
});
