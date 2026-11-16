/**
 * Where the native file and folder dialogs open (UXR4L-01): the folder last
 * used for the same purpose, else the folder last used in any dialog, else
 * the open library's folder. Without any of them the caller falls back to
 * the Pictures folder. Remembered per viewer in this webview's storage — a
 * convenience; nothing breaks when it is empty or unavailable.
 */

const STORAGE_KEY = 'rawtonic.dialogFolders';
/** The purpose that holds the folder last used in any dialog. */
const ANY = '*';

let libraryFolder: string | null = null;

/** Set by the library store whenever the open library changes. */
export function setDialogLibraryFolder(folder: string | null) {
  libraryFolder = folder || null;
}

function read(): Record<string, string> {
  try {
    const parsed: unknown = JSON.parse(window.localStorage?.getItem(STORAGE_KEY) ?? '{}');
    return parsed && typeof parsed === 'object' ? (parsed as Record<string, string>) : {};
  } catch {
    return {};
  }
}

/** The folder a dialog for `purpose` opens in; null when nothing is known. */
export function rememberedDialogFolder(purpose?: string): string | null {
  const folders = read();
  return (purpose && folders[purpose]) || folders[ANY] || libraryFolder;
}

/** The folder of a dialog's answer: a chosen folder itself, a file's folder. */
export function answerFolder(path: string, isFolder: boolean): string | null {
  if (isFolder) return path || null;
  const cut = Math.max(path.lastIndexOf('/'), path.lastIndexOf('\\'));
  if (cut < 0) return null;
  // `/photo.NEF` → `/`, `C:\photo.NEF` → `C:\`.
  return cut === 0 || path.charAt(cut - 1) === ':' ? path.slice(0, cut + 1) : path.slice(0, cut);
}

/** Records where a dialog for `purpose` was answered. */
export function rememberDialogFolder(purpose: string | undefined, folder: string | null) {
  if (!folder) return;
  try {
    const folders = read();
    folders[ANY] = folder;
    if (purpose) folders[purpose] = folder;
    window.localStorage?.setItem(STORAGE_KEY, JSON.stringify(folders));
  } catch {
    // Storage unavailable (private mode, cleared site data): dialogs open
    // at the library folder or Pictures instead.
  }
}

/** Whether `path` names a file alone, without a folder. */
export function isBareName(path: string): boolean {
  return !/[\\/]/.test(path);
}

/** `name` inside `folder`, with the folder's own separator. */
export function joinFolder(folder: string, name: string): string {
  const separator = folder.includes('\\') && !folder.includes('/') ? '\\' : '/';
  return folder.endsWith(separator) ? `${folder}${name}` : `${folder}${separator}${name}`;
}
