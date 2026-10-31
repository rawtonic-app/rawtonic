import { cleanup, renderHook } from '@testing-library/react';
import type { MouseEvent } from 'react';
import { afterEach, beforeEach, expect, it, vi } from 'vitest';
import { useAppContextMenus } from './useAppContextMenus';
import { useEditorStore } from '../store/useEditorStore';
import { useLibraryStore } from '../store/useLibraryStore';
import { useUIStore } from '../store/useUIStore';
import { ImageFile } from '../components/ui/AppProperties';

type Option = { label?: string; submenu?: Option[]; onClick?(): void };
const showContextMenu = vi.fn<(x: number, y: number, options: Option[]) => void>();
vi.mock('../context/ContextMenuContext', () => ({ useContextMenu: () => ({ showContextMenu }) }));
vi.mock('@tauri-apps/api/core', () => ({ invoke: vi.fn(() => Promise.resolve()) }));
vi.mock('../activity/notify', () => ({ notify: { error: vi.fn(), success: vi.fn() } }));

const props = {
  handleImageSelect: vi.fn(),
  handleBackToLibrary: vi.fn(),
  handleRenameFiles: vi.fn(),
  handleImportClick: vi.fn(),
  handleLibraryRefresh: vi.fn(() => Promise.resolve()),
  refreshAllFolderTrees: vi.fn(() => Promise.resolve()),
  refreshImageList: vi.fn(() => Promise.resolve()),
  executeDelete: vi.fn(() => Promise.resolve()),
  handleTogglePinFolder: vi.fn(() => Promise.resolve()),
};
const raw = '/s/Capture/DSCF0135.RAF';
const copy = `${raw}?vc=9b8208`;
const image = (path: string) => ({ path, tags: [], is_raw: path.endsWith('.RAF') }) as unknown as ImageFile;
const event = { preventDefault() {}, stopPropagation() {}, clientX: 10, clientY: 10 } as unknown as MouseEvent;

const openMenu = (path: string) => {
  const { result } = renderHook(() => useAppContextMenus(props));
  result.current.handleThumbnailContextMenu(event, path);
  return showContextMenu.mock.lastCall![2];
};
const deleteChoices = (options: Option[]) =>
  options.find((option) => option.label === 'contextMenus.thumbnail.deleteImage')!.submenu!.map((o) => o.label);

beforeEach(() => {
  vi.clearAllMocks();
  useUIStore.setState({ activeView: 'library' });
  useEditorStore.setState({ selectedImage: null });
  useLibraryStore.setState({
    imageList: [image(raw), image(copy), image('/s/Capture/DSCF0147.RAF')],
    multiSelectedPaths: ['/s/Capture/DSCF0147.RAF'],
    libraryActivePath: '/s/Capture/DSCF0147.RAF',
  });
});
afterEach(cleanup);

it('selects a right-clicked grid photograph like a click, so the tools show it', () => {
  openMenu(raw);
  expect(useLibraryStore.getState().multiSelectedPaths).toEqual([raw]);
  expect(useLibraryStore.getState().libraryActivePath).toBe(raw);
  expect(props.handleImageSelect).toHaveBeenCalledWith(raw, false);
});

it('keeps the photograph shown in Develop when a filmstrip tile is right-clicked', () => {
  useUIStore.setState({ activeView: 'editor' });
  openMenu(raw);
  expect(useLibraryStore.getState().multiSelectedPaths).toEqual([raw]);
  expect(props.handleImageSelect).not.toHaveBeenCalled();
});

it('never offers to delete a virtual copy together with its original', () => {
  const choices = deleteChoices(openMenu(copy));
  expect(choices).toEqual(['contextMenus.editor.cancel', 'contextMenus.thumbnail.confirmDelete']);
  expect(choices).not.toContain('contextMenus.thumbnail.deleteAssociated');
});

const find = (options: Option[], label: string): Option | undefined => {
  for (const option of options) {
    if (option.label === label) return option;
    const inner = option.submenu && find(option.submenu, label);
    if (inner) return inner;
  }
};

it('applies a preset to the selection from the Library', async () => {
  const { usePresetStore } = await import('./usePresets');
  const { invoke } = await import('@tauri-apps/api/core');
  usePresetStore.setState({
    presets: [
      { preset: { id: 'warm', name: 'Warm', adjustments: { temperature: 12 } } },
      { folder: { id: 'f', name: 'Film', children: [{ id: 'bw', name: 'B&W', adjustments: { saturation: -100 } }] } },
    ] as never,
    isLoading: false,
  });
  const options = openMenu('/s/Capture/DSCF0147.RAF');
  const menu = find(options, 'editor.masks.actions.applyPreset')!;
  expect(menu.submenu!.map((o) => o.label)).toEqual(['Warm', 'Film']);
  find(options, 'B&W')!.onClick!();
  await vi.waitFor(() =>
    expect(invoke).toHaveBeenCalledWith('apply_adjustments_to_paths', {
      paths: ['/s/Capture/DSCF0147.RAF'],
      adjustments: { saturation: -100 },
      // A style replaces each photo's last applied one, as in the styles list.
      style: { id: 'bw' },
    }),
  );
});

it('leaves the next photograph alone when auto adjustments finish after a switch', async () => {
  const { invoke } = await import('@tauri-apps/api/core');
  let finishAuto!: () => void;
  vi.mocked(invoke).mockImplementation((command) => {
    if (command === 'apply_auto_adjustments_to_paths') return new Promise<void>((resolve) => { finishAuto = resolve; }) as never;
    if (command === 'load_metadata') return Promise.resolve({ adjustments: { exposure: 2 } }) as never;
    return Promise.resolve() as never;
  });
  const photo = (path: string) => ({ path, isReady: true, isRaw: true, width: 6000, height: 4000 }) as never;
  useUIStore.setState({ activeView: 'editor' });
  useEditorStore.getState().setEditor({ selectedImage: photo(raw) });
  find(openMenu(raw), 'contextMenus.thumbnail.autoAdjust')!.onClick!();
  useEditorStore.getState().setEditor({ selectedImage: photo('/s/Capture/DSCF0147.RAF') });
  const before = useEditorStore.getState().adjustments;
  const history = useEditorStore.getState().history;
  finishAuto();
  await vi.waitFor(() => expect(invoke).toHaveBeenCalledWith('apply_auto_adjustments_to_paths', { paths: [raw], batch: expect.stringMatching(/^batch-\d+$/) }));
  await new Promise((resolve) => setTimeout(resolve, 0));
  expect(invoke).not.toHaveBeenCalledWith('load_metadata', { path: raw });
  expect(useEditorStore.getState().adjustments).toBe(before);
  expect(useEditorStore.getState().history).toBe(history);
});

it('offers Match Light for the selection with the active photo as reference', async () => {
  const { appCommandEnabled, runAppCommand } = await import('../utils/appCommands');
  const { useSeriesMatchDialog } = await import('../components/studio/SeriesMatchDialog');
  renderHook(() => useAppContextMenus(props));
  expect(appCommandEnabled('match_light')).toBe(false);
  useLibraryStore.setState({ multiSelectedPaths: [raw, copy], libraryActivePath: copy });
  expect(appCommandEnabled('match_light')).toBe(true);
  expect(runAppCommand('match_light')).toBe(true);
  expect(useSeriesMatchDialog.getState().request).toEqual({ paths: [raw, copy], references: [copy] });
  useSeriesMatchDialog.getState().close();
});

it('offers selection-aware photo operations in the palette', async () => {
  const { appCommandEnabled, runAppCommand } = await import('../utils/appCommands');
  const { paletteCommands } = await import('../utils/commandPalette');
  renderHook(() => useAppContextMenus(props));
  const commands = paletteCommands((key) => Array.isArray(key) ? key[0] : key);
  const names = commands.map((command) => command.id);
  expect(commands.find((command) => command.id === 'merge_hdr')?.disabledHint).toBe('commandPalette.selectTwoPhotos');
  expect(commands.find((command) => command.id === 'merge_hdr_panorama')?.disabledHint).toBe('commandPalette.selectFourPhotos');
  expect(names).toEqual(expect.arrayContaining(['auto_lens_correction', 'convert_negative', 'merge_panorama', 'merge_hdr', 'merge_hdr_panorama', 'merge_focus_stack']));
  expect(appCommandEnabled('merge_hdr')).toBe(false);
  useLibraryStore.setState({ multiSelectedPaths: [raw, copy] });
  expect(runAppCommand('merge_hdr')).toBe(true);
  expect(useUIStore.getState().mergeModalState).toMatchObject({ isOpen: true, kind: 'hdr', sourcePaths: [raw, copy] });
});

type Custom = { customComponent?: unknown; customProps?: { paths: string[] } };
const pathsOf = (options: Option[], label: string) =>
  (find(options, label)!.submenu![0] as Custom).customProps!.paths;

it('opens or edits the whole selection with another program from the grid and the filmstrip', () => {
  const second = '/s/Capture/DSCF0147.RAF';
  useLibraryStore.setState({ multiSelectedPaths: [raw, second] });
  for (const view of ['library', 'editor'] as const) {
    useUIStore.setState({ activeView: view });
    const options = openMenu(raw);
    expect(pathsOf(options, 'contextMenus.openWith.label')).toEqual([raw, second]);
    expect(pathsOf(options, 'contextMenus.editWith.label')).toEqual([raw, second]);
  }
  // A photograph outside the selection is the one opened.
  expect(pathsOf(openMenu('/s/Capture/other.RAF'), 'contextMenus.openWith.label')).toEqual(['/s/Capture/other.RAF']);
});

it('opens or edits the photograph shown in the viewer with another program', () => {
  useEditorStore.setState({ selectedImage: { path: copy } as never });
  const { result } = renderHook(() => useAppContextMenus(props));
  result.current.handleEditorContextMenu(event);
  const options = showContextMenu.mock.lastCall![2];
  expect(pathsOf(options, 'contextMenus.openWith.label')).toEqual([copy]);
  expect(pathsOf(options, 'contextMenus.editWith.label')).toEqual([copy]);
});

it('offers Open With and Edit With in the menu bar for what the photo commands act on', async () => {
  const { appCommandEnabled, runAppCommand } = await import('../utils/appCommands');
  const { invoke } = await import('@tauri-apps/api/core');
  vi.mocked(invoke).mockImplementation(async (command: string) =>
    command === 'open_with_apps' || command === 'edit_with_apps'
      ? { supported: true, apps: [{ path: '/A/Preview.app', name: 'Preview', isDefault: true, isRecent: false, icon: null }] }
      : command === 'open_with_prefs'
        ? { recentApps: [], editRecipe: null }
        : undefined,
  );
  const hook = renderHook(() => useAppContextMenus(props));
  useLibraryStore.setState({ multiSelectedPaths: [] });
  expect(appCommandEnabled('open_with')).toBe(false);
  useLibraryStore.setState({ multiSelectedPaths: [raw, '/s/Capture/DSCF0147.RAF'] });
  expect(appCommandEnabled('edit_with')).toBe(true);

  expect(runAppCommand('open_with')).toBe(true);
  await vi.waitFor(() => expect(showContextMenu).toHaveBeenCalled());
  const options = showContextMenu.mock.lastCall![2];
  expect(vi.mocked(invoke)).toHaveBeenCalledWith('open_with_apps', { paths: [raw, '/s/Capture/DSCF0147.RAF'] });
  find(options, 'Preview · contextMenus.openWith.default')!.onClick!();
  await vi.waitFor(() =>
    expect(vi.mocked(invoke)).toHaveBeenCalledWith('open_with_open', {
      program: '/A/Preview.app',
      paths: [raw, '/s/Capture/DSCF0147.RAF'],
    }),
  );

  // In Develop it is the photograph shown there.
  useUIStore.setState({ activeView: 'editor' });
  useEditorStore.setState({ selectedImage: { path: copy } as never });
  showContextMenu.mockClear();
  runAppCommand('edit_with');
  await vi.waitFor(() => expect(showContextMenu).toHaveBeenCalled());
  expect(vi.mocked(invoke)).toHaveBeenCalledWith('edit_with_apps', { extension: 'tiff' });
  expect(find(showContextMenu.mock.lastCall![2], 'Preview · contextMenus.openWith.default')).toBeTruthy();
  hook.unmount();
  expect(appCommandEnabled('open_with')).toBe(false);
});
