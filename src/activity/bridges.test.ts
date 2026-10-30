import { act } from '@testing-library/react';
import { afterEach, beforeEach, expect, it, vi } from 'vitest';
import { invoke } from '@tauri-apps/api/core';
import { listen } from '@tauri-apps/api/event';
import { noteWriteError, resetActivityBridges, startActivityBridges } from './bridges';
import { resetActivityStore, useActivityStore } from '../store/useActivityStore';
import { useEditorStore } from '../store/useEditorStore';
import { useProcessStore, type ExportJob } from '../store/useProcessStore';
import { Invokes } from '../components/ui/AppProperties';
import { submitPhotoWrite } from '../utils/photoWrites';
import '../i18n';

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
  ...part,
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
    progress: { done: 1, total: 3 },
  });
  await entry('export:j1')!.cancel!();
  expect(invoke).toHaveBeenCalledWith(Invokes.ExportQueueCancel, { job: 'j1' });
});

it('keeps an export failure under Needs attention with Try again and the photographs, until dismissed', async () => {
  useProcessStore.getState().setExportQueue({ jobs: [job({})], held: false });
  const failures = [{ path: '/s/A.NEF', error: 'disk full' }];
  useProcessStore.getState().setExportQueue({ jobs: [job({ state: 'failed', done: 2, failed: 1, failures })], held: false });
  const failure = entry('export:j1')!;
  expect(failure).toMatchObject({ status: 'failure', title: '1 photo did not export: 3 photos as JPG', paths: ['/s/A.NEF'] });
  expect(failure.detail).toBe('A.NEF: disk full');
  expect(failure.actions?.map((a) => a.label)).toEqual(['Try again']);
  await failure.actions![0].run();
  expect(invoke).toHaveBeenCalledWith(Invokes.ExportQueueResume, { job: 'j1' });

  // Dismissed: the same snapshot again does not bring it back.
  useActivityStore.getState().dismiss('export:j1');
  useProcessStore.getState().setExportQueue({ jobs: [job({ state: 'failed', done: 2, failed: 1, failures })], held: true });
  expect(entry('export:j1')).toBeUndefined();
});

it('ends the task of an export that finished well (its files are reported once with Show in Finder)', () => {
  useProcessStore.getState().setExportQueue({ jobs: [job({})], held: false });
  useProcessStore.getState().setExportQueue({ jobs: [job({ state: 'done', done: 3 })], held: false });
  expect(entry('export:j1')).toBeUndefined();
});

it('reports a photo write that failed until the photograph is written after all', async () => {
  await submitPhotoWrite(['/a.dng', '/b.dng'], async () => ['/b.dng'], { kind: 'paste', failedOf: (failed) => failed });
  await act(async () => {});
  expect(entry('write:not-saved')).toMatchObject({
    status: 'failure',
    source: 'write',
    title: 'Changes to 1 photo were not saved',
    paths: ['/b.dng'],
  });
  await submitPhotoWrite(['/b.dng'], async () => [], { kind: 'recipe', failedOf: (failed: string[]) => failed });
  await act(async () => {});
  expect(entry('write:not-saved')).toBeUndefined();
});

it('gives the write failure the reason a caller learns after the queue settled, as one entry', async () => {
  await submitPhotoWrite(['/a.dng'], async () => ['/a.dng'], { kind: 'rating', failedOf: (failed) => failed });
  await act(async () => {});
  noteWriteError('Failed to apply rating: Permission denied', ['/a.dng']);
  const failures = useActivityStore.getState().entries.filter((e) => e.status === 'failure');
  expect(failures).toHaveLength(1);
  expect(failures[0].detail).toContain('Failed to apply rating: Permission denied');
  // A reason for photographs the queue did not report is its own entry.
  noteWriteError('Failed to set color label: disk full', ['/z.dng']);
  expect(useActivityStore.getState().entries.filter((e) => e.status === 'failure')).toHaveLength(2);
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

it('shows a model download with its progress', async () => {
  await act(async () => {});
  handlers.get('ai-model-download-start')!({ payload: 'sky-v2' });
  handlers.get('ai-model-download-progress')!({ payload: { model: 'sky-v2', received: 50, total: 200 } });
  expect(entry('download:ai-model')).toMatchObject({
    source: 'download',
    title: 'Downloading sky-v2',
    progress: { done: 50, total: 200 },
  });
  handlers.get('ai-model-download-finish')!({ payload: 'sky-v2' });
  expect(entry('download:ai-model')).toBeUndefined();
});

it('shows a batch command photo by photo', async () => {
  let release!: () => void;
  const written = submitPhotoWrite(
    ['/a.dng', '/b.dng'],
    () => new Promise<string[]>((resolve) => (release = () => resolve([]))),
    { kind: 'auto', failedOf: (failed) => failed, batch: true },
  );
  await act(async () => {});
  const batch = useActivityStore.getState().entries.find((e) => e.source === 'batch')!;
  expect(batch).toMatchObject({ status: 'running', title: 'Auto adjustments: 2 photos', progress: { done: 0, total: 2 } });
  release();
  await written;
  await act(async () => {});
  expect(useActivityStore.getState().entries.some((e) => e.source === 'batch')).toBe(false);
});
