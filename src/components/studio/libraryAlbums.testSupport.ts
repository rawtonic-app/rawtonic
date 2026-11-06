// In-memory IPC mock for the Albums tool tests. Rust tests exercise the real mutations.
import type { AlbumItem } from '../ui/AppProperties';
import { findAlbumItem, type AlbumChange } from './libraryAlbums';
/** Removes `id` from the tree (in place) and returns it. */
function extract(nodes: AlbumItem[], id: string): AlbumItem | null {
  const index = nodes.findIndex((node) => node.id === id);
  if (index >= 0) return nodes.splice(index, 1)[0];
  for (const node of nodes) {
    if (node.type === 'group') {
      const found = extract(node.children, id);
      if (found) return found;
    }
  }
  return null;
}

/** The children list of `parent` (the top level for null), or null when it is no group. */
function childList(tree: AlbumItem[], parent: string | null): AlbumItem[] | null {
  if (parent === null) return tree;
  const group = findAlbumItem(tree, parent);
  return group?.type === 'group' ? group.children : null;
}

// Every change works on a copy and keeps what it does not change (icons,
// photographs of other albums); null means nothing to save.

export function withNewItem(
  tree: AlbumItem[],
  item: { kind: 'album' | 'group'; name: string; parent: string | null; paths: string[]; id: string }
): AlbumItem[] | null {
  const next = structuredClone(tree);
  const list = childList(next, item.parent);
  if (!list) return null;
  list.push(
    item.kind === 'group'
      ? { type: 'group', id: item.id, name: item.name, children: [] }
      : { type: 'album', id: item.id, name: item.name, images: [...new Set(item.paths)] }
  );
  return next;
}

export function withRenamed(tree: AlbumItem[], id: string, name: string): AlbumItem[] | null {
  const next = structuredClone(tree);
  const item = findAlbumItem(next, id);
  if (!item || item.name === name) return null;
  item.name = name;
  return next;
}

/** Deletes an album, or a group with what it holds; photographs stay. */
export function withDeleted(tree: AlbumItem[], id: string): AlbumItem[] | null {
  const next = structuredClone(tree);
  return extract(next, id) ? next : null;
}

export function withPhotosAdded(tree: AlbumItem[], id: string, paths: string[]): AlbumItem[] | null {
  const next = structuredClone(tree);
  const album = findAlbumItem(next, id);
  if (album?.type !== 'album') return null;
  const missing = paths.filter((path) => !album.images.includes(path));
  if (!missing.length) return null;
  album.images.push(...new Set(missing));
  return next;
}

export function withPhotosRemoved(tree: AlbumItem[], id: string, paths: string[]): AlbumItem[] | null {
  const next = structuredClone(tree);
  const album = findAlbumItem(next, id);
  if (album?.type !== 'album') return null;
  const remove = new Set(paths);
  const kept = album.images.filter((path) => !remove.has(path));
  if (kept.length === album.images.length) return null;
  album.images = kept;
  return next;
}

/** Moves an item to `parent` at `index`; null when it would go into itself.
 * (The app-wide albums are kept in name order, so only the parent sticks.) */
export function withMoved(tree: AlbumItem[], id: string, parent: string | null, index: number): AlbumItem[] | null {
  const next = structuredClone(tree);
  const item = findAlbumItem(next, id);
  if (!item) return null;
  if (parent !== null && (parent === id || (item.type === 'group' && findAlbumItem(item.children, parent))))
    return null;
  extract(next, id);
  const list = childList(next, parent);
  if (!list) return null;
  list.splice(Math.max(0, Math.min(index, list.length)), 0, item);
  return next;
}

export function applyMockAlbumChange(tree: AlbumItem[], change: AlbumChange): AlbumItem[] {
  let next: AlbumItem[] | null;
  switch (change.kind) {
    case 'addToAlbum':
      if (findAlbumItem(tree, change.albumId)?.type !== 'album') throw `Album ${change.albumId} is not there any more`;
      next = withPhotosAdded(tree, change.albumId, change.paths);
      break;
    case 'removePhotos':
      next = withPhotosRemoved(tree, change.albumId, change.paths);
      break;
    case 'rename':
      next = withRenamed(tree, change.id, change.name);
      break;
    case 'delete':
      next = withDeleted(tree, change.id);
      break;
    case 'move':
      next = withMoved(tree, change.id, change.parent, change.index);
      break;
    case 'create': {
      const item = change.item;
      next = withNewItem(tree, {
        kind: item.type,
        id: item.id,
        name: item.name,
        paths: item.type === 'album' ? item.images : [],
        parent: change.parent
      });
      break;
    }
  }
  return next ?? tree;
}
