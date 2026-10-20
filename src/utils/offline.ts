import { invoke } from '@tauri-apps/api/core';
import { listen } from '@tauri-apps/api/event';
import type { TFunction } from 'i18next';
import { FileSearch, FolderSearch, Unlink } from 'lucide-react';
import { toast } from 'react-toastify';
import {
  Invokes,
  type ImageFile,
  type OfflineEntry,
  type Option,
  type RelinkReport,
} from '../components/ui/AppProperties';
import { openDialog } from './fileDialog';
import { useLibraryStore } from '../store/useLibraryStore';

/**
 * Offline photographs: their file is missing or on a disconnected drive. The
 * session keeps them (edits, metadata, albums) and they open on their
 * previews; they are listed in their folder and albums with an Offline badge
 * and together in the Missing Photographs view (Lightroom's "!" badge and
 * Missing Photographs collection), and relinked with Locate photo / Locate
 * folder (Lightroom's Find Missing Photo with "find nearby", Find Missing
 * Folder). A relink only reads files; nothing is moved or deleted.
 */

/** `activeAlbumId` of the Missing Photographs view. */
export const MISSING_VIEW_ID = 'session-missing';
/** Fired after a relink, so the Albums tool and the session bar update. */
export const OFFLINE_CHANGED = 'rawtonic:offline-changed';

export type SelectView = (id: string, name: string, paths: string[], preserveEditor?: boolean) => void;

/** Fired when a check found other offline photographs than the one before
 * (the library's missing flags changed; a drive followed to another mount
 * point), so counts update. A `CustomEvent` with the entries as `detail`. */
export const OFFLINE_COUNTED = 'rawtonic:offline-counted';

/** The offline photographs the last check found, which the views count. */
const counted = new Set<string>();

/** The session's offline photographs (the files are checked first). Views
 * count again when the set changed, or always with `announce`. */
export async function offlinePhotographs(directory: string, announce = false): Promise<OfflineEntry[]> {
  const asked = useLibraryStore.getState().sessionDirectory;
  const entries = (await invoke<OfflineEntry[]>(Invokes.SessionOffline, { directory })) ?? [];
  // Another library was opened meanwhile: this answer is not about the one
  // the views show, so it neither replaces their count nor is announced.
  if (useLibraryStore.getState().sessionDirectory !== asked) return entries;
  const paths = new Set(entries.map((entry) => entry.path));
  const changed = paths.size !== counted.size || [...paths].some((path) => !counted.has(path));
  counted.clear();
  for (const path of paths) counted.add(path);
  if (changed || announce) window.dispatchEvent(new CustomEvent<OfflineEntry[]>(OFFLINE_COUNTED, { detail: entries }));
  return entries;
}

/** After a file operation or its undo the catalog may hold other
 * photographs offline or missing — also when it caught up on an earlier
 * operation whose sync had failed: the open library checks its files once
 * and the session summary, Missing Photographs and the folders count
 * again (their summary reads the missing marks this check renews). */
export async function recountAfterFileOperation(): Promise<void> {
  const directory = useLibraryStore.getState().sessionDirectory;
  if (!directory) return;
  try {
    await offlinePhotographs(directory, true);
  } catch (err) {
    console.error('Offline photographs could not be counted again:', err);
  }
}

/** The event the app sends when a drive was connected, ejected or mounted
 * elsewhere (session_bridge.rs `VOLUMES_CHANGED`). */
export const VOLUMES_CHANGED = 'volumes-changed';
/** Fired in the webview after a drive change, so the shown photographs are
 * listed again (their Offline badges follow at once). Separate from
 * `OFFLINE_CHANGED`, which a listing itself may fire. */
export const DRIVES_CHANGED = 'rawtonic:drives-changed';

/**
 * Watches the mounted drives while a library is open: the app tells the
 * webview as soon as the system reports a drive connected, ejected or
 * mounted elsewhere (mount notifications; a poll of the mount list only
 * where the system has none), and the views check the offline photographs
 * and folders again (a drive mounted elsewhere is followed there by that
 * check). Returns the stop function.
 */
export function watchVolumes(): () => void {
  let stopped = false;
  let unlisten: (() => void) | undefined;
  void listen(VOLUMES_CHANGED, () => {
    if (stopped) return;
    window.dispatchEvent(new Event(OFFLINE_CHANGED));
    window.dispatchEvent(new Event(DRIVES_CHANGED));
  })
    .then((stop) => {
      if (stopped) stop();
      else unlisten = stop;
    })
    .catch(() => {
      // Outside the app (no event bridge): nothing to watch.
    });
  return () => {
    stopped = true;
    unlisten?.();
  };
}

/** The library paths of offline photographs and their virtual copies, as
 * the grid lists them. */
export function offlinePaths(entries: OfflineEntry[]): string[] {
  return entries.flatMap((entry) => [entry.path, ...(entry.copies ?? []).map((copy) => `${entry.path}?vc=${copy}`)]);
}

/**
 * A folder listing found photographs offline that the views do not count
 * yet (moved or deleted in Finder, a drive ejected), or counted ones that are
 * back: the session bar, Missing Photographs and the folders recount.
 * Returns whether it asked them to.
 */
export function noteListedOffline(files: Pick<ImageFile, 'path' | 'is_offline'>[]): boolean {
  const changed = files.some((file) => Boolean(file.is_offline) !== counted.has(file.path.split('?vc=')[0]));
  if (changed) window.dispatchEvent(new Event(OFFLINE_CHANGED));
  return changed;
}

/** Shows the Missing Photographs view in the grid; returns how many there are. */
export async function openMissingView(
  directory: string,
  name: string,
  select: SelectView,
  preserveEditor = false,
): Promise<number> {
  const asked = useLibraryStore.getState().sessionDirectory;
  const entries = await offlinePhotographs(directory);
  if (useLibraryStore.getState().sessionDirectory !== asked) return entries.length;
  select(MISSING_VIEW_ID, name, offlinePaths(entries), preserveEditor);
  return entries.length;
}

const separator = (path: string) => Math.max(path.lastIndexOf('/'), path.lastIndexOf('\\'));
export const folderOf = (path: string) => path.slice(0, Math.max(0, separator(path)));
export const fileNameOf = (path: string) => path.slice(separator(path) + 1);

/** The tile's tooltip for an offline photograph. */
export function offlineTitle(t: TFunction, file: Pick<ImageFile, 'offline_volume'>): string {
  return file.offline_volume
    ? t('offline.badgeTitleVolume', { volume: file.offline_volume })
    : t('offline.badgeTitleMissing');
}

/** One line on what a relink did. */
export function relinkSummary(t: TFunction, report: RelinkReport): string {
  const parts = [t('offline.toasts.relinked', { count: report.relinked.length })];
  if (report.mismatched.length) parts.push(t('offline.toasts.mismatched', { count: report.mismatched.length }));
  if (report.taken.length) parts.push(t('offline.toasts.taken', { count: report.taken.length }));
  if (report.not_found) parts.push(t('offline.toasts.notFound', { count: report.not_found }));
  return parts.join(' · ');
}

function announce(t: TFunction, report: RelinkReport) {
  const text = relinkSummary(t, report);
  if (report.relinked.length) toast.success(text, { autoClose: 6000 });
  else toast.warning(text, { autoClose: 8000 });
  window.dispatchEvent(new Event(OFFLINE_CHANGED));
}

/** Asks for the photograph's file and relinks it, with the other offline
 * photographs of its folder found beside it. Null when cancelled. */
export async function locatePhotograph(
  t: TFunction,
  directory: string,
  path: string,
): Promise<RelinkReport | null> {
  const source = path.split('?vc=')[0];
  const picked = await openDialog({
    title: t('offline.dialog.locateFile', { name: fileNameOf(source) }),
    defaultPath: folderOf(source) || undefined,
    multiple: false,
    directory: false,
  });
  if (typeof picked !== 'string') return null;
  const report = await invoke<RelinkReport>(Invokes.SessionRelink, { directory, path: source, to: picked, nearby: true });
  announce(t, report);
  return report;
}

/** Asks where the photograph's folder is now and relinks every offline
 * photograph registered in it or below it. Null when cancelled. */
export async function locateFolder(t: TFunction, directory: string, path: string): Promise<RelinkReport | null> {
  return locateFolderOf(t, directory, folderOf(path.split('?vc=')[0]));
}

/** Asks where the folder `from` is now (Lightroom's Find Missing Folder)
 * and relinks every offline photograph registered in it or below it. Null
 * when cancelled. */
export async function locateFolderOf(t: TFunction, directory: string, from: string): Promise<RelinkReport | null> {
  const picked = await openDialog({
    title: t('offline.dialog.locateFolder', { folder: from }),
    multiple: false,
    directory: true,
  });
  if (typeof picked !== 'string') return null;
  const report = await invoke<RelinkReport>(Invokes.SessionRelinkFolder, { directory, from, to: picked });
  announce(t, report);
  return report;
}

/**
 * The thumbnail menu's entries for offline photographs: Locate photo (one
 * photograph), Locate folder, and the Missing Photographs view. Empty when no
 * selected photograph is offline or no session is open.
 */
export function offlineMenu(
  t: TFunction,
  options: {
    directory: string | null;
    selection: ImageFile[];
    /** After a relink (also one that found nothing), with what it did. */
    onRelinked: (report: RelinkReport) => void;
    onShowMissing?: () => void;
  },
): Option[] {
  const { directory, selection, onRelinked, onShowMissing } = options;
  const offline = selection.filter((file) => file.is_offline);
  if (!directory || offline.length === 0) return [];
  const first = offline[0];
  const run = (action: () => Promise<RelinkReport | null>) => () => {
    action()
      .then((report) => {
        if (report) onRelinked(report);
      })
      .catch((err) => toast.error(t('offline.toasts.failed', { err: String(err) }), { autoClose: false }));
  };
  return [
    {
      label: t('offline.menu.locate'),
      icon: FileSearch,
      disabled: offline.length !== 1,
      onClick: run(() => locatePhotograph(t, directory, first.path)),
    },
    {
      label: t('offline.menu.locateFolder'),
      icon: FolderSearch,
      onClick: run(() => locateFolder(t, directory, first.path)),
    },
    ...(onShowMissing ? [{ label: t('offline.menu.showMissing'), icon: Unlink, onClick: onShowMissing }] : []),
  ];
}

/**
 * Where `paths` (library paths, virtual copies with `?vc=`) are after a
 * relink: relinked photographs (and their virtual copies) get their new
 * place, others stay as they are.
 */
export function relinkedPaths(report: RelinkReport, paths: string[]): string[] {
  const moved = new Map(report.relinked);
  return paths.map((path) => {
    const at = path.indexOf('?vc=');
    const source = at < 0 ? path : path.slice(0, at);
    const to = moved.get(source);
    return to === undefined ? path : to + (at < 0 ? '' : path.slice(at));
  });
}

/** The relinked photograph to show after a relink: the first of `preferred`
 * (new places: the open one, the active one) that it moved, else the first
 * it moved. Undefined when it moved none. */
export function locatedPhotograph(report: RelinkReport, preferred: Array<string | null>): string | undefined {
  const moved = new Set(report.relinked.map(([, to]) => to));
  return (
    preferred.find((path): path is string => Boolean(path && moved.has(path.split('?vc=')[0]))) ??
    report.relinked[0]?.[1]
  );
}

/** A folder holding offline photographs, for the Folders tool. */
export interface OfflineFolder {
  path: string;
  /** Offline photographs directly in it. */
  count: number;
  /** The folder itself is gone (renamed, moved, drive disconnected). */
  missing: boolean;
  volume: string | null;
}

/** The folders of the offline photographs, in path order. */
export function offlineFolders(entries: OfflineEntry[]): OfflineFolder[] {
  const folders = new Map<string, OfflineFolder>();
  for (const entry of entries) {
    const path = folderOf(entry.path);
    const folder = folders.get(path) ?? { path, count: 0, missing: entry.folder_missing, volume: entry.volume };
    folder.count += 1;
    folders.set(path, folder);
  }
  return [...folders.values()].sort((a, b) => a.path.localeCompare(b.path));
}

/** How many offline photographs are in `path` or below it. */
export function offlineBelow(folders: OfflineFolder[], path: string): number {
  return folders
    .filter((folder) => folder.path === path || folder.path.startsWith(`${path}/`) || folder.path.startsWith(`${path}\\`))
    .reduce((sum, folder) => sum + folder.count, 0);
}
