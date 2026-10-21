import { beforeEach, expect, it } from 'vitest';
import { currentExportEventSeq, nextExportRead, useProcessStore, type ExportItem } from './useProcessStore';

const photo = (index: number, state: ExportItem['state']): ExportItem => ({
  index,
  path: `/p/${index}.RAF`,
  state,
  output: null,
  error: null,
  attempts: 0,
  startedMs: null,
  finishedMs: null,
});
const event = (item: ExportItem) =>
  useProcessStore.getState().applyExportItemProgress({ job: 'j', item, stage: null, fraction: null });
const states = () => useProcessStore.getState().exportItems.j.map((item) => item.state);

beforeEach(() => {
  // Removed jobs drop what the store keeps about their reads.
  useProcessStore.getState().setExportQueue({ jobs: [], held: false });
  useProcessStore.setState({ exportItems: {} });
});

it('keeps photos an event changed while a read of the list was on its way', () => {
  useProcessStore.getState().setExportItems('j', [photo(0, 'pending'), photo(1, 'pending')]);
  // A read is asked for; photo 0 finishes before its reply arrives.
  const since = currentExportEventSeq();
  event(photo(0, 'done'));
  useProcessStore.getState().setExportItems('j', [photo(0, 'running'), photo(1, 'pending')], since);
  expect(states()).toEqual(['done', 'pending']);
  // A read asked after the event takes the queue's word.
  useProcessStore.getState().setExportItems('j', [photo(0, 'done'), photo(1, 'running')], currentExportEventSeq());
  expect(states()).toEqual(['done', 'running']);
});

it("merges events that came before a job's first read only when they are newer", () => {
  event(photo(0, 'running'));
  const since = currentExportEventSeq();
  event(photo(1, 'done'));
  expect(useProcessStore.getState().exportItems.j).toBeUndefined();
  useProcessStore.getState().setExportItems('j', [photo(0, 'done'), photo(1, 'running')], since);
  // Photo 0's event is older than the read; photo 1's is newer.
  expect(states()).toEqual(['done', 'done']);
});

it('drops the reply of an older read that arrives after a newer one', () => {
  useProcessStore.getState().setExportItems('j', [photo(0, 'pending')]);
  const [first, firstRead] = [currentExportEventSeq(), nextExportRead()];
  event(photo(0, 'running'));
  event(photo(0, 'done'));
  const [second, secondRead] = [currentExportEventSeq(), nextExportRead()];
  useProcessStore.getState().setExportItems('j', [photo(0, 'done')], second, secondRead);
  useProcessStore.getState().setExportItems('j', [photo(0, 'running')], first, firstRead);
  expect(states()).toEqual(['done']);
});

it('orders replies by their reads when no event came between them', () => {
  useProcessStore.getState().setExportItems('j', [photo(0, 'running')]);
  // A cancel puts the photo back to waiting without a photo event.
  const since = currentExportEventSeq();
  const older = nextExportRead();
  const newer = nextExportRead();
  useProcessStore.getState().setExportItems('j', [photo(0, 'pending')], since, newer);
  useProcessStore.getState().setExportItems('j', [photo(0, 'running')], since, older);
  expect(states()).toEqual(['pending']);
});
