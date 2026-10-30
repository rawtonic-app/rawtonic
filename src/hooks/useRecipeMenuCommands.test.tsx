import { useUIStore } from "../store/useUIStore";
import '../i18n';
import { act, cleanup, renderHook, waitFor } from '@testing-library/react';
import { afterEach, beforeEach, expect, it, vi } from 'vitest';
import { invoke } from '@tauri-apps/api/core';
import { open } from '@tauri-apps/plugin-dialog';
import { useRecipeMenuCommands } from './useRecipeMenuCommands';
import { useSettingsStore } from '../store/useSettingsStore';
import { useLibraryStore } from '../store/useLibraryStore';
import { useEditorStore } from '../store/useEditorStore';
import { useProcessStore } from '../store/useProcessStore';
import { Status } from '../components/ui/ExportImportProperties';
import { type AppSettings, Invokes } from '../components/ui/AppProperties';
import { appCommand, appCommandEnabled, runAppCommand } from '../utils/appCommands';
import { DEFAULT_RECIPE_DRAFT, presetFromDraft } from '../utils/exportRecipes';
vi.mock('@tauri-apps/api/core', () => ({ invoke: vi.fn(async () => {}) }));
vi.mock('@tauri-apps/plugin-dialog', () => ({ open: vi.fn(async () => '/output'), save: vi.fn() }));
vi.mock('../activity/notify', () => ({ notify: { success: vi.fn() } }));
beforeEach(() => {
  vi.clearAllMocks();
  useSettingsStore.setState({ appSettings: { exportPresets: [presetFromDraft(DEFAULT_RECIPE_DRAFT, 'jpg', 'Web')] } as AppSettings });
  useLibraryStore.setState({ multiSelectedPaths: [], sessionDirectory: null, rootPaths: ['/photos'] });
  useEditorStore.setState({ selectedImage: null });
  useUIStore.setState({ activeView: "library", libraryLoupe: false, isFullScreen: false });
  useProcessStore.getState().setExportState({ status: Status.Idle });
});
afterEach(cleanup);
it('disables recipes without selection and exports the selection with the saved recipe', async () => {
  renderHook(useRecipeMenuCommands);
  expect(appCommandEnabled('recipe:jpg')).toBe(false);
  expect(runAppCommand('recipe:jpg')).toBe(false);
  useLibraryStore.setState({ multiSelectedPaths: ['/photos/a.raw', '/photos/b.raw'] });
  act(() => { expect(runAppCommand('recipe:jpg')).toBe(true); });
  await waitFor(() => expect(invoke).toHaveBeenCalledWith(Invokes.ExportImagesBatch, { requests: [expect.objectContaining({
    paths: ['/photos/a.raw', '/photos/b.raw'], outputFolderOrFile: '/output', isExplicitFilePath: false,
    baseOriginFolders: ['/photos'], outputFormat: 'jpg',
  })] }));
  expect(open).toHaveBeenCalledOnce();
  expect(useSettingsStore.getState().appSettings?.exportPresets?.[0].lastExportPath).toBe('/output');
});
it('refreshes recipe names and removes deleted recipe handlers', () => {
  renderHook(useRecipeMenuCommands);
  act(() => useSettingsStore.setState({ appSettings: { exportPresets: [presetFromDraft(DEFAULT_RECIPE_DRAFT, 'new', 'Print')] } as AppSettings }));
  expect(appCommand('recipe:jpg')).toBeUndefined();
  expect(appCommand('recipe:new')?.menu?.label).toBe('Print');
});

it('does not export the cached Develop photograph after the grid selection is cleared', () => {
  useEditorStore.setState({ selectedImage: { path: '/photos/cached.raw' } as never });
  renderHook(useRecipeMenuCommands);
  expect(appCommandEnabled('recipe:jpg')).toBe(false);
  expect(runAppCommand('recipe:jpg')).toBe(false);
});
