/** Session and catalog folders the app keeps for itself (mask bitmaps,
 * previews), not photographs. */
const INTERNAL_SESSION_FOLDERS = new Set(['masks', 'previews', 'smart-previews']);

/** Whether `name`, a folder directly inside `parent`, is one of the open
 * session's own folders that folder lists must not show. */
export function isInternalSessionFolder(parent: string | null | undefined, name: string | undefined, sessionRoot?: string | null) {
  return !!sessionRoot && parent === sessionRoot && INTERNAL_SESSION_FOLDERS.has(name ?? '');
}

/** The folder trees without the session's internal folders (one level below the session root). */
export function withoutInternalSessionFolders<T extends { path: string; name: string; children: T[] }>(
  trees: T[],
  sessionRoot?: string | null,
): T[] {
  if (!sessionRoot) return trees;
  return trees.map((tree) =>
    tree.path === sessionRoot && tree.children?.length
      ? { ...tree, children: tree.children.filter((child) => !isInternalSessionFolder(tree.path, child.name, sessionRoot)) }
      : tree,
  );
}
