import { type AppSettings, LibraryViewMode } from '../components/ui/AppProperties';

/** Whether `folder`'s grid includes its subfolders' photos (Lightroom's Show
 * Photos in Subfolders): the user's choice for that folder, else on for
 * `defaultRoot` (a catalog's Photos root, whose photos sit in dated folders),
 * else the inherited global view mode. */
export function showsSubfolders(
  folder: string,
  settings: Pick<AppSettings, 'subfolderViews' | 'libraryViewMode'> | null | undefined,
  defaultRoot: string | null,
): boolean {
  const chosen = settings?.subfolderViews?.[folder];
  if (chosen !== undefined) return chosen;
  return folder === defaultRoot || settings?.libraryViewMode === LibraryViewMode.Recursive;
}

export const folderViewMode = (...args: Parameters<typeof showsSubfolders>): LibraryViewMode =>
  showsSubfolders(...args) ? LibraryViewMode.Recursive : LibraryViewMode.Flat;

export function withSubfolderView(settings: AppSettings, folder: string, on: boolean): AppSettings {
  return { ...settings, subfolderViews: { ...settings.subfolderViews, [folder]: on } };
}
