import { joinPath, pathSeparator } from './path';
import { pathKey } from './pathKey';
import type { FolderTree } from './folderTree';
import type { OfflineFolder } from './offline';

/**
 * The Folders panel of a catalog: folders grouped under the drive they are
 * on, as Lightroom's Folders panel and Capture One's catalog Folders list
 * them. Each drive shows whether it is connected and its free space; under
 * it the library's top-level folders and every catalog folder outside them
 * (where photographs were found, followed to another mount point or
 * relinked to), by name (by their path on the drive where two names would
 * be the same; Lightroom shows two alike there). Folders gone from disk
 * (renamed, moved, on a disconnected drive) stay listed at their place with
 * a missing marker; nothing is removed from the catalog.
 */

/** A catalog folder (`catalog_folders`, rawtonic_catalog::folders::CatalogFolder). */
export interface CatalogFolder {
  path: string;
  /** Registered photographs in it. */
  photos: number;
  /** Of those, offline at the last check. */
  offline: number;
  /** The folder is there now. */
  exists: boolean;
}

/** A drive and the catalog folders on it (rawtonic_catalog::folders::VolumeFolders). */
export interface VolumeFolders {
  key: string;
  id: string | null;
  name: string;
  kind: 'startup' | 'local' | 'removable' | 'network';
  mount_point: string;
  online: boolean;
  free_bytes: number | null;
  total_bytes: number | null;
  folders: CatalogFolder[];
  /** The library's top-level folders on it. */
  roots: string[];
}

/** What `catalog_folders` answers: drives for a catalog, none for a session. */
export interface CatalogFoldersView {
  catalog: boolean;
  volumes: VolumeFolders[];
  /** Photographs copied into the catalog's own folder, by folder (absent in older answers). */
  internal?: { path: string; photos: number }[];
}

/** A drive with its folder trees, ready to show. */
export interface VolumeSection {
  volume: VolumeFolders;
  trees: FolderTree[];
}

function folderPrefix(folder: string): string {
  const sep = pathSeparator(folder);
  return folder.endsWith(sep) ? folder : folder + sep;
}

/** The relative path within a folder, or null outside it or at its root. */
export function relativePathBelow(path: string, folder: string): string | null {
  const candidate = pathKey(path);
  const prefix = folderPrefix(pathKey(folder));
  return candidate.length > prefix.length && candidate.startsWith(prefix) ? candidate.slice(prefix.length) : null;
}

/** Whether the path lies strictly below the folder. */
export function isBelow(path: string, folder: string): boolean {
  return relativePathBelow(path, folder) !== null;
}

const baseName = (path: string) => path.split(/[\\/]/).filter(Boolean).pop() ?? path;

/** How a top-level folder is labelled: its path on the drive, e.g.
 * `Shoots/2026` for `/Volumes/Card/Shoots/2026`; the drive's own root by
 * its name. */
export function labelOnVolume(path: string, mountPoint: string): string {
  if (path === mountPoint) return baseName(path) || path;
  return relativePathBelow(path, mountPoint) ?? path;
}

/** The outermost of `paths` (none below another), in path order. */
export function outermost(paths: string[]): string[] {
  const unique = [...new Set(paths)].sort((a, b) => a.localeCompare(b));
  return unique.filter((path) => !unique.some((other) => isBelow(path, other)));
}

/** A drive's top-level folders: its roots and the catalog folders outside them. */
export function volumeTops(volume: VolumeFolders): string[] {
  return outermost([...volume.roots, ...volume.folders.map((folder) => folder.path)]);
}

/** Top-level catalog folders on disk that are not library roots: their disk
 * trees are loaded like the roots' (the `catalogTrees` of the library store). */
export function catalogTreePaths(view: CatalogFoldersView | null, rootPaths: string[]): string[] {
  if (!view?.catalog) return [];
  return view.volumes.flatMap((volume) =>
    volumeTops(volume).filter(
      (top) => !rootPaths.includes(top) && volume.folders.some((folder) => folder.path === top && folder.exists)
    )
  );
}

/** A folder known only from the library (not on disk now). */
export function missingNode(path: string, imageCount?: number): FolderTree {
  return { path, name: baseName(path), isDir: true, children: [], imageCount, hasSubdirs: false, missing: true };
}

/** What is known of a folder gone from disk: registered photographs (catalog
 * folders) or offline photographs (offline folders) directly in it. */
interface MissingFolder {
  path: string;
  count: number;
}

/**
 * `node` with the folder `folder` (gone from disk, below `node`) put at its
 * place, with any missing parents between; nodes already there are kept
 * (and marked when they are the folder). Returns new nodes along the path
 * only; `node` is not changed.
 */
export function graftMissing(node: FolderTree, folder: MissingFolder): FolderTree {
  const target = pathKey(folder.path);
  if (target === pathKey(node.path)) {
    return node.missing ? { ...node, imageCount: node.imageCount ?? folder.count } : node;
  }
  const rest = relativePathBelow(folder.path, node.path);
  if (rest === null) return node;
  const children = node.children ?? [];
  const holder = children.find((child) => pathKey(child.path) === target || isBelow(folder.path, child.path));
  if (holder) {
    return { ...node, children: children.map((child) => (child === holder ? graftMissing(child, folder) : child)) };
  }
  // The next level towards the folder: the folder itself or a missing parent.
  const first = rest.split(pathSeparator(pathKey(node.path)))[0];
  const childPath = joinPath(node.path, first);
  const child =
    pathKey(childPath) === target ? missingNode(childPath, folder.count) : graftMissing(missingNode(childPath), folder);
  return { ...node, hasSubdirs: true, children: [...children, child] };
}

/** `trees` with every missing folder below one of them put at its place. */
export function withMissingFolders(trees: FolderTree[], missing: MissingFolder[]): FolderTree[] {
  return trees.map((tree) =>
    missing
      .filter((folder) => isBelow(folder.path, tree.path))
      .sort((a, b) => a.path.localeCompare(b.path))
      .reduce(graftMissing, tree)
  );
}

/** The missing folders the panel knows of: offline folders gone from disk
 * and catalog folders not on disk. */
export function missingFolders(view: CatalogFoldersView | null, offline: OfflineFolder[]): MissingFolder[] {
  const found = new Map<string, MissingFolder>();
  for (const folder of offline) if (folder.missing) found.set(folder.path, { path: folder.path, count: folder.count });
  for (const volume of view?.volumes ?? []) {
    for (const folder of volume.folders) {
      if (!folder.exists) found.set(folder.path, { path: folder.path, count: folder.photos });
    }
  }
  return [...found.values()];
}

/**
 * The drives of a catalog with their folder trees: each top-level folder's
 * disk tree (`trees`: the roots' and the catalog trees) or, when it is not
 * on disk, a tree made of the catalog folders below it; missing folders are
 * put at their place. Top-level trees carry their path on the drive as
 * `label`.
 */
export function volumeSections(
  view: CatalogFoldersView | null,
  trees: FolderTree[],
  offline: OfflineFolder[]
): VolumeSection[] {
  if (!view?.catalog) return [];
  const missing = missingFolders(view, offline);
  const folders = [...view.volumes.flatMap((volume) => volume.folders), ...(view.internal ?? [])];
  const rank = (volume: VolumeFolders) => (volume.kind === 'startup' ? 0 : volume.kind === 'network' ? 2 : 1);
  return [...view.volumes]
    .sort((a, b) => rank(a) - rank(b) || a.name.localeCompare(b.name) || a.key.localeCompare(b.key))
    .map((volume) => {
      const paths = volumeTops(volume);
      // Named as Lightroom names top-level folders; by their path on the
      // drive where two would read the same.
      const names = paths.map(baseName);
      const tops = paths.map((top, index) => {
        const onDisk = trees.find((tree) => tree.path === top);
        const registered = volume.folders.find((folder) => folder.path === top);
        const node = onDisk ?? missingNode(top, registered?.photos);
        const clash = names.indexOf(names[index]) !== names.lastIndexOf(names[index]);
        return { ...node, label: clash ? labelOnVolume(top, volume.mount_point) : baseName(top) };
      });
      return {
        volume,
        trees: withMissingFolders(tops, missing).map((tree) => withCatalogCounts(tree, folders))
      };
    });
}

/**
 * `node` and its children with the photograph counts of the catalog: a
 * catalog counts what it holds (Lightroom's Folders panel), not the files
 * lying in the folder, so a copy left out as a duplicate or a file not
 * added yet does not make the badge disagree with the grid. A folder counts
 * the catalog photographs in it and below it, whether or not its children
 * are loaded.
 */
export function withCatalogCounts(node: FolderTree, folders: Pick<CatalogFolder, 'path' | 'photos'>[]): FolderTree {
  const imageCount = folders
    .filter((folder) => folder.path === node.path || isBelow(folder.path, node.path))
    .reduce((sum, folder) => sum + folder.photos, 0);
  return {
    ...node,
    imageCount,
    children: node.children?.map((child) => withCatalogCounts(child, folders))
  };
}

/** A size in decimal units, as the system's file manager shows it. */
export function formatBytes(bytes: number): string {
  const units = ['B', 'KB', 'MB', 'GB', 'TB', 'PB'];
  let unit = 0;
  while (bytes >= 1000 ** (unit + 1) && unit < units.length - 1) unit += 1;
  const value = bytes / 1000 ** unit;
  const text = value < 10 && Math.round(value * 10) % 10 !== 0 ? value.toFixed(1) : value.toFixed(0);
  return `${text} ${units[unit]}`;
}

/** Free space as the drive header shows it: `free / total`. */
export function formatSpace(free: number | null, total: number | null): string | null {
  if (free === null || total === null || total <= 0) return null;
  return `${formatBytes(free)} / ${formatBytes(total)}`;
}
