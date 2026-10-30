import { invoke } from '@tauri-apps/api/core';
import { notify } from '../activity/notify';
import { Invokes, SortDirection, type FolderTreeSort } from '../components/ui/AppProperties';
import { useLibraryStore } from '../store/useLibraryStore';
import { useSettingsStore } from '../store/useSettingsStore';

/** A folder as `get_folder_tree` returns it (file_management/folders.rs `FolderNode`). */
export interface FolderTree {
  children: FolderTree[];
  isDir: boolean;
  name: string;
  path: string;
  imageCount?: number;
  hasSubdirs?: boolean;
  modified?: number;
  created?: number;
  /** Not on disk now (renamed, moved, on a disconnected drive): known from
   * the library only (utils/catalogFolders.ts). */
  missing?: boolean;
  /** Shown instead of `name` (a top-level folder of a drive). */
  label?: string;
}

export const DEFAULT_FOLDER_TREE_SORT: FolderTreeSort = { key: 'name', order: SortDirection.Ascending };

/** `node` with `newChildren` (a lazily loaded level) put under `targetPath`;
 * children already loaded below them are kept. */
export function insertChildrenIntoTree(node: FolderTree, targetPath: string, newChildren: FolderTree[]): FolderTree {
  if (node.path === targetPath) {
    const children = newChildren.map((newChild) => {
      const existing = node.children?.find((c) => c.path === newChild.path);
      return existing?.children?.length ? { ...newChild, children: existing.children } : newChild;
    });
    return { ...node, children };
  }
  if (node.children?.length) {
    return { ...node, children: node.children.map((child) => insertChildrenIntoTree(child, targetPath, newChildren)) };
  }
  return node;
}

/** The folders whose name contains `query` (case-insensitive) with their parents, or null. */
export function filterFolderTree(node: FolderTree, query: string): FolderTree | null {
  const isMatch = node.name.toLowerCase().includes(query.toLowerCase());
  const children = (node.children ?? [])
    .map((child) => filterFolderTree(child, query))
    .filter((child): child is FolderTree => child !== null);
  if (isMatch || children.length) return { ...node, children };
  return null;
}

/** Every level of the trees in the chosen order. */
export function sortFolderTree(nodes: FolderTree[], sort: FolderTreeSort): FolderTree[] {
  const sorted = [...(nodes ?? [])].sort((a, b) => {
    let comparison = 0;
    if (sort.key === 'name') comparison = a.name.localeCompare(b.name, undefined, { numeric: true, sensitivity: 'base' });
    else if (sort.key === 'modified') comparison = (a.modified || 0) - (b.modified || 0);
    else if (sort.key === 'created') comparison = (a.created || 0) - (b.created || 0);
    else if (sort.key === 'imageCount') comparison = (a.imageCount || 0) - (b.imageCount || 0);
    return sort.order === SortDirection.Ascending ? comparison : -comparison;
  });
  return sorted.map((node) =>
    node.children?.length ? { ...node, children: sortFolderTree(node.children, sort) } : node,
  );
}

/** Expands or collapses a folder; expanding loads its next level (folders
 * are listed lazily) into the library, catalog and favourite trees. A
 * folder not on disk (`load` false) only opens: its levels are known. */
export async function toggleFolder(path: string, load = true) {
  const { expandedFolders, setLibrary } = useLibraryStore.getState();
  const expanding = !expandedFolders.has(path);
  setLibrary((state) => {
    const next = new Set(state.expandedFolders);
    if (expanding) next.add(path);
    else next.delete(path);
    return { expandedFolders: next };
  });
  if (!expanding || !load) return;
  try {
    const settings = useSettingsStore.getState().appSettings;
    const children = await invoke<FolderTree[]>(Invokes.GetFolderChildren, {
      path,
      showImageCounts: Boolean(settings?.enableFolderImageCounts || settings?.folderTreeSort?.key === 'imageCount'),
    });
    setLibrary((state) => ({
      folderTrees: state.folderTrees.map((tree) => insertChildrenIntoTree(tree, path, children)),
      pinnedFolderTrees: state.pinnedFolderTrees.map((tree) => insertChildrenIntoTree(tree, path, children)),
      catalogTrees: state.catalogTrees.map((tree) => insertChildrenIntoTree(tree, path, children)),
    }));
  } catch (err) {
    notify.error(`The folder could not be read: ${err}`);
  }
}
