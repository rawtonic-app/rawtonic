// Pins what the editor SAVES today (base aa8e7f3c) when a photo with a given
// stored recipe is opened, Exposure is changed by +0.3 and the recipe is
// saved: load (`editorRecipe` = normalizeLoadedAdjustments + camera crop +
// never-edited defaults, as useImageLoader) -> `updateAdjustments` (the
// slider's store update) -> `debouncedSave` (what useImageProcessing calls
// on a changed recipe) -> the `save_metadata_and_update_thumbnail` command,
// mocked at the invoke boundary. The typed-recipe refactor must reproduce
// the expected JSON exactly (plan 0.3b-workflow).
//
// Not covered: the React effect that calls debouncedSave (the test calls it
// with the store's recipe), the camera's lens data (camera = null), camera
// crops (none), and the backend's own handling of the saved JSON.
//
// Record: WF_RECORD=1 npx vitest run src/hooks/recipeWorkflow.test.ts
// (rewrites the committed fixture and the scratch JSONs in WF_SCRATCH or
// ~/rawtonic-scratch/arch-wf).
import { act } from '@testing-library/react';
// @ts-expect-error node:fs is untyped in the frontend tsconfig
import { mkdirSync, readFileSync, writeFileSync } from 'node:fs';
// @ts-expect-error node:os is untyped in the frontend tsconfig
import { homedir } from 'node:os';
import { beforeEach, describe, expect, it, vi } from 'vitest';
import { invoke } from '@tauri-apps/api/core';
import { debouncedSave, updateAdjustments } from './useEditorActions';
import { useEditorStore } from '../store/useEditorStore';
import { useLibraryStore } from '../store/useLibraryStore';
import { Invokes } from '../components/ui/AppProperties';
import { type Adjustments } from '../utils/adjustments';
import { editorRecipe, neverEdited, newSaved, savedSourceFrame } from '../utils/neverEdited';

vi.mock('@tauri-apps/api/core', () => ({ invoke: vi.fn() }));
vi.mock('../activity/notify', () => ({ notify: { error: vi.fn() } }));

const env = (globalThis as unknown as { process: { env: Record<string, string | undefined> } }).process.env;
const FIXTURE_FILE = `${(globalThis as unknown as { process: { cwd(): string } }).process.cwd()}/src/hooks/__fixtures__/recipeWorkflow.expected.json`;
const RECORD = env.WF_RECORD === '1';
const SCRATCH: string = env.WF_SCRATCH ?? `${homedir()}/rawtonic-scratch/arch-wf`;

const FIXTURES: Record<string, unknown> = {
  null: null,
  empty: {},
  'frame-only': { sourceFrameVersion: 3 },
  'lens-profile-only': { lensProfileFor: 'Some Lens 50mm f/1.8' },
  'legacy-no-versions': { exposure: 0.7, contrast: 20 },
  'old-partial': { exposure: 0.2, toneVersion: 5, renderVersion: 2 },
  'explicit-nulls': { exposure: null, contrast: null, crop: null, lensProfileFor: null, toneVersion: null, renderVersion: null },
  'nested-unknown': {
    exposure: 0.5,
    futureTool: { a: { b: [1, 2, { c: 'x' }] }, enabled: true },
    masks: [
      {
        id: 'm1',
        futureMaskField: 7,
        subMasks: [{ id: 's1', type: 'radial', futureSub: true, parameters: { cx: 1, futureParam: 2 } }],
        adjustments: { exposure: 0.2, futureAdj: 5 },
      },
    ],
  },
  'future-version': { exposure: 0.1, renderVersion: 999, unknownKey: { keep: 'me' } },
};

const PATH = '/photo.x';
const sortKeys = (value: unknown): unknown =>
  Array.isArray(value)
    ? value.map(sortKeys)
    : value && typeof value === 'object'
      ? Object.fromEntries(
          Object.keys(value as object)
            .sort()
            .map((k) => [k, sortKeys((value as Record<string, unknown>)[k])]),
        )
      : value;

let saved: unknown[];
beforeEach(() => {
  saved = [];
  vi.mocked(invoke).mockImplementation(async (command, args) => {
    if (command === Invokes.SaveMetadataAndUpdateThumbnail) saved.push((args as { adjustments: unknown }).adjustments);
    return undefined;
  });
});

async function run(stored: unknown, isRaw: boolean) {
  const opened = editorRecipe(stored, isRaw, 4000, 3000, null);
  const photo = { path: PATH, isReady: true, isRaw, width: 4000, height: 3000, exif: {}, thumbnailUrl: '' };
  useEditorStore.setState({ selectedImage: photo, adjustments: opened, history: [opened], historyIndex: 0 });
  useLibraryStore.setState({ libraryActivePath: PATH, multiSelectedPaths: [PATH], libraryActiveAdjustments: opened });
  const exposure = Math.round((opened.exposure + 0.3) * 100) / 100;
  act(() => updateAdjustments({ exposure }));
  const edited = useEditorStore.getState().adjustments as Adjustments;
  debouncedSave(PATH, edited);
  await act(async () => {
    debouncedSave.flush();
  });
  await act(async () => {});
  expect(saved).toHaveLength(1);
  return {
    // JSON round trip: what the command serialises.
    savedJson: sortKeys(JSON.parse(JSON.stringify(saved[0]))),
    treatedAsNewEdit: newSaved(stored),
    neverEdited: neverEdited(stored),
    requestedFrame: savedSourceFrame(stored),
  };
}

describe('editor workflow: open stored recipe, Exposure +0.3, save', () => {
  const results: Record<string, unknown> = {};
  const expected: Record<string, unknown> = RECORD ? {} : JSON.parse(readFileSync(FIXTURE_FILE, 'utf8'));

  for (const kind of ['raw', 'raster'] as const) {
    for (const [name, stored] of Object.entries(FIXTURES)) {
      it(`${kind} / ${name}`, async () => {
        const result = await run(stored === null ? null : structuredClone(stored), kind === 'raw');
        const key = `${kind}.${name}`;
        results[key] = result;
        if (RECORD) {
          mkdirSync(SCRATCH, { recursive: true });
          writeFileSync(`${SCRATCH}/${kind}.wf-${name}.json`, JSON.stringify(result.savedJson, null, 2) + '\n');
          writeFileSync(FIXTURE_FILE, JSON.stringify(sortKeys(results), null, 1) + '\n');
          return;
        }
        expect(sortKeys(result)).toEqual(sortKeys(expected[key]));
        expect(JSON.stringify(sortKeys(result))).toBe(JSON.stringify(sortKeys(expected[key])));
      });
    }
  }
});

// Findings, pinned (they document current behaviour, they do not endorse it).
describe('recorded behaviour worth a second look', () => {
  const fixture = () => (RECORD ? {} : JSON.parse(readFileSync(FIXTURE_FILE, 'utf8')));
  const saved = (key: string) => (fixture()[key]?.savedJson ?? {}) as Record<string, unknown>;

  it.skipIf(RECORD)('legacy RAW recipe without a lens mode gains the new-edit RAW defaults on save', () => {
    // FINDING: a legacy RAW edit (exposure/contrast only, no lensCorrectionMode) counts as
    // "never edited", so the save adds fringeVersion 6 and autoCaVersion 3 (edge-fringe pass and
    // automatic lateral CA) and lensCorrectionMode 'manual': its render changes on first save.
    // The raster twin gains lensCorrectionMode/grainVersion 3 but no RAW passes.
    expect(saved('raw.legacy-no-versions')).toMatchObject({ fringeVersion: 6, autoCaVersion: 3, lensCorrectionMode: 'manual' });
    expect(saved('raster.legacy-no-versions').fringeVersion).toBeUndefined();
  });

  it.skipIf(RECORD)('legacy recipes keep their old render version but gain grainVersion 3', () => {
    // FINDING: the legacy recipe stays on the first render (no renderVersion key) and toneVersion 5,
    // yet grainVersion becomes 3 on save.
    expect(saved('raw.legacy-no-versions').renderVersion).toBeUndefined();
    expect(saved('raw.legacy-no-versions').toneVersion).toBe(5);
    expect(saved('raw.legacy-no-versions').grainVersion).toBe(3);
  });

  it.skipIf(RECORD)('future version and unknown keys survive; explicit nulls survive except version keys', () => {
    // FINDING: renderVersion 999 is kept (not downgraded) but toneVersion is filled with 5 beside it;
    // explicit null on a value (contrast) stays null in the saved JSON, null version keys are dropped.
    expect(saved('raw.future-version')).toMatchObject({ renderVersion: 999, toneVersion: 5, unknownKey: { keep: 'me' } });
    expect(saved('raw.explicit-nulls').contrast).toBeNull();
    expect(saved('raw.explicit-nulls').renderVersion).toBeUndefined();
    expect(saved('raw.explicit-nulls').toneVersion).toBe(5);
  });

  it.skipIf(RECORD)('a Reset recipe, a lens-only recipe and null all save as a new edit at the current versions', () => {
    for (const name of ['null', 'empty', 'frame-only', 'lens-profile-only']) {
      expect(saved(`raw.${name}`)).toMatchObject({ renderVersion: 11, sourceFrameVersion: 3, toneVersion: 10 });
    }
  });
});
