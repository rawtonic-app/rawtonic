// SPDX-License-Identifier: AGPL-3.0-or-later
/**
 * One form of a photo's path for state keyed by it (picks and rejects).
 * On Windows the same file reaches the window as `\\?\C:\…` (the library's
 * canonical paths: flags, culling receipts, albums) and as `C:\…` (a folder
 * listed from a plain path, as after a restart): keyed by either form, a
 * flag was not found under the other (WFQ/WPFC: Pick and Reject read 0/0
 * after a restart although the catalog held them). The key drops the
 * verbatim prefix: `\\?\C:\a` → `C:\a`, `\\?\UNC\server\share\a` →
 * `\\server\share\a`. Every other path is its own key. A key is never used
 * to open a file.
 */
export function pathKey(path: string): string {
  if (!path.startsWith('\\\\?\\')) return path;
  const rest = path.slice(4);
  if (/^UNC\\/i.test(rest)) return `\\\\${rest.slice(4)}`;
  if (/^[A-Za-z]:\\/.test(rest)) return rest;
  return path;
}

/** `record` with each key in its `pathKey` form (the same object when no
 * key changes). A key given in both forms keeps the value under the plain
 * one's position last seen in `record`. */
export function byPathKey<T>(record: Record<string, T>): Record<string, T> {
  if (!Object.keys(record).some((key) => pathKey(key) !== key)) return record;
  const next: Record<string, T> = {};
  for (const [key, value] of Object.entries(record)) next[pathKey(key)] = value;
  return next;
}
