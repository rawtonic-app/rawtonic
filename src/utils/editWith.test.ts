import { beforeEach, describe, expect, it, vi } from 'vitest';
import { invoke } from '@tauri-apps/api/core';
import {
  BUILT_IN_EDIT_RECIPES,
  chosenRecipe,
  editRecipes,
  editRequests,
  runEditWith,
  waitForJobs,
} from './editWith';
import { exportSettingsFromPreset } from './exportRecipes';
import { useProcessStore, type ExportJob } from '../store/useProcessStore';
import { useLibraryStore } from '../store/useLibraryStore';
import { useEditorStore } from '../store/useEditorStore';
import type { ExportPreset } from '../components/ui/ExportImportProperties';

vi.mock('@tauri-apps/api/core', () => ({ invoke: vi.fn() }));
vi.mock('../hooks/useEditorActions', () => ({ debouncedSave: { flush: vi.fn() } }));

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
    const requests = editRequests(
      [
        { source: '/s/Capture/A.NEF', target: '/s/Capture/A_edit.tiff' },
        { source: '/s/Capture/A.NEF?vc=c1', target: '/s/Capture/A_edit-2.tiff' },
      ],
      recipe,
    );
    expect(requests.map((r) => [r.paths, r.outputFolderOrFile, r.isExplicitFilePath])).toEqual([
      [['/s/Capture/A.NEF'], '/s/Capture/A_edit.tiff', true],
      [['/s/Capture/A.NEF?vc=c1'], '/s/Capture/A_edit-2.tiff', true],
    ]);
    const { destinationType, subfolder, filenameTemplate, ...pixels } = requests[0].exportSettings;
    const { destinationType: _d, subfolder: _s, filenameTemplate: _f, ...recipePixels } = recipe.settings;
    expect(pixels).toEqual(recipePixels);
    expect(editRequests(requests.slice(0, 1).map(() => ({ source: 'a', target: 'b' })), editRecipes([preset({})])[2])[0]
      .exportSettings.exportMasks).toBe(false);
    expect({ destinationType, subfolder, filenameTemplate, exportMasks: requests[0].exportSettings.exportMasks }).toEqual({
      destinationType: 'customFolder',
      subfolder: '',
      filenameTemplate: null,
      exportMasks: false,
    });
    expect(requests[0].outputFormat).toBe('tiff');
    expect(requests[0].baseOriginFolders).toEqual(['/s']);
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
  it('renders each photograph, then links and opens only the renders that were made', async () => {
    const targets = [
      { source: '/s/Capture/A.NEF', target: '/s/Capture/A_edit.tiff' },
      { source: '/s/Capture/B.NEF', target: '/s/Capture/B_edit.tiff' },
      { source: '/s/Capture/C.NEF', target: '/s/Capture/C_edit.tiff' },
    ];
    vi.mocked(invoke).mockImplementation(async (command: string) => {
      if (command === 'edit_with_targets') return targets;
      if (command === 'export_images_batch') {
        setTimeout(() =>
          useProcessStore.setState({ exportQueue: [job('j1', 'done'), job('j2', 'failed'), job('j3', 'done')] }),
        );
        return ['j1', 'j2', 'j3'];
      }
      if (command === 'edit_with_finish') return { linked: 2, problems: [] };
    });
    const paths = targets.map((t) => t.source);
    const result = await runEditWith(paths, BUILT_IN_EDIT_RECIPES[0], '/Applications/Preview.app');
    expect(result).toEqual({ rendered: 2, failed: 1, linked: 2, problems: [] });
    const calls = vi.mocked(invoke).mock.calls;
    expect(calls[0]).toEqual(['edit_with_targets', { paths, extension: 'tiff' }]);
    expect((calls[1][1] as { requests: unknown[] }).requests).toHaveLength(3);
    expect(calls[2]).toEqual([
      'edit_with_finish',
      { items: [targets[0], targets[2]], program: '/Applications/Preview.app' },
    ]);
  });

  it('opens nothing when no render was made', async () => {
    vi.mocked(invoke).mockImplementation(async (command: string) => {
      if (command === 'edit_with_targets') return [{ source: '/a.NEF', target: '/a_edit.tiff' }];
      if (command === 'export_images_batch') {
        setTimeout(() => useProcessStore.setState({ exportQueue: [job('j1', 'cancelled')] }));
        return ['j1'];
      }
    });
    const result = await runEditWith(['/a.NEF'], BUILT_IN_EDIT_RECIPES[0], '/Applications/Preview.app');
    expect(result.rendered).toBe(0);
    expect(vi.mocked(invoke).mock.calls.map((c) => c[0])).not.toContain('edit_with_finish');
  });
});
