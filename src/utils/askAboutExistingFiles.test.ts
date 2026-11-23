import { afterEach, beforeEach, expect, it, vi } from 'vitest';
import { invoke } from '@tauri-apps/api/core';
import { askAboutExistingFiles } from './askAboutExistingFiles';
import { useExportCollisionStore } from '../store/useExportCollisionStore';
import { Invokes } from '../components/ui/AppProperties';
import type { CollisionSummary } from '../components/ui/ExportImportProperties';

vi.mock('@tauri-apps/api/core', () => ({ invoke: vi.fn() }));

const none: CollisionSummary = {
  total: 0,
  unchangedOwn: { count: 0, names: [] },
  changedOwn: { count: 0, names: [] },
  other: { count: 0, names: [] },
  originals: 0
};
const some: CollisionSummary = { ...none, total: 1, changedOwn: { count: 1, names: ['a.jpg'] } };
const recipe = (existingFiles: string, folder = '/out') => ({
  outputFolderOrFile: folder,
  exportSettings: { existingFiles, jpegQuality: 90 }
});

beforeEach(() => vi.mocked(invoke).mockReset());
afterEach(() => useExportCollisionStore.setState({ pending: null }));

it('leaves requests without Ask alone and asks the backend nothing', async () => {
  const requests = [recipe('keepBoth'), recipe('skip', '/b')];
  expect(await askAboutExistingFiles(requests)).toEqual(requests);
  expect(invoke).not.toHaveBeenCalled();
});

it('without a collision there is no question and Keep both is sent', async () => {
  vi.mocked(invoke).mockResolvedValue(none);
  const sent = await askAboutExistingFiles([recipe('ask')]);
  expect(sent?.[0].exportSettings.existingFiles).toBe('keepBoth');
  expect(useExportCollisionStore.getState().pending).toBeNull();
});

it('asks once for all the recipes set to Ask and sends the answer in them only', async () => {
  vi.mocked(invoke).mockResolvedValue(some);
  const requests = [recipe('ask'), recipe('skip', '/b'), recipe('ask', '/c')];
  const result = askAboutExistingFiles(requests);
  await vi.waitFor(() => expect(useExportCollisionStore.getState().pending).not.toBeNull());
  expect(invoke).toHaveBeenCalledTimes(1);
  expect(invoke).toHaveBeenCalledWith(Invokes.PlanExportCollisions, { requests: [requests[0], requests[2]] });
  expect(useExportCollisionStore.getState().pending?.summary).toEqual(some);
  useExportCollisionStore.getState().answer('replaceIfChanged');
  const sent = await result;
  expect(sent?.map((r) => r.exportSettings.existingFiles)).toEqual(['replaceIfChanged', 'skip', 'replaceIfChanged']);
});

it('a file named in Save As is asked about like any other', async () => {
  vi.mocked(invoke).mockResolvedValue(some);
  const named = { ...recipe('ask'), isExplicitFilePath: true };
  const result = askAboutExistingFiles([named]);
  await vi.waitFor(() => expect(useExportCollisionStore.getState().pending).not.toBeNull());
  expect(invoke).toHaveBeenCalledWith(Invokes.PlanExportCollisions, { requests: [named] });
  useExportCollisionStore.getState().answer('keepBoth');
  expect((await result)?.[0].exportSettings.existingFiles).toBe('keepBoth');
});

it('cancelling the question queues nothing', async () => {
  vi.mocked(invoke).mockResolvedValue(some);
  const result = askAboutExistingFiles([recipe('ask')]);
  await vi.waitFor(() => expect(useExportCollisionStore.getState().pending).not.toBeNull());
  useExportCollisionStore.getState().answer(null);
  expect(await result).toBeNull();
});
