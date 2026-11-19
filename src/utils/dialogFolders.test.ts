import { afterEach, beforeEach, expect, it, vi } from 'vitest';
import { open, save } from '@tauri-apps/plugin-dialog';
import { pictureDir } from '@tauri-apps/api/path';
import { openDialog, saveDialog } from './fileDialog';
import { answerFolder, setDialogLibraryFolder } from './dialogFolders';

vi.mock('@tauri-apps/plugin-dialog', () => ({ open: vi.fn(), save: vi.fn() }));
vi.mock('@tauri-apps/api/path', () => ({ pictureDir: vi.fn(), homeDir: vi.fn() }));

beforeEach(() => {
  window.localStorage.clear();
  setDialogLibraryFolder(null);
  vi.mocked(pictureDir).mockResolvedValue('/home/ana/Pictures');
});
afterEach(() => vi.clearAllMocks());

it('opens at Pictures, never at "Recently used", when nothing is known (UXR4L-01)', async () => {
  vi.mocked(open).mockResolvedValue(null);
  await openDialog({ title: 'Choose where the new library is created', directory: true, purpose: 'library' });
  expect(open).toHaveBeenCalledWith({
    title: 'Choose where the new library is created',
    directory: true,
    defaultPath: '/home/ana/Pictures'
  });
});

it('opens at the library folder, then where the same purpose was last answered', async () => {
  setDialogLibraryFolder('/photos/Wedding');
  vi.mocked(open).mockResolvedValueOnce(['/cards/DCIM/100/A.NEF', '/cards/DCIM/100/B.NEF']);
  await openDialog({ title: 'Import', multiple: true, purpose: 'ingest' });
  expect(vi.mocked(open).mock.calls[0][0]).toMatchObject({ defaultPath: '/photos/Wedding' });

  vi.mocked(open).mockResolvedValueOnce('/exports/Client');
  await openDialog({ title: 'Export to', directory: true, purpose: 'export' });
  // Its own purpose has no folder yet: the last folder of any dialog.
  expect(vi.mocked(open).mock.calls[1][0]).toMatchObject({ defaultPath: '/cards/DCIM/100' });

  vi.mocked(open).mockResolvedValueOnce(null);
  await openDialog({ title: 'Import', multiple: true, purpose: 'ingest' });
  expect(vi.mocked(open).mock.calls[2][0]).toMatchObject({ defaultPath: '/cards/DCIM/100' });
  vi.mocked(open).mockResolvedValueOnce(null);
  await openDialog({ title: 'Export to', directory: true, purpose: 'export' });
  expect(vi.mocked(open).mock.calls[3][0]).toMatchObject({ defaultPath: '/exports/Client' });
});

it("keeps the caller's start folder and puts a save dialog's file name into the remembered folder", async () => {
  window.localStorage.setItem('rawtonic.dialogFolders', JSON.stringify({ presets: '/home/ana/Presets' }));
  vi.mocked(open).mockResolvedValue(null);
  await openDialog({ title: 'Locate A.NEF', defaultPath: '/Volumes/Card/DCIM' });
  expect(vi.mocked(open).mock.calls[0][0]).toMatchObject({ defaultPath: '/Volumes/Card/DCIM' });

  vi.mocked(save).mockResolvedValue('/home/ana/Presets/Warm.rrpreset');
  await saveDialog({ title: 'Export preset', defaultPath: 'Warm.rrpreset', purpose: 'presets' });
  expect(save).toHaveBeenCalledWith({ title: 'Export preset', defaultPath: '/home/ana/Presets/Warm.rrpreset' });
});

it('a remembered folder survives no storage', async () => {
  vi.spyOn(Storage.prototype, 'getItem').mockImplementation(() => {
    throw new Error('blocked');
  });
  vi.mocked(open).mockResolvedValue('/x/y.NEF');
  expect(await openDialog({ title: 'Locate' })).toBe('/x/y.NEF');
  expect(vi.mocked(open).mock.calls[0][0]).toMatchObject({ defaultPath: '/home/ana/Pictures' });
});

it('names the folder of an answer', () => {
  expect(answerFolder('/a/b/c.NEF', false)).toBe('/a/b');
  expect(answerFolder('/c.NEF', false)).toBe('/');
  expect(answerFolder('C:\\Photos\\c.NEF', false)).toBe('C:\\Photos');
  expect(answerFolder('C:\\c.NEF', false)).toBe('C:\\');
  expect(answerFolder('/a/b', true)).toBe('/a/b');
  expect(answerFolder('c.NEF', false)).toBeNull();
});

it('offers a filename in the remembered Windows folder with one separator at the join', async () => {
  setDialogLibraryFolder('\\\\nas\\Exports\\\\');
  vi.mocked(save).mockResolvedValueOnce(null);
  await saveDialog({ title: 'Export', defaultPath: 'Photo.jpg' });
  expect(save).toHaveBeenCalledWith({ title: 'Export', defaultPath: '\\\\nas\\Exports\\Photo.jpg' });
});
