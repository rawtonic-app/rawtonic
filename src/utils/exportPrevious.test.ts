import '../i18n';
import { beforeEach, expect, it, vi } from 'vitest';
import { DEFAULT_RECIPE_DRAFT, exportSettingsFromDraft } from './exportRecipes';
import { exportPreviousState, lastExportFor, rememberLastExport, replayableDraft } from './exportPrevious';
import { useSettingsStore } from '../store/useSettingsStore';
import { useLibraryStore } from '../store/useLibraryStore';
import type { AppSettings } from '../components/ui/AppProperties';

vi.mock('@tauri-apps/api/core', () => ({ invoke: vi.fn(async () => {}) }));

const job = (where: 'output' | 'original' | 'choose', folder?: string) => {
  const draft = { ...DEFAULT_RECIPE_DRAFT, where, folder };
  return { recipe: null, draft, ...exportSettingsFromDraft(draft) };
};

beforeEach(() => {
  useSettingsStore.setState({ appSettings: {} as AppSettings });
  useLibraryStore.setState({ sessionDirectory: '/jobs/A' });
});

it('repeats a picked folder, or the folder of a file named for one photo, without asking again', () => {
  const choose = { ...DEFAULT_RECIPE_DRAFT, where: 'choose' as const };
  expect(replayableDraft(choose, { path: '/out/web', isFile: false })).toMatchObject({
    where: 'choose',
    folder: '/out/web'
  });
  expect(replayableDraft(choose, { path: 'C:\\out\\a.jpg', isFile: true }).folder).toBe('C:\\out');
  // The Output folder and the originals' folders are found again each time.
  const output = { ...DEFAULT_RECIPE_DRAFT, where: 'output' as const };
  expect(replayableDraft(output, { path: '/jobs/A/Output', isFile: false })).toBe(output);
  const fixed = { ...choose, folder: '/fixed' };
  expect(replayableDraft(fixed, { path: '/fixed', isFile: false })).toBe(fixed);
});

it('keeps the last export of each library, at most 20, and is unavailable before the first', () => {
  expect(exportPreviousState(['/a.NEF'])).toEqual({ enabled: false, hint: 'No export to repeat yet' });
  vi.useFakeTimers();
  for (let i = 0; i < 25; i += 1) {
    vi.setSystemTime(new Date(2026, 10, 27, 10, 0, i));
    rememberLastExport(`/jobs/${i}`, [job('original')]);
  }
  rememberLastExport('/jobs/A', [job('choose', '/out')]);
  vi.useRealTimers();
  const kept = Object.keys(useSettingsStore.getState().appSettings?.lastExports ?? {});
  expect(kept).toHaveLength(20);
  expect(kept).not.toContain('/jobs/0');
  expect(lastExportFor('/jobs/A')?.jobs[0].draft.folder).toBe('/out');
  expect(exportPreviousState([])).toEqual({ enabled: false, hint: 'Select photos to export' });
  expect(exportPreviousState(['/a.NEF'])).toEqual({ enabled: true });
});
