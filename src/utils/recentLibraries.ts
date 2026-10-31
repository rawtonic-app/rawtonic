/** Most recently opened first; clearing the list never affects startup restore. */
export function withRecentLibrary(recent: string[] | undefined, directory: string): string[] {
  return [directory, ...(recent ?? []).filter((path) => path !== directory)].slice(0, 10);
}

/** A library folder moved from `from` to `to`: its entry follows in place. */
export function withMovedLibrary(recent: string[] | undefined, from: string, to: string): string[] | undefined {
  if (!recent?.includes(from)) return recent;
  return recent.filter((path) => path !== to).map((path) => (path === from ? to : path));
}
