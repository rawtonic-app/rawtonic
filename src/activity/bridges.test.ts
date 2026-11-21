import { act } from '@testing-library/react';
import { afterEach, beforeEach, expect, it, vi } from 'vitest';
import { invoke } from '@tauri-apps/api/core';
import { listen } from '@tauri-apps/api/event';
import { noteWriteError, registerPhotoReload, resetActivityBridges, startActivityBridges } from './bridges';
import { resetActivityStore, useActivityStore } from '../store/useActivityStore';
import { useEditorStore } from '../store/useEditorStore';
import { useProcessStore, type ExportJob } from '../store/useProcessStore';
import { Invokes } from '../components/ui/AppProperties';
import { submitPhotoWrite } from '../utils/photoWrites';
import { followRenamedPaths } from '../utils/pathKeyedState';
import { useDuplicatesDialog } from '../utils/duplicates';
import '../i18n';
import i18n from 'i18next';

vi.mock('@tauri-apps/api/core', () => ({ invoke: vi.fn(async () => undefined) }));
vi.mock('@tauri-apps/api/event', () => ({ listen: vi.fn() }));

const handlers = new Map<string, (event: { payload: unknown }) => void>();
const entry = (id: string) => useActivityStore.getState().entries.find((e) => e.id === id);
const job = (part: Partial<ExportJob>): ExportJob => ({
  id: 'j1',
  label: '3 photos as JPG',
  state: 'running',
  createdAt: '',
  total: 3,
  done: 0,
  failed: 0,
  failures: [],
  ...part
});
let stop: () => void;

beforeEach(() => {
  resetActivityStore();
  resetActivityBridges();
  handlers.clear();
  vi.mocked(listen).mockImplementation(async (event, handler) => {
    handlers.set(event, handler as (event: { payload: unknown }) => void);
    return () => handlers.delete(event);
  });
  useProcessStore.setState({ exportQueue: [], exportQueueHeld: false, activeAiTasks: {} });
  stop = startActivityBridges();
});
afterEach(() => {
  stop();
  vi.clearAllMocks();
});

it('shows a running export with its progress and Cancel', async () => {
  useProcessStore.getState().setExportQueue({ jobs: [job({ done: 1 })], held: false });
  expect(entry('export:j1')).toMatchObject({
    status: 'running',
    source: 'export',
    title: 'Exporting 3 photos as JPG',
    progress: { done: 1, total: 3 }
  });
  await entry('export:j1')!.cancel!();
  expect(invoke).toHaveBeenCalledWith(Invokes.ExportQueueCancel, { job: 'j1' });
});

it('keeps an export failure under Needs attention with Try again and the photographs, until dismissed', async () => {
  useProcessStore.getState().setExportQueue({ jobs: [job({})], held: false });
  const failures = [{ path: '/s/A.NEF', error: 'disk full' }];
  useProcessStore
    .getState()
    .setExportQueue({ jobs: [job({ state: 'failed', done: 2, failed: 1, failures })], held: false });
  const failure = entry('export:j1')!;
  expect(failure).toMatchObject({
    status: 'failure',
    title: '1 photo did not export: 3 photos as JPG',
    paths: ['/s/A.NEF']
  });
  expect(failure.detail).toBe('A.NEF: disk full');
  expect(failure.actions?.map((a) => a.label)).toEqual(['Try again']);
  await failure.actions![0].run();
  expect(invoke).toHaveBeenCalledWith(Invokes.ExportQueueResume, { job: 'j1' });

  // Dismissed: the same snapshot again does not bring it back.
  useActivityStore.getState().dismiss('export:j1');
  useProcessStore
    .getState()
    .setExportQueue({ jobs: [job({ state: 'failed', done: 2, failed: 1, failures })], held: true });
  expect(entry('export:j1')).toBeUndefined();
});

it('says an export interrupted at the last quit in the interface language, though reported before it was applied', async () => {
  // At launch the queue's first snapshot arrives while the page is still in
  // English; the macOS language follows (MSMOKE1: German UI, English entry).
  useProcessStore.getState().setExportQueue({ jobs: [job({ state: 'interrupted', done: 1 })], held: true });
  expect(entry('export:j1')).toMatchObject({ status: 'warning', title: 'Export interrupted: 3 photos as JPG' });
  try {
    await i18n.changeLanguage('de');
    const shown = entry('export:j1')!;
    expect(shown.title).toBe('Export unterbrochen: 3 photos as JPG');
    expect(shown.actions?.map((a) => a.label)).toEqual(['Fortsetzen']);

    // Dismissed, it stays dismissed when the language changes again.
    useActivityStore.getState().dismiss('export:j1');
    await i18n.changeLanguage('en');
    expect(entry('export:j1')).toBeUndefined();
  } finally {
    await i18n.changeLanguage('en');
  }
});

it('offers no Try again for a failed Edit With render: it is never resumed', () => {
  const failures = [{ path: '/s/A.NEF', error: 'disk full' }];
  useProcessStore
    .getState()
    .setExportQueue({ jobs: [job({ state: 'failed', total: 1, failed: 1, failures })], held: false, editWith: ['j1'] });
  expect(useProcessStore.getState().exportQueue[0].editWith).toBe(true);
  expect(entry('export:j1')).toMatchObject({ status: 'failure', paths: ['/s/A.NEF'] });
  expect(entry('export:j1')!.actions).toBeUndefined();
});

it('ends the task of an export that finished well (its files are reported once with Show in Finder)', () => {
  useProcessStore.getState().setExportQueue({ jobs: [job({})], held: false });
  useProcessStore.getState().setExportQueue({ jobs: [job({ state: 'done', done: 3 })], held: false });
  expect(entry('export:j1')).toBeUndefined();
});

it('keeps a failed paste until dismissed: a later paste of other values says nothing about it', async () => {
  await submitPhotoWrite(['/a.dng', '/b.dng'], async () => ['/b.dng'], { kind: 'paste', failedOf: (failed) => failed });
  await act(async () => {});
  expect(entry('write:not-saved')).toMatchObject({
    status: 'failure',
    source: 'write',
    title: 'Changes to 1 photo were not saved',
    paths: ['/b.dng']
  });
  await submitPhotoWrite(['/b.dng'], async () => [], { kind: 'paste', failedOf: (failed: string[]) => failed });
  await submitPhotoWrite(['/b.dng'], async () => [], { kind: 'recipe', failedOf: (failed: string[]) => failed });
  await act(async () => {});
  expect(entry('write:not-saved')).toMatchObject({ paths: ['/b.dng'] });
  useActivityStore.getState().dismiss('write:not-saved');
  expect(entry('write:not-saved')).toBeUndefined();
});

it('resolves a failed rating or edit once a later one of the same kind is saved, also after a rename', async () => {
  await submitPhotoWrite(['/a.dng'], async () => ['/a.dng'], {
    kind: Invokes.SetRatingForPaths,
    failedOf: (failed) => failed
  });
  await submitPhotoWrite(['/c.dng'], async () => ['/c.dng'], { kind: 'recipe', failedOf: (failed) => failed });
  await act(async () => {});
  expect(entry('write:not-saved')?.paths).toEqual(['/a.dng', '/c.dng']);
  // A rating saved later says nothing about the edit not saved.
  await submitPhotoWrite(['/c.dng'], async () => [], {
    kind: Invokes.SetRatingForPaths,
    failedOf: (failed: string[]) => failed
  });
  await act(async () => {});
  expect(entry('write:not-saved')?.paths).toEqual(['/a.dng', '/c.dng']);
  followRenamedPaths(['/a.dng'], ['/Wedding-001.dng']);
  expect(entry('write:not-saved')?.paths?.slice().sort()).toEqual(['/Wedding-001.dng', '/c.dng']);
  await submitPhotoWrite(['/Wedding-001.dng'], async () => [], {
    kind: Invokes.SetRatingForPaths,
    failedOf: (failed: string[]) => failed
  });
  await submitPhotoWrite(['/c.dng'], async () => [], { kind: 'recipe', failedOf: (failed: string[]) => failed });
  await act(async () => {});
  expect(entry('write:not-saved')).toBeUndefined();
});

it('gives the write failure the reason a caller learns after the queue settled, as one entry', async () => {
  await submitPhotoWrite(['/a.dng'], async () => ['/a.dng'], {
    kind: Invokes.SetRatingForPaths,
    failedOf: (failed) => failed
  });
  await act(async () => {});
  noteWriteError('Failed to apply rating: Permission denied', ['/a.dng']);
  const failures = useActivityStore.getState().entries.filter((e) => e.status === 'failure');
  expect(failures).toHaveLength(1);
  expect(failures[0].detail).toContain('Failed to apply rating: Permission denied');
  // A reason for photographs the queue did not report is its own entry.
  noteWriteError('Failed to set color label: disk full', ['/z.dng']);
  expect(useActivityStore.getState().entries.filter((e) => e.status === 'failure')).toHaveLength(2);
});

it("offers Reload for a save refused because the open photo's file was replaced", async () => {
  const reload = vi.fn();
  const unregister = registerPhotoReload(reload);
  noteWriteError(
    "Failed to save changes: This photo's file was replaced on disk since you opened it. Reload to see the new file with your edit applied."
  );
  await submitPhotoWrite(['/a.dng'], async () => ['/a.dng'], { kind: 'recipe', failedOf: (failed) => failed });
  await act(async () => {});
  const failed = useActivityStore.getState().entries.find((e) => e.status === 'failure')!;
  expect(failed.actions).toHaveLength(1);
  failed.actions![0].run();
  expect(reload).toHaveBeenCalledWith('/a.dng');
  unregister();
});

it('offers no Reload for other save failures', async () => {
  registerPhotoReload(vi.fn());
  noteWriteError('Failed to save changes: disk full');
  await submitPhotoWrite(['/b.dng'], async () => ['/b.dng'], { kind: 'recipe', failedOf: (failed) => failed });
  await act(async () => {});
  expect(useActivityStore.getState().entries.find((e) => e.status === 'failure')!.actions).toBeUndefined();
});

it('shows AI tasks while they run, with Generative fill progress', async () => {
  await act(async () => {});
  useProcessStore.getState().registerAiTask('mask-1', 'sky');
  useProcessStore.getState().registerAiTask('patch-1', 'inpaint');
  expect(entry('ai:task:mask-1')).toMatchObject({ status: 'running', source: 'ai', title: 'AI mask: Sky' });
  handlers.get('ai-fill-progress')!({ payload: { taskId: 'patch-1', done: 2, total: 8 } });
  expect(entry('ai:task:patch-1')).toMatchObject({ title: 'Generative fill', progress: { done: 2, total: 8 } });
  useProcessStore.getState().unregisterAiTask('mask-1');
  expect(entry('ai:task:mask-1')).toBeUndefined();

  useEditorStore.setState({ aiDenoiseBusy: true });
  expect(entry('ai:denoise')).toMatchObject({ status: 'running', title: 'AI Denoise' });
  useEditorStore.setState({ aiDenoiseBusy: false });
  expect(entry('ai:denoise')).toBeUndefined();
});

it('shows the AI selections of closed photos and names each photo they failed for', async () => {
  await act(async () => {});
  const progress = (part: object) =>
    handlers.get('ai-selections-progress')!({
      payload: { total: 3, done: 0, current: '/s/A.NEF', failed: [], finished: false, ...part }
    });
  progress({});
  expect(entry('ai:selections')).toMatchObject({
    status: 'running',
    source: 'ai',
    title: 'Computing AI selections: 0 of 3 photos',
    progress: { done: 0, total: 3 }
  });
  progress({ done: 2, current: '/s/C.NEF' });
  expect(entry('ai:selections')).toMatchObject({ progress: { done: 2, total: 3 } });
  const failed = [
    { path: '/s/B.NEF', message: 'layer “Skin”: its People selection: there is no person 2 in this photo' }
  ];
  progress({ done: 3, current: null, failed, finished: true });
  expect(entry('ai:selections')).toBeUndefined();
  const report = useActivityStore.getState().entries.find((e) => e.id.startsWith('ai:selections:failed'))!;
  expect(report).toMatchObject({
    status: 'warning',
    title: 'AI selections not computed for 1 of 3 photos',
    paths: ['/s/B.NEF']
  });
  expect(report.detail).toBe('B.NEF: layer “Skin”: its People selection: there is no person 2 in this photo');

  // All computed: a result under Done.
  progress({ total: 2, done: 0 });
  progress({ total: 2, done: 2, current: null, finished: true });
  expect(entry('ai:selections')).toMatchObject({
    status: 'done',
    title: 'AI selections computed for closed photos (2)'
  });
});

it('shows a model download with its progress', async () => {
  await act(async () => {});
  handlers.get('ai-model-download-start')!({ payload: 'sky-v2' });
  handlers.get('ai-model-download-progress')!({ payload: { model: 'sky-v2', received: 50, total: 200 } });
  expect(entry('download:ai-model')).toMatchObject({
    source: 'download',
    title: 'Downloading sky-v2',
    progress: { done: 50, total: 200 }
  });
  handlers.get('ai-model-download-finish')!({ payload: 'sky-v2' });
  expect(entry('download:ai-model')).toBeUndefined();
});

it("titles a model download in the UI's language, not the backend's English name", async () => {
  await act(async () => {});
  handlers.get('ai-model-download-start')!({ payload: 'Eye and teeth model' });
  expect(entry('download:ai-model')?.title).toBe('Downloading eye and teeth model');
  handlers.get('ai-model-download-finish')!({ payload: 'Eye and teeth model' });
  await i18n.changeLanguage('de');
  try {
    handlers.get('ai-model-download-start')!({ payload: 'Eye and teeth model' });
    expect(entry('download:ai-model')?.title).toBe('Wird geladen: Augen- und Zahnmodell');
    handlers.get('ai-model-download-finish')!({ payload: 'Eye and teeth model' });
    handlers.get('ai-model-download-start')!({ payload: 'Face models' });
    expect(entry('download:ai-model')?.title).toBe('Wird geladen: Gesichtsmodelle');
  } finally {
    await i18n.changeLanguage('en');
  }
});

it('shows a batch command photo by photo', async () => {
  let release!: () => void;
  const written = submitPhotoWrite(
    ['/a.dng', '/b.dng'],
    () => new Promise<string[]>((resolve) => (release = () => resolve([]))),
    { kind: 'auto', failedOf: (failed) => failed, batch: true }
  );
  await act(async () => {});
  const batch = useActivityStore.getState().entries.find((e) => e.source === 'batch')!;
  expect(batch).toMatchObject({
    status: 'running',
    title: 'Auto adjustments: 2 photos',
    progress: { done: 0, total: 2 }
  });
  release();
  await written;
  await act(async () => {});
  expect(useActivityStore.getState().entries.some((e) => e.source === 'batch')).toBe(false);
});

it('shows the duplicate comparison with Stop, then what it read with Review', async () => {
  await act(async () => {});
  handlers.get('duplicates-progress')!({ payload: { run: 4, directory: '/jobs/A', done: 10, total: 40 } });
  expect(entry('duplicates:4')).toMatchObject({
    status: 'running',
    title: 'Comparing photos for duplicates',
    progress: { done: 10, total: 40 }
  });
  await entry('duplicates:4')!.cancel!();
  expect(invoke).toHaveBeenCalledWith(Invokes.DuplicatesCancel, { run: 4 });
  handlers.get('duplicates-finished')!({
    payload: {
      run: 4,
      directory: '/jobs/A',
      photos: 40,
      hashed: 39,
      unreadable: ['/jobs/A/x.NEF'],
      cancelled: false,
      error: null,
      seconds: 1
    }
  });
  expect(entry('duplicates:4')).toMatchObject({ status: 'done', title: 'Compared 39 photos for duplicates' });
  expect(entry('duplicates:4:unreadable')).toMatchObject({
    status: 'warning',
    title: '1 photo could not be compared for duplicates',
    paths: ['/jobs/A/x.NEF']
  });
  entry('duplicates:4')!.actions![0].run();
  expect(useDuplicatesDialog.getState().open).toBe(true);
  useDuplicatesDialog.getState().close();
});

it('reports a failed duplicate comparison until dismissed', async () => {
  await act(async () => {});
  handlers.get('duplicates-progress')!({ payload: { run: 5, directory: '/jobs/A', done: 0, total: 3 } });
  handlers.get('duplicates-finished')!({
    payload: {
      run: 5,
      directory: '/jobs/A',
      photos: 0,
      hashed: 0,
      unreadable: [],
      cancelled: false,
      error: 'disk gone',
      seconds: 0
    }
  });
  expect(entry('duplicates:5')).toMatchObject({
    status: 'failure',
    title: 'Finding duplicates failed',
    detail: 'disk gone'
  });
});

it('says once what an interrupted job left and was removed, and names staging folders left in place', async () => {
  await vi.waitFor(() => expect(handlers.has('leftovers-removed')).toBe(true));
  act(() =>
    handlers.get('leftovers-removed')!({
      payload: { tempFiles: 2, folders: 1, bytes: 2_400_000_000, kept: ['/cat/Photos/.rawtonic-import-old'] }
    })
  );
  expect(entry('leftovers:removed')).toMatchObject({
    status: 'done',
    source: 'files',
    title: 'Removed 2.4 GB of unfinished copies from interrupted jobs'
  });
  expect(entry('leftovers:kept')).toMatchObject({
    status: 'warning',
    title: '1 unfinished folder of an interrupted job was left in place',
    detail: '/cat/Photos/.rawtonic-import-old'
  });
});

it('says once, in the activity panel, that photos registered twice were merged and where the catalog was saved', async () => {
  await vi.waitFor(() => expect(handlers.has('library-duplicates-merged')).toBe(true));
  act(() =>
    handlers.get('library-duplicates-merged')!({
      payload: {
        files: [
          { path: '/Shoot/Canon_R6.CR3', fields: ['flag', 'title'] },
          { path: '/Shoot/Nikon_Z6.NEF', fields: [] }
        ],
        backup: '/cat/catalog.sqlite.before-duplicate-repair-20261114'
      }
    })
  );
  expect(entry('library:duplicates-merged')).toMatchObject({
    status: 'done',
    source: 'files',
    title: '2 photos were registered twice and have been merged — earlier edits are kept as snapshots',
    detail:
      'Canon_R6.CR3, Nikon_Z6.NEF\nEach file is one photo again; no photo file was changed. The catalog as it was is saved as catalog.sqlite.before-duplicate-repair-20261114.\nCanon_R6.CR3: flag, title taken from its other record',
    paths: ['/Shoot/Canon_R6.CR3', '/Shoot/Nikon_Z6.NEF']
  });
});

it('names every merged photograph, not only the first few', async () => {
  await vi.waitFor(() => expect(handlers.has('library-duplicates-merged')).toBe(true));
  const files = Array.from({ length: 12 }, (_, i) => ({
    path: `/Shoot/IMG_${i}.CR3`,
    fields: i === 0 ? ['flag'] : []
  }));
  act(() => handlers.get('library-duplicates-merged')!({ payload: { files, backup: null } }));
  const detail = entry('library:duplicates-merged')!.detail!;
  // The names come first (the panel shows only a few lines); all are there.
  expect(detail.split('\n')[0]).toBe(files.map((f) => f.path.split('/').pop()).join(', '));
  expect(entry('library:duplicates-merged')!.paths).toHaveLength(12);
});

it('says once, not as a dialog, that the opening scan left copies of catalog photographs out', async () => {
  await vi.waitFor(() => expect(handlers.has('library-duplicates-skipped')).toBe(true));
  const send = () =>
    act(() =>
      handlers.get('library-duplicates-skipped')!({
        payload: { files: [{ path: '/cat/Photos/Canon_R6_copy.CR3', of: '/cat/Photos/Canon_R6.CR3' }] }
      })
    );
  send();
  send();
  expect(useActivityStore.getState().entries.filter((e) => e.id === 'library:duplicates-skipped')).toHaveLength(1);
  expect(entry('library:duplicates-skipped')).toMatchObject({
    status: 'done',
    title: '1 duplicate in the catalog’s folder was not added — already in the catalog as Canon_R6.CR3',
    paths: ['/cat/Photos/Canon_R6.CR3']
  });
  expect(entry('library:duplicates-skipped')?.detail).toContain('Canon_R6_copy.CR3 = Canon_R6.CR3');
});

it('shows the duplicates report of a scan that ran before the panel listened', async () => {
  vi.mocked(invoke).mockImplementation(async (command) =>
    command === Invokes.SessionMissedDuplicates
      ? { files: [{ path: '/cat/Photos/Nikon_Z6_copy.NEF', of: '/cat/Photos/Nikon_Z6.NEF' }] }
      : undefined
  );
  stop();
  resetActivityStore();
  stop = startActivityBridges();
  await vi.waitFor(() => expect(entry('library:duplicates-skipped')).toBeDefined());
  expect(entry('library:duplicates-skipped')?.detail).toContain('Nikon_Z6_copy.NEF = Nikon_Z6.NEF');
});

it('shows the merge report of a library that opened before the panel listened', async () => {
  vi.mocked(invoke).mockImplementation(async (command) =>
    command === Invokes.SessionMissedMergedDuplicates
      ? { files: [{ path: '/Shoot/Canon_R6.CR3', fields: [] }], backup: null }
      : undefined
  );
  stop();
  resetActivityStore();
  stop = startActivityBridges();
  await vi.waitFor(() => expect(entry('library:duplicates-merged')).toBeDefined());
  expect(entry('library:duplicates-merged')?.detail).toContain('Canon_R6.CR3');
});
