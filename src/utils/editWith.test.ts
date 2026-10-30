import { beforeEach, describe, expect, it, vi } from 'vitest';
import { invoke } from '@tauri-apps/api/core';
import {
  BUILT_IN_EDIT_RECIPES,
  chosenRecipe,
  editRecipes,
  editRequest,
  runEditWith,
  waitForJobs,
} from './editWith';
import { exportSettingsFromPreset } from './exportRecipes';
import { useProcessStore, type ExportJob } from '../store/useProcessStore';
import { useLibraryStore } from '../store/useLibraryStore';
import { debouncedSetHistory, useEditorStore } from '../store/useEditorStore';
import { submitPhotoWrite } from './photoWrites';
import type { ExportPreset } from '../components/ui/ExportImportProperties';

vi.mock('@tauri-apps/api/core', () => ({ invoke: vi.fn() }));
const saves = vi.hoisted(() => ({ flush: vi.fn(async (): Promise<string[]> => []) }));
vi.mock('../hooks/useEditorActions', () => ({ flushRecipeSaves: saves.flush }));

const preset = (over: Partial<ExportPreset>): ExportPreset =>
  ({
    id: 'r1',
    name: 'Print TIFF',
    fileFormat: 'tiff',
    jpegQuality: 90,
    tiffBitDepth: 16,
    colorSpace: 'proPhoto',
    enableResize: true,
    resizeMode: 'longEdge',
    resizeValue: 4000,
    dontEnlarge: true,
    keepMetadata: true,
    preserveTimestamps: false,
    stripGps: false,
    exportMasks: true,
    filenameTemplate: '{original_filename}-print',
    destinationType: 'originalFolder',
    enableWatermark: false,
    watermarkPath: null,
    watermarkAnchor: 'bottomRight',
    watermarkScale: 10,
    watermarkSpacing: 5,
    watermarkOpacity: 75,
    ...over,
  }) as ExportPreset;

const job = (id: string, state: ExportJob['state']) => ({ id, state }) as ExportJob;

beforeEach(() => {
  vi.mocked(invoke).mockReset();
  useProcessStore.setState({ exportQueue: [] });
  useLibraryStore.setState({ rootPaths: ['/s'] });
  useEditorStore.setState({ selectedImage: null });
});

describe('recipes', () => {
  it('offers full-size 16-bit TIFFs first, then the saved image recipes', () => {
    const [adobe, proPhoto] = BUILT_IN_EDIT_RECIPES;
    expect(adobe.outputFormat).toBe('tiff');
    expect(adobe.settings).toMatchObject({ tiffBitDepth: 16, colorSpace: 'adobeRgb', resize: null, keepMetadata: true });
    expect(proPhoto.settings.colorSpace).toBe('proPhoto');
    const recipes = editRecipes([
      preset({}),
      preset({ id: 'lut', name: 'LUT', fileFormat: 'cube' }),
      preset({ id: '__last_used__', name: 'last' }),
    ]);
    expect(recipes.map((r) => r.id)).toEqual(['builtin-tiff16-adobe', 'builtin-tiff16-prophoto', 'r1']);
    // A saved recipe renders with exactly what Export uses for it.
    expect(recipes[2].settings).toEqual(exportSettingsFromPreset(preset({})).settings);
    expect(chosenRecipe(recipes, 'r1').id).toBe('r1');
    expect(chosenRecipe(recipes, 'gone').id).toBe('builtin-tiff16-adobe');
    expect(chosenRecipe(recipes, null).id).toBe('builtin-tiff16-adobe');
  });

  it('changes only where each render goes, so it is the bytes of an Export with the recipe', () => {
    // Without separate mask files (a recipe exporting them gets one flattened image).
    const recipe = editRecipes([preset({ exportMasks: false })])[2];
    const request = editRequest(recipe);
    // The photograph and its file are filled in by edit_with_start.
    expect([request.paths, request.outputFolderOrFile, request.isExplicitFilePath]).toEqual([[], '', true]);
    const { destinationType, subfolder, filenameTemplate, ...pixels } = request.exportSettings;
    const { destinationType: _d, subfolder: _s, filenameTemplate: _f, ...recipePixels } = recipe.settings;
    expect(pixels).toEqual(recipePixels);
    expect(editRequest(editRecipes([preset({})])[2]).exportSettings.exportMasks).toBe(false);
    expect({ destinationType, subfolder, filenameTemplate, exportMasks: request.exportSettings.exportMasks }).toEqual({
      destinationType: 'customFolder',
      subfolder: '',
      filenameTemplate: null,
      exportMasks: false,
    });
    expect(request.outputFormat).toBe('tiff');
    expect(request.baseOriginFolders).toEqual(['/s']);
  });
});

describe('waiting for the renders', () => {
  it('resolves with the jobs that were done once all have settled', async () => {
    let result: Set<string> | undefined;
    void waitForJobs(['a', 'b', 'c']).then((done) => (result = done));
    useProcessStore.setState({ exportQueue: [job('a', 'running'), job('b', 'queued'), job('c', 'queued')] });
    useProcessStore.setState({ exportQueue: [job('a', 'done'), job('b', 'paused'), job('c', 'running')] });
    await Promise.resolve();
    expect(result).toBeUndefined();
    // b was removed from the queue after it was listed: not done.
    useProcessStore.setState({ exportQueue: [job('a', 'done'), job('c', 'failed')] });
    await vi.waitFor(() => expect(result).toEqual(new Set(['a'])));
  });

  it('remembers jobs that were done before the queue dropped them', async () => {
    // The queue keeps only its latest finished jobs.
    let result: Set<string> | undefined;
    void waitForJobs(['a', 'b', 'c']).then((done) => (result = done));
    useProcessStore.setState({ exportQueue: [job('a', 'done'), job('b', 'running'), job('c', 'queued')] });
    useProcessStore.setState({ exportQueue: [job('b', 'done'), job('c', 'running')] });
    useProcessStore.setState({ exportQueue: [job('c', 'done')] });
    await vi.waitFor(() => expect(result).toEqual(new Set(['a', 'b', 'c'])));
  });
});

describe('runEditWith', () => {
  const started = (targets: { source: string; target: string }[], jobs: string[], queue: ExportJob[]) =>
    vi.mocked(invoke).mockImplementation(async (command: string) => {
      if (command === 'edit_with_start') {
        setTimeout(() => useProcessStore.setState({ exportQueue: queue }));
        return { targets, jobs };
      }
      if (command === 'edit_with_finish') return { linked: 2, problems: [] };
    });

  it('renders each photograph, then links and opens only the renders that were made', async () => {
    const targets = [
      { source: '/s/Capture/A.NEF', target: '/s/Capture/A_edit.tiff' },
      { source: '/s/Capture/B.NEF', target: '/s/Capture/B_edit.tiff' },
      { source: '/s/Capture/C.NEF', target: '/s/Capture/C_edit.tiff' },
    ];
    started(targets, ['j1', 'j2', 'j3'], [job('j1', 'done'), job('j2', 'failed'), job('j3', 'done')]);
    const paths = targets.map((t) => t.source);
    const result = await runEditWith(paths, BUILT_IN_EDIT_RECIPES[0], '/Applications/Preview.app');
    expect(result).toEqual({ rendered: 2, failed: 1, linked: 2, problems: [] });
    const calls = vi.mocked(invoke).mock.calls;
    // The program goes with the jobs: a quit offers this Edit With again.
    expect(calls[0]).toEqual([
      'edit_with_start',
      { paths, recipe: editRequest(BUILT_IN_EDIT_RECIPES[0]), program: '/Applications/Preview.app' },
    ]);
    expect(calls[1]).toEqual([
      'edit_with_finish',
      { items: [targets[0], targets[2]], program: '/Applications/Preview.app' },
    ]);
  });

  it('starts only once the pending edits are saved, and not when they could not be', async () => {
    let saved: (failed: string[]) => void = () => {};
    saves.flush.mockImplementationOnce(() => new Promise((resolve) => (saved = resolve)));
    started([{ source: '/a.NEF', target: '/a_edit.tiff' }], ['j1'], [job('j1', 'done')]);
    const run = runEditWith(['/a.NEF'], BUILT_IN_EDIT_RECIPES[0], '/P.app');
    await Promise.resolve();
    expect(vi.mocked(invoke)).not.toHaveBeenCalled();
    saved([]);
    await run;
    expect(vi.mocked(invoke).mock.calls[0][0]).toBe('edit_with_start');

    vi.mocked(invoke).mockClear();
    saves.flush.mockResolvedValueOnce(['/a.NEF']);
    await expect(runEditWith(['/a.NEF'], BUILT_IN_EDIT_RECIPES[0], '/P.app')).rejects.toThrow('/a.NEF');
    expect(vi.mocked(invoke)).not.toHaveBeenCalled();
  });

  it('records the pending undo step before the edits are saved', async () => {
    const order: string[] = [];
    const history = vi.spyOn(debouncedSetHistory, 'flush').mockImplementation(() => order.push('history'));
    saves.flush.mockImplementationOnce(async () => (order.push('save'), []));
    started([{ source: '/a.NEF', target: '/a_edit.tiff' }], ['j1'], [job('j1', 'done')]);
    await runEditWith(['/a.NEF'], BUILT_IN_EDIT_RECIPES[0], '/P.app');
    expect(order).toEqual(['history', 'save']);
    history.mockRestore();
  });

  it('waits for the queued photo writes before the renders are queued', async () => {
    let written: () => void = () => {};
    void submitPhotoWrite(['/a.NEF'], () => new Promise<void>((resolve) => (written = resolve)), { kind: 'rating' });
    started([{ source: '/a.NEF', target: '/a_edit.tiff' }], ['j1'], [job('j1', 'done')]);
    const run = runEditWith(['/a.NEF'], BUILT_IN_EDIT_RECIPES[0], '/P.app');
    await new Promise((resolve) => setTimeout(resolve, 0));
    expect(vi.mocked(invoke)).not.toHaveBeenCalled();
    written();
    await run;
    expect(vi.mocked(invoke).mock.calls[0][0]).toBe('edit_with_start');
  });

  it('queues what the click saw: its library and editor recipe, whatever opens or changes meanwhile', async () => {
    useLibraryStore.setState({ sessionDirectory: '/A.session' });
    useEditorStore.setState({
      selectedImage: { path: '/A.session/Capture/a.NEF', isReady: true } as never,
      adjustments: { exposure: 1 } as never,
    });
    const atClick = editRequest(BUILT_IN_EDIT_RECIPES[0]);
    expect(atClick).toMatchObject({
      library: { library: '/A.session' },
      currentEditPath: '/A.session/Capture/a.NEF',
    });
    let saved: (failed: string[]) => void = () => {};
    saves.flush.mockImplementationOnce(() => new Promise((resolve) => (saved = resolve)));
    const targets = [
      { source: '/A.session/Capture/a.NEF', target: '/A.session/Capture/a_edit.tiff', library: '/A.session' },
    ];
    vi.mocked(invoke).mockImplementation(async (command: string) => {
      if (command === 'edit_with_start') {
        setTimeout(() => useProcessStore.setState({ exportQueue: [job('j1', 'done')] }));
        return { targets, jobs: ['j1'] };
      }
      if (command === 'edit_with_finish') return { linked: 1, problems: [] };
    });
    const run = runEditWith([targets[0].source], BUILT_IN_EDIT_RECIPES[0], '/P.app');
    // While the edits are being written, another library opens and the
    // editor moves on.
    useLibraryStore.setState({ sessionDirectory: '/B.catalog' });
    useEditorStore.setState({ selectedImage: null, adjustments: { exposure: 3 } as never });
    saved([]);
    const result = await run;
    expect(result).toMatchObject({ rendered: 1, failed: 0 });
    const calls = vi.mocked(invoke).mock.calls;
    expect(calls[0]).toEqual(['edit_with_start', { paths: [targets[0].source], recipe: atClick, program: '/P.app' }]);
    expect(calls.map((c) => c[0])).not.toContain('export_queue_cancel');
    expect(calls[1][1]).toEqual({ items: targets, program: '/P.app' });
  });

  it('opens nothing when no render was made', async () => {
    started([{ source: '/a.NEF', target: '/a_edit.tiff' }], ['j1'], [job('j1', 'cancelled')]);
    const result = await runEditWith(['/a.NEF'], BUILT_IN_EDIT_RECIPES[0], '/Applications/Preview.app');
    expect(result.rendered).toBe(0);
    expect(vi.mocked(invoke).mock.calls.map((c) => c[0])).not.toContain('edit_with_finish');
  });
});
