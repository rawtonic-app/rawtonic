import { invoke } from "@tauri-apps/api/core";
import { Invokes, type Album, type AlbumGroup, type AlbumItem } from "../ui/AppProperties";
import { useLibraryStore } from "../../store/useLibraryStore";
import type { SessionAlbum } from "./StudioAlbums";

/** The app-wide albums (`albums.json`, outside sessions) as the Albums tool
 * lists them: depth first, a group before its children, in stored order. */
export function flattenAlbumTree(tree: AlbumItem[], parent: string | null = null, out: SessionAlbum[] = []) {
  tree.forEach((item, position) => {
    const group = item.type === "group";
    out.push({
      id: item.id,
      name: item.name,
      parent,
      count: group ? 0 : (item as Album).images.length,
      kind: group ? "group" : "album",
      position,
    });
    if (group) flattenAlbumTree((item as AlbumGroup).children, item.id, out);
  });
  return out;
}

export function findAlbumItem(tree: AlbumItem[], id: string): AlbumItem | null {
  for (const item of tree) {
    if (item.id === id) return item;
    if (item.type === "group") {
      const found = findAlbumItem(item.children, id);
      if (found) return found;
    }
  }
  return null;
}

/** Removes `id` from the tree (in place) and returns it. */
function extract(nodes: AlbumItem[], id: string): AlbumItem | null {
  const index = nodes.findIndex((node) => node.id === id);
  if (index >= 0) return nodes.splice(index, 1)[0];
  for (const node of nodes) {
    if (node.type === "group") {
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
  return group?.type === "group" ? group.children : null;
}

// Every change works on a copy and keeps what it does not change (icons,
// photographs of other albums); null means nothing to save.

export function withNewItem(
  tree: AlbumItem[],
  item: { kind: "album" | "group"; name: string; parent: string | null; paths: string[]; id: string },
): AlbumItem[] | null {
  const next = structuredClone(tree);
  const list = childList(next, item.parent);
  if (!list) return null;
  list.push(
    item.kind === "group"
      ? { type: "group", id: item.id, name: item.name, children: [] }
      : { type: "album", id: item.id, name: item.name, images: [...new Set(item.paths)] },
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
  if (album?.type !== "album") return null;
  const missing = paths.filter((path) => !album.images.includes(path));
  if (!missing.length) return null;
  album.images.push(...new Set(missing));
  return next;
}

export function withPhotosRemoved(tree: AlbumItem[], id: string, paths: string[]): AlbumItem[] | null {
  const next = structuredClone(tree);
  const album = findAlbumItem(next, id);
  if (album?.type !== "album") return null;
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
  if (parent !== null && (parent === id || (item.type === "group" && findAlbumItem(item.children, parent)))) return null;
  extract(next, id);
  const list = childList(next, parent);
  if (!list) return null;
  list.splice(Math.max(0, Math.min(index, list.length)), 0, item);
  return next;
}

/** Applies `change` to the stored albums and saves them. It reads the file
 * first, so an album list not loaded yet (or changed elsewhere) is never
 * overwritten with an older one. */
export async function changeLibraryAlbums(change: (tree: AlbumItem[]) => AlbumItem[] | null) {
  const stored = (await invoke<AlbumItem[]>(Invokes.GetAlbums)) ?? [];
  const next = change(stored);
  if (!next) {
    useLibraryStore.getState().setLibrary({ albumTree: stored });
    return stored;
  }
  await invoke(Invokes.SaveAlbums, { tree: next });
  const saved = (await invoke<AlbumItem[]>(Invokes.GetAlbums)) ?? next;
  useLibraryStore.getState().setLibrary({ albumTree: saved });
  return saved;
}

export async function loadLibraryAlbums() {
  const tree = (await invoke<AlbumItem[]>(Invokes.GetAlbums)) ?? [];
  useLibraryStore.getState().setLibrary({ albumTree: tree });
  return tree;
}
