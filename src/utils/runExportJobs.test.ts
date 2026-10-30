import '../i18n';
import { beforeEach, expect, it, vi } from 'vitest';
import { invoke } from '@tauri-apps/api/core';
import { runExportJobs } from './runExportJobs';
import { invokeInOrder } from './catalogWrites';
import { DEFAULT_RECIPE_DRAFT, draftFromPreset, exportSettingsFromDraft, presetFromDraft } from './exportRecipes';
import { useLibraryStore } from '../store/useLibraryStore';
import { useSettingsStore } from '../store/useSettingsStore';
import type { AppSettings } from '../components/ui/AppProperties';
import { useEditorStore } from '../store/useEditorStore';
import { useProcessStore } from '../store/useProcessStore';
import { Status } from '../components/ui/ExportImportProperties';
import { toast } from 'react-toastify';

vi.mock('react-toastify', () => ({ toast: { success: vi.fn(), error: vi.fn(), info: vi.fn() } }));
vi.mock('@tauri-apps/api/core', () => ({ invoke: vi.fn() }));
vi.mock('@tauri-apps/plugin-dialog', () => ({ open: vi.fn(), save: vi.fn() }));
const saves = vi.hoisted(() => ({ flush: vi.fn(async (): Promise<string[]> => []) }));
vi.mock('../hooks/useEditorActions', () => ({ flushRecipeSaves: saves.flush }));

beforeEach(async () => {
  vi.mocked(invoke).mockReset();
  // Fresh pickers and toasts per test: a leftover answer would stand in for a
  // picker this test never meant to answer.
  const { open, save } = await import('@tauri-apps/plugin-dialog');
  vi.mocked(open).mockReset();
  vi.mocked(save).mockReset();
  vi.mocked(toast.success).mockReset();
  vi.mocked(toast.error).mockReset();
  // Recipes' folders are remembered through the settings, never a real file here.
  useSettingsStore.setState({ appSettings: null, handleSettingsChange: (async () => {}) as never });
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
    library: { library: '/Shoots/A' },
    baseOriginFolders: ['/Shoots/A'],
    currentEditPath: '/p/a.NEF',
    currentEditAdjustments: { exposure: 1 },
  });
});

it('says an export comes from no library when none is open', async () => {
  useLibraryStore.setState({ sessionDirectory: null });
  const draft = { ...DEFAULT_RECIPE_DRAFT, where: 'original' as const };
  const { settings, outputFormat } = exportSettingsFromDraft(draft);
  expect(await runExportJobs([{ recipe: null, draft, settings, outputFormat }], ['/p/a.NEF'])).toBe(1);
  const [, args] = vi.mocked(invoke).mock.calls[0];
  expect((args as { requests: { library: unknown }[] }).requests[0].library).toBe('noLibrary');
});

it('exports nothing while a selected photograph\'s edits could not be saved', async () => {
  saves.flush.mockResolvedValueOnce(['/p/b.NEF', '/elsewhere/c.NEF']);
  const draft = { ...DEFAULT_RECIPE_DRAFT, where: 'original' as const };
  const { settings, outputFormat } = exportSettingsFromDraft(draft);
  expect(await runExportJobs([{ recipe: null, draft, settings, outputFormat }], ['/p/a.NEF', '/p/b.NEF'])).toBe(0);
  expect(vi.mocked(invoke)).not.toHaveBeenCalled();
  const { status, errorMessage } = useProcessStore.getState().exportState;
  expect(status).toBe(Status.Error);
  expect(errorMessage).toContain('b.NEF');
  expect(errorMessage).not.toContain('c.NEF');
});

it('cancels the whole export when a recipe\'s folder picker is cancelled', async () => {
  const { open } = await import('@tauri-apps/plugin-dialog');
  // The first recipe's folder is cancelled: the second one is not asked for.
  vi.mocked(open).mockResolvedValueOnce(null).mockResolvedValueOnce('/out/print');
  const handleSettingsChange = vi.fn(async () => {});
  const recipe = (id: string, name: string) =>
    presetFromDraft({ ...DEFAULT_RECIPE_DRAFT, where: 'choose', folder: undefined }, id, name);
  useSettingsStore.setState({
    appSettings: { exportPresets: [recipe('web', 'Web'), recipe('print', 'Print')] } as never,
    handleSettingsChange,
  });
  const planned = ['web', 'print'].map((id) => {
    const preset = recipe(id, id);
    const draft = draftFromPreset(preset);
    return { recipe: preset, draft, ...exportSettingsFromDraft(draft) };
  });
  useProcessStore.setState({ exportState: { status: Status.Idle, errorMessage: '', progress: { current: 0, total: 0 } } });
  expect(await runExportJobs(planned, ['/p/a.NEF'])).toBe(0);
  // Nothing is exported, and no later folder is asked for.
  expect(vi.mocked(invoke)).not.toHaveBeenCalled();
  expect(vi.mocked(open)).toHaveBeenCalledTimes(1);
  // Cancelling is not a failure: no error, and the button is back to normal.
  expect(useProcessStore.getState().exportState).toMatchObject({ status: Status.Idle, errorMessage: '' });
  // The cancelled recipe's folder is not remembered as chosen.
  expect(handleSettingsChange).not.toHaveBeenCalled();
});

it('exports nothing when a later recipe\'s folder picker is cancelled', async () => {
  const { open } = await import('@tauri-apps/plugin-dialog');
  // The first recipe's folder is chosen, the second one's is cancelled: the
  // requests are only sent once every picker answered, so none is sent.
  vi.mocked(open).mockResolvedValueOnce('/out/web').mockResolvedValueOnce(null);
  const handleSettingsChange = vi.fn(async (_settings: AppSettings) => {});
  const recipe = (id: string, name: string) =>
    presetFromDraft({ ...DEFAULT_RECIPE_DRAFT, where: 'choose', folder: undefined }, id, name);
  useSettingsStore.setState({
    appSettings: { exportPresets: [recipe('web', 'Web'), recipe('print', 'Print')] } as never,
    handleSettingsChange,
  });
  const planned = ['web', 'print'].map((id) => {
    const preset = recipe(id, id);
    const draft = draftFromPreset(preset);
    return { recipe: preset, draft, ...exportSettingsFromDraft(draft) };
  });
  useProcessStore.setState({ exportState: { status: Status.Idle, errorMessage: '', progress: { current: 0, total: 0 } } });
  expect(await runExportJobs(planned, ['/p/a.NEF'])).toBe(0);
  // Neither recipe is exported, although the first one's folder was chosen.
  expect(vi.mocked(invoke)).not.toHaveBeenCalled();
  expect(vi.mocked(open)).toHaveBeenCalledTimes(2);
  // No toast, no error: cancelling is not a failure.
  expect(toast.success).not.toHaveBeenCalled();
  expect(toast.error).not.toHaveBeenCalled();
  expect(useProcessStore.getState().exportState).toMatchObject({ status: Status.Idle, errorMessage: '' });
  // The first recipe's chosen folder may be remembered, the cancelled one's not.
  for (const [changed] of handleSettingsChange.mock.calls)
    expect(changed.exportPresets?.filter((p) => p.lastExportPath)).toMatchObject([{ lastExportPath: '/out/web' }]);
});

it('queues an export once a rating set just before Export is written', async () => {  let rated: (value?: unknown) => void = () => {};
  vi.mocked(invoke).mockImplementation((command: string) =>
    command === 'set_rating_for_paths' ? new Promise((resolve) => (rated = resolve)) : Promise.resolve(undefined),
  );
  void invokeInOrder('set_rating_for_paths', { paths: ['/p/a.NEF'], rating: 5 });
  const draft = { ...DEFAULT_RECIPE_DRAFT, where: 'original' as const };
  const { settings, outputFormat } = exportSettingsFromDraft(draft);
  const run = runExportJobs([{ recipe: null, draft, settings, outputFormat }], ['/p/a.NEF']);
  await vi.waitFor(() => expect(saves.flush).toHaveBeenCalled());
  await new Promise((resolve) => setTimeout(resolve, 10));
  // The rating is still being written: the export (which reads it when
  // queued) waits.
  expect(vi.mocked(invoke).mock.calls.map(([command]) => command)).toEqual(['set_rating_for_paths']);
  rated([]);
  expect(await run).toBe(1);
  expect(vi.mocked(invoke).mock.calls.map(([command]) => command)).toEqual(['set_rating_for_paths', 'export_images_batch']);
});
