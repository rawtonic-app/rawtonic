// SPDX-License-Identifier: AGPL-3.0-or-later
import type { LastFolderState } from '../components/ui/AppProperties';
import { PREVIOUS_IMPORT_VIEW_ID } from './folderSync';
import { pathKey } from './pathKey';

/** A path in one comparable form: no verbatim `\\?\` prefix, forward slashes,
 * no trailing slash; case-insensitive for Windows paths (drive letter or
 * share), as the file system is. */
export function comparablePath(path: string): string {
  const plain = pathKey(path).replace(/\\/g, '/').replace(/\/+$/, '');
  return /^([A-Za-z]:|\/\/)/.test(plain) ? plain.toLowerCase() : plain;
}

export function samePath(a: string, b: string): boolean {
  return comparablePath(a) === comparablePath(b);
}

/** `path` is `root` or inside it. */
export function isInside(path: string, root: string): boolean {
  const p = comparablePath(path);
  const r = comparablePath(root);
  return p === r || p.startsWith(`${r}/`);
}

/** What the grid showed when the library was last open, as far as it can be
 * shown again: a folder of the library, Previous Import or a collection.
 * Transient views (missing photographs, pending sidecars, an activity entry's
 * photographs) are not restored. */
export type SavedView = { kind: 'folder'; path: string } | { kind: 'previousImport' } | { kind: 'album'; id: string };

const SESSION_ALBUM = 'session-album:';

/** The view to open again for the library at `directory` (whose browse roots
 * are `roots`), or null (the library's own folder is shown): nothing saved,
 * saved for another library or by an older version that did not say which,
 * or a view that cannot be restored. The caller checks the view still exists. */
export function savedView(
  last: LastFolderState | null | undefined,
  directory: string,
  roots: string[]
): SavedView | null {
  if (!last?.sessionDirectory || !samePath(last.sessionDirectory, directory)) return null;
  const album = last.activeAlbumId;
  if (album === PREVIOUS_IMPORT_VIEW_ID) return { kind: 'previousImport' };
  if (album?.startsWith(SESSION_ALBUM) && album.length > SESSION_ALBUM.length)
    return { kind: 'album', id: album.slice(SESSION_ALBUM.length) };
  if (album) return null;
  const folder = last.currentFolderPath;
  if (folder && !folder.startsWith('Album: ') && roots.some((root) => isInside(folder, root)))
    return { kind: 'folder', path: folder };
  return null;
}
