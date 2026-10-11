import { afterEach, expect, it, vi } from 'vitest';
import { invoke } from '@tauri-apps/api/core';
import { listen } from '@tauri-apps/api/event';
import { open } from '@tauri-apps/plugin-dialog';
import type { TFunction } from 'i18next';
import { toast } from 'react-toastify';
import { Invokes, type ImageFile, type RelinkReport } from '../components/ui/AppProperties';
import {
  folderOf,
  locatedPhotograph,
  MISSING_VIEW_ID,
  offlineBelow,
  offlineFolders,
  OFFLINE_CHANGED,
  OFFLINE_COUNTED,
  noteListedOffline,
  offlineMenu,
  offlinePhotographs,
  offlineTitle,
  openMissingView,
  relinkedPaths,
  relinkSummary,
  DRIVES_CHANGED,
  VOLUMES_CHANGED,
  watchVolumes,
} from './offline';

vi.mock('@tauri-apps/api/core', () => ({ invoke: vi.fn() }));
vi.mock('@tauri-apps/api/event', () => ({ listen: vi.fn() }));
vi.mock('@tauri-apps/plugin-dialog', () => ({ open: vi.fn() }));
vi.mock('react-toastify', () => ({ toast: { error: vi.fn(), success: vi.fn(), warning: vi.fn() } }));

/** Returns the key and its options, so the test sees what was asked for. */
const t = ((key: string, options?: Record<string, unknown>) =>
  options ? `${key} ${JSON.stringify(options)}` : key) as unknown as TFunction;

afterEach(() => vi.clearAllMocks());

const flush = () => new Promise((resolve) => setTimeout(resolve, 0));

const file = (path: string, offline: boolean, volume: string | null = null) =>
  ({ path, is_offline: offline, offline_volume: volume, is_raw: true }) as unknown as ImageFile;

const report = (relinked: number, extra: Partial<RelinkReport> = {}): RelinkReport => ({
  relinked: Array.from({ length: relinked }, (_, i) => [`/old/${i}.NEF`, `/new/${i}.NEF`]),
  mismatched: [],
  taken: [],
  not_found: 0,
  ...extra,
});

it('offers nothing when no selected photograph is offline or no session is open', () => {
  const onRelinked = vi.fn();
  expect(offlineMenu(t, { directory: '/s', selection: [file('/s/Capture/A.NEF', false)], onRelinked })).toEqual([]);
  expect(offlineMenu(t, { directory: null, selection: [file('/s/Capture/A.NEF', true)], onRelinked })).toEqual([]);
});

it('locates one photograph with its neighbours and refreshes the view', async () => {
  vi.mocked(open).mockResolvedValue('/Volumes/Archive/2026/A.NEF');
  vi.mocked(invoke).mockResolvedValue(report(3));
  const onRelinked = vi.fn();
  const changed = vi.fn();
  window.addEventListener(OFFLINE_CHANGED, changed);
  const onShowMissing = vi.fn();
  const [locate, locateFolder, showMissing] = offlineMenu(t, {
    directory: '/cat',
    selection: [file('/Volumes/Card/2026/A.NEF', true)],
    onRelinked,
    onShowMissing,
  });
  expect(locate.label).toBe('offline.menu.locate');
  expect(locate.disabled).toBe(false);
  expect(locateFolder.label).toBe('offline.menu.locateFolder');
  locate.onClick!();
  await flush();
  expect(vi.mocked(open).mock.calls[0][0]).toMatchObject({ directory: false, defaultPath: '/Volumes/Card/2026' });
  expect(invoke).toHaveBeenCalledWith(Invokes.SessionRelink, {
    directory: '/cat',
    path: '/Volumes/Card/2026/A.NEF',
    to: '/Volumes/Archive/2026/A.NEF',
    nearby: true,
  });
  expect(toast.success).toHaveBeenCalledWith('offline.toasts.relinked {"count":3}', expect.anything());
  expect(onRelinked).toHaveBeenCalledExactlyOnceWith(report(3));
  expect(changed).toHaveBeenCalledOnce();
  window.removeEventListener(OFFLINE_CHANGED, changed);
  showMissing.onClick!();
  expect(onShowMissing).toHaveBeenCalledOnce();
});

it('locates a folder for the old folder of the photograph', async () => {
  vi.mocked(open).mockResolvedValue('/Volumes/Archive/2026');
  vi.mocked(invoke).mockResolvedValue(report(0, { not_found: 2, mismatched: ['/Volumes/Archive/2026/B.NEF'] }));
  const onRelinked = vi.fn();
  const [locate, locateFolder] = offlineMenu(t, {
    directory: '/cat',
    selection: [file('/Volumes/Card/2026/A.NEF', true), file('/Volumes/Card/2026/B.NEF?vc=1a2b3c', true)],
    onRelinked,
  });
  expect(locate.disabled).toBe(true);
  locateFolder.onClick!();
  await flush();
  expect(invoke).toHaveBeenCalledWith(Invokes.SessionRelinkFolder, {
    directory: '/cat',
    from: '/Volumes/Card/2026',
    to: '/Volumes/Archive/2026',
  });
  expect(toast.warning).toHaveBeenCalledWith(
    'offline.toasts.relinked {"count":0} · offline.toasts.mismatched {"count":1} · offline.toasts.notFound {"count":2}',
    expect.anything(),
  );
});

it('does nothing when the dialog is cancelled and reports failures', async () => {
  vi.mocked(open).mockResolvedValue(null);
  const onRelinked = vi.fn();
  const [locate] = offlineMenu(t, { directory: '/cat', selection: [file('/x/A.NEF', true)], onRelinked });
  locate.onClick!();
  await flush();
  expect(invoke).not.toHaveBeenCalled();
  expect(onRelinked).not.toHaveBeenCalled();

  vi.mocked(open).mockResolvedValue('/y/A.NEF');
  vi.mocked(invoke).mockRejectedValue('is not this photograph');
  locate.onClick!();
  await flush();
  expect(toast.error).toHaveBeenCalledWith('offline.toasts.failed {"err":"is not this photograph"}', expect.anything());
  expect(onRelinked).not.toHaveBeenCalled();
});

it('opens the Missing Photographs view with the offline paths and their virtual copies', async () => {
  vi.mocked(invoke).mockResolvedValue([
    { path: '/Volumes/Card/A.NEF', volume: 'Card', mtime: 1, copies: ['0a0a0a'] },
    { path: '/s/Capture/B.NEF', volume: null, mtime: 2 },
  ]);
  const select = vi.fn();
  expect(await openMissingView('/s', 'Missing', select)).toBe(2);
  expect(invoke).toHaveBeenCalledWith(Invokes.SessionOffline, { directory: '/s' });
  expect(select).toHaveBeenCalledWith(
    MISSING_VIEW_ID,
    'Missing',
    ['/Volumes/Card/A.NEF', '/Volumes/Card/A.NEF?vc=0a0a0a', '/s/Capture/B.NEF'],
    false,
  );
});

it('groups offline photographs by folder and counts them below a folder', () => {
  const folders = offlineFolders([
    { path: '/s/Capture/A.NEF', volume: null, mtime: 1, folder_missing: false },
    { path: '/s/Capture/Day 2/B.NEF', volume: 'Card', mtime: 1, folder_missing: true },
    { path: '/s/Capture/Day 2/C.NEF', volume: 'Card', mtime: 1, folder_missing: true },
    { path: '/s/Capture 2/D.NEF', volume: null, mtime: 1, folder_missing: false },
  ]);
  expect(folders.map((f) => [f.path, f.count, f.missing])).toEqual([
    ['/s/Capture', 1, false],
    ['/s/Capture 2', 1, false],
    ['/s/Capture/Day 2', 2, true],
  ]);
  expect(offlineBelow(folders, '/s/Capture')).toBe(3);
  expect(offlineBelow(folders, '/s')).toBe(4);
  expect(offlineBelow(folders, '/s/Selects')).toBe(0);
});

it('names the disconnected drive in the badge', () => {
  expect(offlineTitle(t, { offline_volume: 'Card' })).toBe('offline.badgeTitleVolume {"volume":"Card"}');
  expect(offlineTitle(t, { offline_volume: null })).toBe('offline.badgeTitleMissing');
  expect(relinkSummary(t, report(1, { taken: ['/a'] }))).toBe(
    'offline.toasts.relinked {"count":1} · offline.toasts.taken {"count":1}',
  );
  expect(folderOf('C:\\Photos\\A.NEF')).toBe('C:\\Photos');
});

it('recounts when a listing finds photographs offline or back that the views do not count', async () => {
  const changed = vi.fn();
  window.addEventListener(OFFLINE_CHANGED, changed);
  vi.mocked(invoke).mockResolvedValue([{ path: '/s/Capture/A.NEF' }]);
  await offlinePhotographs('/s');
  const listing = (offline: string[]) =>
    ['/s/Capture/A.NEF', '/s/Capture/A.NEF?vc=1', '/s/Capture/B.NEF'].map((path) => ({
      path,
      is_offline: offline.includes(path.split('?vc=')[0]),
    }));
  // As counted: nothing to do.
  expect(noteListedOffline(listing(['/s/Capture/A.NEF']))).toBe(false);
  // B was moved in Finder: the listing sees it first.
  expect(noteListedOffline(listing(['/s/Capture/A.NEF', '/s/Capture/B.NEF']))).toBe(true);
  // A is back.
  expect(noteListedOffline(listing([]))).toBe(true);
  expect(changed).toHaveBeenCalledTimes(2);
  window.removeEventListener(OFFLINE_CHANGED, changed);

  // The recount tells the session bar when the set changed, and only then.
  const counted = vi.fn();
  window.addEventListener(OFFLINE_COUNTED, counted);
  vi.mocked(invoke).mockResolvedValue([{ path: '/s/Capture/A.NEF' }, { path: '/s/Capture/B.NEF' }]);
  await offlinePhotographs('/s');
  await offlinePhotographs('/s');
  expect(counted).toHaveBeenCalledOnce();
  expect((counted.mock.calls[0][0] as CustomEvent).detail).toHaveLength(2);
  window.removeEventListener(OFFLINE_COUNTED, counted);
});

it('checks again as soon as the app reports a drive connected, ejected or mounted elsewhere', async () => {
  const changed = vi.fn();
  window.addEventListener(OFFLINE_CHANGED, changed);
  const unlisten = vi.fn();
  let handler: (() => void) | undefined;
  vi.mocked(listen).mockImplementation(async (event, callback) => {
    expect(event).toBe(VOLUMES_CHANGED);
    handler = callback as () => void;
    return unlisten;
  });
  const stop = watchVolumes();
  await flush();
  expect(changed).not.toHaveBeenCalled();
  const drives = vi.fn();
  window.addEventListener(DRIVES_CHANGED, drives);
  handler?.();
  handler?.();
  expect(changed).toHaveBeenCalledTimes(2);
  // The shown photographs are listed again too.
  expect(drives).toHaveBeenCalledTimes(2);
  window.removeEventListener(DRIVES_CHANGED, drives);
  // No polling: nothing is asked of the app between changes.
  expect(invoke).not.toHaveBeenCalled();
  stop();
  expect(unlisten).toHaveBeenCalledOnce();
  handler?.();
  expect(changed).toHaveBeenCalledTimes(2);
  window.removeEventListener(OFFLINE_CHANGED, changed);
});

it('stops a watch that ends before the app answered', async () => {
  const unlisten = vi.fn();
  vi.mocked(listen).mockResolvedValue(unlisten);
  const stop = watchVolumes();
  stop();
  await flush();
  expect(unlisten).toHaveBeenCalledOnce();
});

it('shows the photograph the user located from after a relink', () => {
  const done = report(0, {
    relinked: [
      ['/d/Shoot/Day B/C.NEF', '/c/Shoot/Day B/C.NEF'],
      ['/d/Shoot/A.NEF', '/c/Shoot/A.NEF'],
    ],
  });
  // Locate Folder from A: A's folder, not the first relinked (a subfolder).
  expect(locatedPhotograph(done, [null, '/c/Shoot/A.NEF?vc=1'])).toBe('/c/Shoot/A.NEF?vc=1');
  expect(locatedPhotograph(done, ['/elsewhere/X.NEF', null])).toBe('/c/Shoot/Day B/C.NEF');
  expect(locatedPhotograph(report(0), ['/c/Shoot/A.NEF'])).toBeUndefined();
});

it('maps a selection to the places the relink found, virtual copies included', () => {
  const done = report(0, {
    relinked: [
      ['/s/Capture/Day1/A.NEF', '/s/Capture/Moved/A.NEF'],
      ['/s/Capture/Day1/B.NEF', '/s/Capture/Moved/B.NEF'],
    ],
  });
  expect(
    relinkedPaths(done, ['/s/Capture/Day1/A.NEF', '/s/Capture/Day1/B.NEF?vc=2', '/s/Capture/Day2/C.NEF']),
  ).toEqual(['/s/Capture/Moved/A.NEF', '/s/Capture/Moved/B.NEF?vc=2', '/s/Capture/Day2/C.NEF']);
});
