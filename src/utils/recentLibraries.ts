/** Most recently opened first; clearing the list never affects startup restore. */
export function withRecentLibrary(recent: string[] | undefined, directory: string): string[] {
  return [directory, ...(recent ?? []).filter((path) => path !== directory)].slice(0, 10);
}
