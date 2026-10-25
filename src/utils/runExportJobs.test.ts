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
  useLibraryStore.setState({ sessionDirectory: '/Shoots/A', rootPaths: [] });
});

it('queues an export from its library once the recipes being saved are written', async () => {
  let saved: () => void = () => {};
  saves.flush.mockImplementationOnce(() => new Promise((resolve) => (saved = () => resolve([]))));
  const draft = { ...DEFAULT_RECIPE_DRAFT, where: 'original' as const };
  const { settings, outputFormat } = exportSettingsFromDraft(draft);
  const run = runExportJobs([{ recipe: null, draft, settings, outputFormat }], ['/p/a.NEF', '/p/b.NEF']);
  await vi.waitFor(() => expect(saves.flush).toHaveBeenCalled());
  expect(vi.mocked(invoke)).not.toHaveBeenCalled();
  // Another library opens while the saves finish: the export stays A's.
  useLibraryStore.setState({ sessionDirectory: '/Shoots/B' });
  saved();
  expect(await run).toBe(1);
  const [command, args] = vi.mocked(invoke).mock.calls[0];
  expect(command).toBe('export_images_batch');
  expect((args as { requests: { library: string }[] }).requests[0].library).toBe('/Shoots/A');
});
