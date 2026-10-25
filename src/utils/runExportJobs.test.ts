import { beforeEach, expect, it, vi } from 'vitest';
import { invoke } from '@tauri-apps/api/core';
import { runExportJobs } from './runExportJobs';
import { DEFAULT_RECIPE_DRAFT, exportSettingsFromDraft } from './exportRecipes';
import { useLibraryStore } from '../store/useLibraryStore';
import { useEditorStore } from '../store/useEditorStore';

vi.mock('@tauri-apps/api/core', () => ({ invoke: vi.fn() }));
vi.mock('@tauri-apps/plugin-dialog', () => ({ open: vi.fn(), save: vi.fn() }));
const saves = vi.hoisted(() => ({ flush: vi.fn(async (): Promise<string[]> => []) }));
vi.mock('../hooks/useEditorActions', () => ({ flushRecipeSaves: saves.flush }));

beforeEach(() => {
  vi.mocked(invoke).mockReset();
  useLibraryStore.setState({ sessionDirectory: '/Shoots/A', rootPaths: ['/Shoots/A'] });
  useEditorStore.setState({
    selectedImage: { path: '/p/a.NEF', isReady: true } as never,
    adjustments: { exposure: 1 } as never,
  });
});

it('queues an export from its library once the recipes being saved are written', async () => {
  let saved: () => void = () => {};
  saves.flush.mockImplementationOnce(() => new Promise((resolve) => (saved = () => resolve([]))));
  const draft = { ...DEFAULT_RECIPE_DRAFT, where: 'original' as const };
  const { settings, outputFormat } = exportSettingsFromDraft(draft);
  const run = runExportJobs([{ recipe: null, draft, settings, outputFormat }], ['/p/a.NEF', '/p/b.NEF']);
  await vi.waitFor(() => expect(saves.flush).toHaveBeenCalled());
  expect(vi.mocked(invoke)).not.toHaveBeenCalled();
  // Another library opens while the saves finish, showing the same
  // photograph with its own edit: the export stays A's, with A's edit.
  useLibraryStore.setState({ sessionDirectory: '/Shoots/B', rootPaths: ['/Shoots/B'] });
  useEditorStore.setState({ adjustments: { exposure: 2 } as never });
  saved();
  expect(await run).toBe(1);
  const [command, args] = vi.mocked(invoke).mock.calls[0];
  expect(command).toBe('export_images_batch');
  const [request] = (args as { requests: Record<string, unknown>[] }).requests;
  expect(request).toMatchObject({
    library: '/Shoots/A',
    baseOriginFolders: ['/Shoots/A'],
    currentEditPath: '/p/a.NEF',
    currentEditAdjustments: { exposure: 1 },
  });
});
