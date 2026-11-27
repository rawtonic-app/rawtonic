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
  expect(replayableDraft(choose, { path: '/out/web', isFile: false }, '/jobs/A')).toMatchObject({
    where: 'choose',
    folder: '/out/web'
  });
  expect(replayableDraft(choose, { path: 'C:\\out\\a.jpg', isFile: true }, '/jobs/A').folder).toBe('C:\\out');
  // A file at a drive's root or the POSIX root: the root itself, not "E:" or "".
  expect(replayableDraft(choose, { path: 'E:\\proof.jpg', isFile: true }, null).folder).toBe('E:\\');
  expect(replayableDraft(choose, { path: '/proof.jpg', isFile: true }, null).folder).toBe('/');
  // The Output folder and the originals' folders are found again each time.
  const output = { ...DEFAULT_RECIPE_DRAFT, where: 'output' as const };
  expect(replayableDraft(output, { path: '/jobs/A/Output', isFile: false }, '/jobs/A')).toBe(output);
  // Output with no library open asked for a folder: that one is repeated.
  expect(replayableDraft(output, { path: '/picked', isFile: false }, null)).toMatchObject({
    where: 'choose',
    folder: '/picked'
  });
  const fixed = { ...choose, folder: '/fixed' };
  expect(replayableDraft(fixed, { path: '/fixed', isFile: false }, '/jobs/A')).toBe(fixed);
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
