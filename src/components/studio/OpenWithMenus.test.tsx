import '../../i18n';
import { afterEach, beforeEach, expect, it, vi } from 'vitest';
import { cleanup, fireEvent, render, screen, within } from '@testing-library/react';
import { invoke } from '@tauri-apps/api/core';
import { open } from '@tauri-apps/plugin-dialog';
import { EditWithSubMenu, OpenWithSubMenu, editWithOptions, offerInterruptedEditWith } from './OpenWithMenus';
import { EditWithSettingsDialogHost } from './EditWithSettingsDialog';
import { runEditWith, type AppChoices } from '../../utils/editWith';
import { useSettingsStore } from '../../store/useSettingsStore';
import { useLibraryStore } from '../../store/useLibraryStore';
import { submitPhotoWrite } from '../../utils/photoWrites';
import i18n from 'i18next';
import packaged from './fixtures/openWithPackaged.json';
import linux from './fixtures/openWithLinux.json';
import programSettingsFixture from './fixtures/editWithProgramSettings.json';

vi.mock('@tauri-apps/api/core', () => ({ invoke: vi.fn() }));
vi.mock('@tauri-apps/plugin-dialog', () => ({ open: vi.fn() }));
vi.mock('../../activity/notify', () => ({
  notify: { error: vi.fn(), success: vi.fn(), warn: vi.fn(), info: vi.fn(), dismiss: vi.fn() }
}));
vi.mock('../../utils/editWith', async (original) => ({
  ...(await original<typeof import('../../utils/editWith')>()),
  runEditWith: vi.fn(async () => ({
    rendered: 2,
    cancelled: 0,
    failed: 0,
    failedPaths: [],
    library: null,
    linked: 2,
    problems: []
  }))
}));
const mockInvoke = vi.mocked(invoke);

const apps: AppChoices = {
  supported: true,
  apps: [
    { path: '/System/Applications/Preview.app', name: 'Preview', isDefault: true, isRecent: false, icon: null },
    { path: '/Applications/Paint.app', name: 'Paint', isDefault: false, isRecent: true, icon: null },
    { path: '/Applications/Viewer.app', name: 'Viewer', isDefault: false, isRecent: false, icon: null }
  ]
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
  await vi.waitFor(() =>
    expect(mockInvoke).toHaveBeenCalledWith('open_with_open', { program: '/Applications/Paint.app', paths })
  );
});

it('hands the originals over only once their queued writes (rating, XMP) are written', async () => {
  mockInvoke.mockImplementation(async (command) => (command === 'open_with_apps' ? apps : undefined));
  let written: () => void = () => {};
  void submitPhotoWrite([paths[0]], () => new Promise<void>((resolve) => (written = resolve)), { kind: 'rating' });
  render(<OpenWithSubMenu paths={paths} hideContextMenu={vi.fn()} />);
  await screen.findByText('Preview');
  fireEvent.click(screen.getByRole('menuitem', { name: /Paint/ }));
  await new Promise((resolve) => setTimeout(resolve, 0));
  expect(mockInvoke).not.toHaveBeenCalledWith('open_with_open', expect.anything());
  written();
  await vi.waitFor(() =>
    expect(mockInvoke).toHaveBeenCalledWith('open_with_open', { program: '/Applications/Paint.app', paths })
  );
});

it('opens with any program chosen by Other…', async () => {
  mockInvoke.mockImplementation(async (command) => (command === 'open_with_apps' ? apps : undefined));
  vi.mocked(open).mockResolvedValue('/Applications/Other.app');
  render(<OpenWithSubMenu paths={paths} hideContextMenu={vi.fn()} />);
  fireEvent.click(await screen.findByRole('menuitem', { name: /Other…/ }));
  await vi.waitFor(() =>
    expect(mockInvoke).toHaveBeenCalledWith('open_with_open', { program: '/Applications/Other.app', paths })
  );
});

it('on Windows, Browse for a program… asks for a program file and Edit With names the program as the menu does', async () => {
  const { notify } = await import('../../activity/notify');
  useSettingsStore.setState({ osPlatform: 'windows' });
  const windowsApps: AppChoices = {
    supported: true,
    apps: [
      { path: 'Windows-Fotoanzeige', name: 'Windows-Fotoanzeige', isDefault: true, isRecent: false, icon: null },
      {
        path: 'C:\\Program Files\\WindowsApps\\Microsoft.Paint_11.2609.401.0_x64__8wekyb3d8bbwe\\PaintApp\\mspaint.exe',
        name: 'Paint',
        isDefault: false,
        isRecent: false,
        icon: null
      }
    ]
  };
  mockInvoke.mockImplementation(async (command) => {
    if (command === 'open_with_prefs') return { recentApps: [], editRecipe: null };
    if (command === 'open_with_pick')
      return { ...windowsApps.apps[1], path: 'D:\\Tools\\Bild Bearbeiter\\Größe ändern.exe' };
    if (command === 'edit_with_apps' || command === 'open_with_apps') return windowsApps;
  });
  try {
    render(<EditWithSubMenu paths={paths} hideContextMenu={vi.fn()} />);
    fireEvent.click(await screen.findByRole('menuitem', { name: /^Paint/ }));
    await vi.waitFor(() => expect(notify.info).toHaveBeenCalledWith('Rendering for Paint…', expect.anything()));
    expect(vi.mocked(runEditWith).mock.lastCall![2]).toBe(windowsApps.apps[1].path);
    cleanup();

    vi.mocked(open).mockResolvedValue('D:\\Tools\\Bild Bearbeiter\\Größe ändern.exe');
    render(<OpenWithSubMenu paths={paths} hideContextMenu={vi.fn()} />);
    fireEvent.click(await screen.findByRole('menuitem', { name: /Browse for a program…/ }));
    await vi.waitFor(() =>
      expect(mockInvoke).toHaveBeenCalledWith('open_with_open', {
        program: 'D:\\Tools\\Bild Bearbeiter\\Größe ändern.exe',
        paths
      })
    );
    expect(mockInvoke).toHaveBeenCalledWith('open_with_pick', { title: expect.any(String) });
  } finally {
    useSettingsStore.setState({ osPlatform: '' });
  }
});

it('on Windows lists the programs Windows offers without recommending them (Photos) after the others, and opens them by their id', async () => {
  useSettingsStore.setState({ osPlatform: 'windows' });
  const { choices, openWithOpen } = packaged;
  mockInvoke.mockImplementation(async (command) => (command === 'open_with_apps' ? choices : undefined));
  try {
    const { container } = render(<OpenWithSubMenu paths={openWithOpen.paths} hideContextMenu={vi.fn()} />);
    await screen.findByText('Photos');
    const names = screen.getAllByRole('menuitem').map((item) => item.textContent);
    expect(names).toEqual(['PaintDefault', 'Photos', 'Browse for a program…']);
    // Default · separator · offered, not recommended · separator · browse.
    expect(container.querySelectorAll('.rawtonic-menu-separator')).toHaveLength(2);
    fireEvent.click(screen.getByRole('menuitem', { name: /Photos/ }));
    // The exact arguments the Rust side reads (a_packaged_program_crosses_ipc_as_the_frontend_sends_it).
    await vi.waitFor(() => expect(mockInvoke).toHaveBeenCalledWith('open_with_open', openWithOpen));
  } finally {
    useSettingsStore.setState({ osPlatform: '' });
  }
});

it('on Linux lists the desktop entries, default first, and opens the chosen one by its .desktop file', async () => {
  useSettingsStore.setState({ osPlatform: 'linux' });
  mockInvoke.mockImplementation(async (command) => (command === 'open_with_apps' ? linux.choices : undefined));
  try {
    render(<OpenWithSubMenu paths={linux.openWithOpen.paths} hideContextMenu={vi.fn()} />);
    await screen.findByText('GIMP');
    expect(screen.getAllByRole('menuitem').map((item) => item.textContent)).toEqual([
      'GIMPDefault',
      'Krita',
      'Browse for a program…'
    ]);
    fireEvent.click(screen.getByRole('menuitem', { name: /Krita/ }));
    // The exact arguments the Rust side reads (a_desktop_program_crosses_ipc_as_the_frontend_sends_it).
    await vi.waitFor(() => expect(mockInvoke).toHaveBeenCalledWith('open_with_open', linux.openWithOpen));
  } finally {
    useSettingsStore.setState({ osPlatform: '' });
  }
});

it('says where Open With is not available', async () => {
  mockInvoke.mockResolvedValue({ supported: false, apps: [] });
  render(<OpenWithSubMenu paths={paths} hideContextMenu={vi.fn()} />);
  expect(await screen.findByText('Not available on Linux yet')).toBeTruthy();
  expect(screen.queryByRole('menuitem')).toBeNull();
});

it('edits the selection with the remembered recipe in the chosen program', async () => {
  useSettingsStore.setState({
    appSettings: {
      exportPresets: [
        { id: 'jpg', name: 'Proof JPEG', fileFormat: 'jpeg', jpegQuality: 90, filenameTemplate: '{original_filename}' }
      ]
    } as never
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
    ['Proof JPEG', 'false']
  ]);
  await screen.findByText('Preview');
  expect(mockInvoke).toHaveBeenCalledWith('edit_with_apps', { extension: 'tiff' });

  // Another recipe is remembered and lists the programs for its format.
  fireEvent.click(within(screen.getByRole('menu')).getByText('Proof JPEG'));
  expect(mockInvoke).toHaveBeenCalledWith('edit_with_set_recipe', { recipe: 'jpg' });
  await vi.waitFor(() => expect(mockInvoke).toHaveBeenCalledWith('edit_with_apps', { extension: 'jpg' }));

  fireEvent.click(await screen.findByRole('menuitem', { name: /^Preview/ }));
  expect(hide).toHaveBeenCalled();
  await vi.waitFor(() => expect(rendered).toHaveBeenCalled());
  const [renderedPaths, recipe, program] = vi.mocked(runEditWith).mock.lastCall!;
  expect(renderedPaths).toEqual(paths);
  expect(recipe.id).toBe('jpg');
  expect(program).toBe('/System/Applications/Preview.app');
});

it('reports a cancelled batch row as information without a failure', async () => {
  const { notify } = await import('../../activity/notify');
  vi.mocked(runEditWith).mockResolvedValueOnce({
    rendered: 2,
    cancelled: 1,
    failed: 0,
    failedPaths: [],
    library: null,
    linked: 2,
    problems: []
  });
  mockInvoke.mockImplementation(async (command) => {
    if (command === 'open_with_prefs') return { recentApps: [], editRecipe: null };
    if (command === 'edit_with_apps') return apps;
  });
  render(<EditWithSubMenu paths={['/a.NEF', '/b.NEF', '/c.NEF']} hideContextMenu={vi.fn()} />);
  fireEvent.click(await screen.findByRole('menuitem', { name: /^Preview/ }));
  await vi.waitFor(() => expect(notify.info).toHaveBeenCalledWith('1 cancelled, 2 rendered.'));
  expect(notify.error).not.toHaveBeenCalled();
});

it('offers Try again after a failed render, which renders only the photographs that failed', async () => {
  const { notify } = await import('../../activity/notify');
  vi.mocked(notify.error).mockClear();
  vi.mocked(runEditWith).mockResolvedValueOnce({
    rendered: 1,
    cancelled: 0,
    failed: 1,
    failedPaths: ['/b.NEF'],
    library: '/A.catalog',
    linked: 1,
    problems: []
  });
  mockInvoke.mockImplementation(async (command) => {
    if (command === 'open_with_prefs') return { recentApps: [], editRecipe: null };
    if (command === 'edit_with_apps') return apps;
  });
  useLibraryStore.setState({ sessionDirectory: '/A.catalog' });
  const rendered = vi.fn();
  render(<EditWithSubMenu paths={['/a.NEF', '/b.NEF']} hideContextMenu={vi.fn()} onRendered={rendered} />);
  fireEvent.click(await screen.findByRole('menuitem', { name: /^Preview/ }));
  await vi.waitFor(() => expect(notify.error).toHaveBeenCalled());
  const [text, options] = vi.mocked(notify.error).mock.lastCall!;
  expect(text).toBe('1 of 2 photographs could not be rendered.');
  expect(options?.paths).toEqual(['/b.NEF']);
  const retry = options?.actions?.find((action) => action.label === 'Try again');
  expect(retry).toBeDefined();
  const calls = vi.mocked(runEditWith).mock.calls.length;
  await retry!.run();
  expect(vi.mocked(runEditWith).mock.calls.length).toBe(calls + 1);
  const [again, recipe, program] = vi.mocked(runEditWith).mock.lastCall!;
  // The finished render of a.NEF is not made a second time (WEW3: a manual
  // re-run duplicated it).
  expect(again).toEqual(['/b.NEF']);
  expect(recipe.id).toBe('builtin-tiff16-adobe');
  expect(program).toBe('/System/Applications/Preview.app');
  expect(rendered).toHaveBeenCalledTimes(2);
  useLibraryStore.setState({ sessionDirectory: null });
});

it('Try again waits until the library of the click is open again', async () => {
  const { notify } = await import('../../activity/notify');
  vi.mocked(notify.error).mockClear();
  vi.mocked(notify.warn).mockClear();
  const failed = {
    rendered: 1,
    cancelled: 0,
    failed: 1,
    failedPaths: ['/Capture/b.NEF'],
    library: '/Shoots/A.session',
    linked: 1,
    problems: []
  };
  vi.mocked(runEditWith).mockResolvedValueOnce(failed);
  mockInvoke.mockImplementation(async (command) => {
    if (command === 'open_with_prefs') return { recentApps: [], editRecipe: null };
    if (command === 'edit_with_apps') return apps;
  });
  useLibraryStore.setState({ sessionDirectory: '/Shoots/A.session' });
  render(<EditWithSubMenu paths={['/Capture/a.NEF', '/Capture/b.NEF']} hideContextMenu={vi.fn()} />);
  fireEvent.click(await screen.findByRole('menuitem', { name: /^Preview/ }));
  await vi.waitFor(() => expect(notify.error).toHaveBeenCalledTimes(1));
  const calls = vi.mocked(runEditWith).mock.calls.length;
  // Another library is open: its edits (or none) would be rendered, so the
  // retry says which one to open and stays offered.
  useLibraryStore.setState({ sessionDirectory: '/Other.catalog' });
  await vi.mocked(notify.error).mock.lastCall![1]!.actions![0].run();
  expect(vi.mocked(runEditWith).mock.calls.length).toBe(calls);
  expect(notify.warn).toHaveBeenCalledWith(
    'Open A.session again to try again: its photos are rendered with the edits they have there.'
  );
  expect(notify.error).toHaveBeenCalledTimes(2);
  const [, again] = vi.mocked(notify.error).mock.lastCall!;
  expect(again?.paths).toEqual(['/Capture/b.NEF']);
  // Back in the click's library it renders the failed photograph.
  useLibraryStore.setState({ sessionDirectory: '/Shoots/A.session' });
  await again!.actions![0].run();
  expect(vi.mocked(runEditWith).mock.lastCall![0]).toEqual(['/Capture/b.NEF']);
  useLibraryStore.setState({ sessionDirectory: null });
});

it('on Windows says why Browse for a program… failed instead of doing nothing', async () => {
  const { notify } = await import('../../activity/notify');
  useSettingsStore.setState({ osPlatform: 'windows' });
  mockInvoke.mockImplementation(async (command) => {
    if (command === 'open_with_apps') return apps;
    if (command === 'open_with_pick') throw 'C:\\x\\tool.txt is not a program file.';
  });
  try {
    render(<OpenWithSubMenu paths={paths} hideContextMenu={vi.fn()} />);
    fireEvent.click(await screen.findByRole('menuitem', { name: /Browse for a program…/ }));
    await vi.waitFor(() =>
      expect(notify.error).toHaveBeenCalledWith('Could not choose that program: C:\\x\\tool.txt is not a program file.')
    );
    expect(mockInvoke).not.toHaveBeenCalledWith('open_with_open', expect.anything());
  } finally {
    useSettingsStore.setState({ osPlatform: '' });
  }
});

it('says Edit With is not available for photographs with generated areas', async () => {
  const { notify } = await import('../../activity/notify');
  vi.mocked(runEditWith).mockRejectedValueOnce({
    code: 'generated_areas',
    message: 'generated-areas: B.jpg, C.jpg',
    names: ['B.jpg', 'C.jpg']
  });
  mockInvoke.mockImplementation(async (command) => {
    if (command === 'open_with_prefs') return { recentApps: [], editRecipe: null };
    if (command === 'edit_with_apps') return apps;
  });
  render(<EditWithSubMenu paths={paths} hideContextMenu={vi.fn()} />);
  fireEvent.click(await screen.findByRole('menuitem', { name: /^Preview/ }));
  await vi.waitFor(() =>
    expect(notify.error).toHaveBeenCalledWith(
      'Edit With is not available for photos with generated areas yet: B.jpg, C.jpg'
    )
  );
});

it("shows another Edit With failure's message, also one that starts like the generated areas'", async () => {
  const { notify } = await import('../../activity/notify');
  vi.mocked(runEditWith).mockRejectedValueOnce({ code: 'internal', message: 'generated-areas: not the code' });
  mockInvoke.mockImplementation(async (command) => {
    if (command === 'open_with_prefs') return { recentApps: [], editRecipe: null };
    if (command === 'edit_with_apps') return apps;
  });
  render(<EditWithSubMenu paths={paths} hideContextMenu={vi.fn()} />);
  fireEvent.click(await screen.findByRole('menuitem', { name: /^Preview/ }));
  await vi.waitFor(() => expect(notify.error).toHaveBeenCalled());
  const [text] = vi.mocked(notify.error).mock.lastCall!;
  expect(text).toContain('generated-areas: not the code');
  expect(text).not.toContain('not available for photos with generated areas');
});

it.each([
  ['/Applications/Paint.app', false],
  ['C:\\Users\\test\\WindowsApps\\mspaint.exe', false],
  ['C:\\Users\\test\\WindowsApps\\mspaint.exe', true]
] as const)(
  'retries unfinished photos for %s even when completed handoff fails: %s',
  async (selectedProgram, failCompleted) => {
    const { notify } = await import('../../activity/notify');
    const run = { program: selectedProgram, photos: [paths[0], paths[1]] };
    const completed = [{ source: '/s/A.NEF', target: '/s/A_edit.tif', library: '/s' }];
    mockInvoke.mockImplementation(async (command) => {
      if (command === 'edit_with_interrupted') return [{ ...run, completed }];
      if (command === 'edit_with_finish') {
        if (failCompleted) throw new Error('finished file moved');
        return { linked: 1, problems: [] };
      }
      if (command === 'open_with_describe') return { path: run.program, name: 'Paint' };
      if (command === 'open_with_prefs') return { recentApps: [], editRecipe: 'builtin-tiff16-prophoto' };
    });
    await offerInterruptedEditWith(i18n.t);
    expect(vi.mocked(notify.warn)).toHaveBeenCalledTimes(1);
    const [title, options] = vi.mocked(notify.warn).mock.lastCall!;
    expect(title).toBe('Edit With in Paint was interrupted when Rawtonic quit. Try again to render the photos again.');
    expect(options).toMatchObject({ paths: run.photos, source: 'export' });
    // Not a resume of the old jobs: a new Edit With of the same photographs,
    // with the program chosen then and the recipe chosen now.
    await options!.actions![0].run();
    expect(mockInvoke).toHaveBeenCalledWith('edit_with_finish', { items: completed, program: run.program });
    const finishCall = mockInvoke.mock.calls.findIndex(([command]) => command === 'edit_with_finish');
    expect(mockInvoke.mock.invocationCallOrder[finishCall]).toBeLessThan(
      vi.mocked(runEditWith).mock.invocationCallOrder.at(-1)!
    );
    const [renderedPaths, recipe, program] = vi.mocked(runEditWith).mock.lastCall!;
    expect([renderedPaths, recipe.id, program]).toEqual([run.photos, 'builtin-tiff16-prophoto', run.program]);
    expect(mockInvoke.mock.calls.map((c) => c[0])).not.toContain('export_queue_resume');
  }
);

const linuxApps: AppChoices = {
  supported: true,
  apps: [
    {
      path: '/usr/share/applications/org.gimp.GIMP.desktop',
      name: 'GNU Image Manipulation Program',
      isDefault: false,
      isRecent: true,
      icon: null
    },
    {
      path: '/usr/share/applications/org.kde.gwenview.desktop',
      name: 'Gwenview',
      isDefault: true,
      isRecent: false,
      icon: null
    }
  ]
};
const [gimp, gwenview] = linuxApps.apps;

it("Settings for a program… starts from the default recipe and saves the program's own settings", async () => {
  mockInvoke.mockImplementation(async (command) => {
    if (command === 'open_with_prefs') return { recentApps: [], editRecipe: 'builtin-tiff16-adobe' };
    if (command === 'edit_with_apps') return linuxApps;
  });
  const hide = vi.fn();
  render(
    <>
      <EditWithSubMenu paths={paths} hideContextMenu={hide} />
      <EditWithSettingsDialogHost />
    </>
  );
  fireEvent.click(await screen.findByRole('menuitem', { name: 'Settings for GNU Image Manipulation Program…' }));
  expect(hide).toHaveBeenCalled();
  const dialog = await screen.findByRole('dialog', { name: 'Edit With settings for GNU Image Manipulation Program' });
  expect(dialog.textContent).toContain('uses the default recipe (TIFF 16-bit · Adobe RGB)');
  // No own settings yet: nothing to go back to.
  expect(within(dialog).queryByRole('button', { name: 'Use default recipe' })).toBeNull();
  fireEvent.change(within(dialog).getByLabelText('Color space'), { target: { value: 'proPhoto' } });
  fireEvent.click(within(dialog).getByRole('button', { name: 'Save' }));
  // The exact arguments the Rust side reads (program_settings_cross_ipc_as_the_dialog_sends_them).
  await vi.waitFor(() =>
    expect(mockInvoke).toHaveBeenCalledWith(
      'edit_with_set_program_settings',
      programSettingsFixture.editWithSetProgramSettings
    )
  );
  await vi.waitFor(() => expect(screen.queryByRole('dialog')).toBeNull());
  // The default recipe is left as it was.
  expect(mockInvoke).not.toHaveBeenCalledWith('edit_with_set_recipe', expect.anything());
});

it('renders for a program with its own settings with them, and for the others with the default recipe', async () => {
  mockInvoke.mockImplementation(async (command) => {
    if (command === 'open_with_prefs') return programSettingsFixture.openWithPrefs;
    if (command === 'edit_with_apps') return linuxApps;
  });
  render(<EditWithSubMenu paths={paths} hideContextMenu={vi.fn()} />);
  // Its own settings are shown in its row; the default recipe is still checked.
  const own = await screen.findByRole('menuitem', { name: /^Gwenview/ });
  expect(own.textContent).toBe('GwenviewDefault · JPEG · sRGB');
  expect(screen.getByRole('menuitem', { name: /^GNU Image/ }).textContent).toBe('GNU Image Manipulation Program');
  expect(screen.getByRole('menuitemradio', { checked: true }).textContent).toBe('TIFF 16-bit · Adobe RGB');
  expect(mockInvoke).toHaveBeenCalledWith('edit_with_apps', { extension: 'tiff' });

  fireEvent.click(own);
  await vi.waitFor(() => expect(vi.mocked(runEditWith).mock.lastCall![2]).toBe(gwenview.path));
  const jpeg = vi.mocked(runEditWith).mock.lastCall![1];
  expect([jpeg.outputFormat, jpeg.settings.colorSpace, jpeg.settings.jpegQuality]).toEqual(['jpg', 'srgb', 85]);

  fireEvent.click(screen.getByRole('menuitem', { name: /^GNU Image/ }));
  await vi.waitFor(() => expect(vi.mocked(runEditWith).mock.lastCall![2]).toBe(gimp.path));
  const tiff = vi.mocked(runEditWith).mock.lastCall![1];
  expect([tiff.id, tiff.outputFormat, tiff.settings.colorSpace, tiff.settings.tiffBitDepth]).toEqual([
    'builtin-tiff16-adobe',
    'tiff',
    'adobeRgb',
    16
  ]);
});

it("Use default recipe removes a program's own settings", async () => {
  mockInvoke.mockImplementation(async (command) => {
    if (command === 'open_with_prefs') return programSettingsFixture.openWithPrefs;
    if (command === 'edit_with_apps') return linuxApps;
  });
  render(
    <>
      <EditWithSubMenu paths={paths} hideContextMenu={vi.fn()} />
      <EditWithSettingsDialogHost />
    </>
  );
  fireEvent.click(await screen.findByRole('menuitem', { name: 'Settings for Gwenview…' }));
  const dialog = await screen.findByRole('dialog', { name: 'Edit With settings for Gwenview' });
  expect(dialog.textContent).toContain('These settings are used only for Gwenview.');
  expect((within(dialog).getByLabelText('Format') as HTMLSelectElement).value).toBe('jpg');
  fireEvent.click(within(dialog).getByRole('button', { name: 'Use default recipe' }));
  await vi.waitFor(() =>
    expect(mockInvoke).toHaveBeenCalledWith('edit_with_set_program_settings', programSettingsFixture.useDefault)
  );
});

it("the menu bar's Edit With offers each program's settings and renders with them", async () => {
  mockInvoke.mockImplementation(async (command) => {
    if (command === 'open_with_prefs') return programSettingsFixture.openWithPrefs;
    if (command === 'edit_with_apps') return linuxApps;
  });
  const options = await editWithOptions(paths, i18n.t, []);
  const settings = options.find((o) => o.label === 'Settings for a program');
  expect(settings?.submenu?.map((o) => o.label)).toEqual([
    'Settings for GNU Image Manipulation Program…',
    'Settings for Gwenview…'
  ]);
  const entry = options.find((o) => o.label?.startsWith('Gwenview'));
  expect(entry?.label).toBe('Gwenview · Default · JPEG · sRGB');
  entry!.onClick!();
  await vi.waitFor(() => expect(vi.mocked(runEditWith).mock.lastCall![2]).toBe(gwenview.path));
  expect(vi.mocked(runEditWith).mock.lastCall![1].outputFormat).toBe('jpg');
});
