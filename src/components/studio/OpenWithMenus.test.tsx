import '../../i18n';
import { afterEach, beforeEach, expect, it, vi } from 'vitest';
import { cleanup, fireEvent, render, screen, within } from '@testing-library/react';
import { invoke } from '@tauri-apps/api/core';
import { open } from '@tauri-apps/plugin-dialog';
import { EditWithSubMenu, OpenWithSubMenu } from './OpenWithMenus';
import { runEditWith, type AppChoices } from '../../utils/editWith';
import { useSettingsStore } from '../../store/useSettingsStore';

vi.mock('@tauri-apps/api/core', () => ({ invoke: vi.fn() }));
vi.mock('@tauri-apps/plugin-dialog', () => ({ open: vi.fn() }));
vi.mock('react-toastify', () => ({
  toast: { error: vi.fn(), success: vi.fn(), warn: vi.fn(), info: vi.fn(), dismiss: vi.fn() },
}));
vi.mock('../../utils/editWith', async (original) => ({
  ...(await original<typeof import('../../utils/editWith')>()),
  runEditWith: vi.fn(async () => ({ rendered: 2, failed: 0, linked: 2, problems: [] })),
}));
const mockInvoke = vi.mocked(invoke);

const apps: AppChoices = {
  supported: true,
  apps: [
    { path: '/System/Applications/Preview.app', name: 'Preview', isDefault: true, isRecent: false, icon: null },
    { path: '/Applications/Paint.app', name: 'Paint', isDefault: false, isRecent: true, icon: null },
    { path: '/Applications/Viewer.app', name: 'Viewer', isDefault: false, isRecent: false, icon: null },
  ],
};
const paths = ['/s/Capture/A.NEF', '/s/Capture/B.NEF?vc=c1', '/s/Capture/C.NEF'];

beforeEach(() => {
  mockInvoke.mockReset();
  useSettingsStore.setState({ appSettings: { exportPresets: [] } as never });
});
afterEach(cleanup);

it('lists the default program first, then the recent ones, and opens every selected original at once', async () => {
  mockInvoke.mockImplementation(async (command) => (command === 'open_with_apps' ? apps : undefined));
  const hide = vi.fn();
  render(<OpenWithSubMenu paths={paths} hideContextMenu={hide} />);
  await screen.findByText('Preview');
  const names = screen.getAllByRole('menuitem').map((item) => item.textContent);
  expect(names).toEqual(['PreviewDefault', 'Paint', 'Viewer', 'Other…']);
  expect(mockInvoke).toHaveBeenCalledWith('open_with_apps', { paths });
  fireEvent.click(screen.getByRole('menuitem', { name: /Paint/ }));
  expect(hide).toHaveBeenCalled();
  expect(mockInvoke).toHaveBeenCalledWith('open_with_open', { program: '/Applications/Paint.app', paths });
});

it('opens with any program chosen by Other…', async () => {
  mockInvoke.mockImplementation(async (command) => (command === 'open_with_apps' ? apps : undefined));
  vi.mocked(open).mockResolvedValue('/Applications/Other.app');
  render(<OpenWithSubMenu paths={paths} hideContextMenu={vi.fn()} />);
  fireEvent.click(await screen.findByRole('menuitem', { name: /Other…/ }));
  await vi.waitFor(() =>
    expect(mockInvoke).toHaveBeenCalledWith('open_with_open', { program: '/Applications/Other.app', paths }),
  );
});

it('says where Open With is not available', async () => {
  mockInvoke.mockResolvedValue({ supported: false, apps: [] });
  render(<OpenWithSubMenu paths={paths} hideContextMenu={vi.fn()} />);
  expect(await screen.findByText('Available on macOS only')).toBeTruthy();
  expect(screen.queryByRole('menuitem')).toBeNull();
});

it('edits the selection with the remembered recipe in the chosen program', async () => {
  useSettingsStore.setState({
    appSettings: {
      exportPresets: [
        { id: 'jpg', name: 'Proof JPEG', fileFormat: 'jpeg', jpegQuality: 90, filenameTemplate: '{original_filename}' },
      ],
    } as never,
  });
  mockInvoke.mockImplementation(async (command) => {
    if (command === 'open_with_prefs') return { recentApps: [], editRecipe: 'builtin-tiff16-prophoto' };
    if (command === 'edit_with_apps') return apps;
  });
  const hide = vi.fn();
  const rendered = vi.fn();
  render(<EditWithSubMenu paths={paths} hideContextMenu={hide} onRendered={rendered} />);
  const recipes = await screen.findAllByRole('menuitemradio');
  expect(recipes.map((r) => [r.textContent, r.getAttribute('aria-checked')])).toEqual([
    ['TIFF 16-bit · Adobe RGB', 'false'],
    ['TIFF 16-bit · ProPhoto RGB', 'true'],
    ['Proof JPEG', 'false'],
  ]);
  await screen.findByText('Preview');
  expect(mockInvoke).toHaveBeenCalledWith('edit_with_apps', { extension: 'tiff' });

  // Another recipe is remembered and lists the programs for its format.
  fireEvent.click(within(screen.getByRole('menu')).getByText('Proof JPEG'));
  expect(mockInvoke).toHaveBeenCalledWith('edit_with_set_recipe', { recipe: 'jpg' });
  await vi.waitFor(() => expect(mockInvoke).toHaveBeenCalledWith('edit_with_apps', { extension: 'jpg' }));

  fireEvent.click(await screen.findByRole('menuitem', { name: /Preview/ }));
  expect(hide).toHaveBeenCalled();
  await vi.waitFor(() => expect(rendered).toHaveBeenCalled());
  const [renderedPaths, recipe, program] = vi.mocked(runEditWith).mock.lastCall!;
  expect(renderedPaths).toEqual(paths);
  expect(recipe.id).toBe('jpg');
  expect(program).toBe('/System/Applications/Preview.app');
});

it('says Edit With is not available for photographs with generated areas', async () => {
  const { toast } = await import('react-toastify');
  vi.mocked(runEditWith).mockRejectedValueOnce('generated-areas: B.jpg, C.jpg');
  mockInvoke.mockImplementation(async (command) => {
    if (command === 'open_with_prefs') return { recentApps: [], editRecipe: null };
    if (command === 'edit_with_apps') return apps;
  });
  render(<EditWithSubMenu paths={paths} hideContextMenu={vi.fn()} />);
  fireEvent.click(await screen.findByRole('menuitem', { name: /Preview/ }));
  await vi.waitFor(() =>
    expect(toast.error).toHaveBeenCalledWith('Edit With is not available for photos with generated areas yet: B.jpg, C.jpg'),
  );
});
