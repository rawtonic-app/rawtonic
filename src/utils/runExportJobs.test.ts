import { beforeEach, expect, it, vi } from 'vitest';
import { invoke } from '@tauri-apps/api/core';
import { runExportJobs } from './runExportJobs';
import { DEFAULT_RECIPE_DRAFT, exportSettingsFromDraft } from './exportRecipes';
import { useLibraryStore } from '../store/useLibraryStore';

vi.mock('@tauri-apps/api/core', () => ({ invoke: vi.fn() }));
vi.mock('@tauri-apps/plugin-dialog', () => ({ open: vi.fn(), save: vi.fn() }));
const saves = vi.hoisted(() => ({ flush: vi.fn(async (): Promise<string[]> => []) }));
vi.mock('../hooks/useEditorActions', () => ({ flushRecipeSaves: saves.flush }));

beforeEach(() => {
  vi.mocked(invoke).mockReset();
  useLibraryStore.setState({ sessionDirectory: null, rootPaths: [] });
});

it('queues an export only once the recipes being saved are written: it reads them when queued', async () => {
  let saved: () => void = () => {};
  saves.flush.mockImplementationOnce(() => new Promise((resolve) => (saved = () => resolve([]))));
  const draft = { ...DEFAULT_RECIPE_DRAFT, where: 'original' as const };
  const { settings, outputFormat } = exportSettingsFromDraft(draft);
  const run = runExportJobs([{ recipe: null, draft, settings, outputFormat }], ['/p/a.NEF', '/p/b.NEF']);
  await vi.waitFor(() => expect(saves.flush).toHaveBeenCalled());
  expect(vi.mocked(invoke)).not.toHaveBeenCalled();
  saved();
  expect(await run).toBe(1);
  expect(vi.mocked(invoke).mock.calls[0][0]).toBe('export_images_batch');
});
