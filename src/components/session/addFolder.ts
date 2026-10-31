/** Folders › "Add Folder…" with a library open (G39 AF): a catalog adds the
 * folder's photographs where they are (Lightroom's Add), a session shows the
 * folder only and offers to copy it in. The dialog lives in
 * SessionWorkspace, which runs the add as an ingest. */

/** Mirrors `ingest::folder_add::FolderPlan` (`catalog_add_folder_scan`). */
export interface FolderPlan {
  files: string[];
  photographs: number;
  registered: number;
  owned: { library: string; name: string; count: number }[];
  truncated: boolean;
}

export interface AddFolderDetail {
  folder: string;
  /** Lists the folder in the Folders panel and shows it in the grid. */
  show: () => Promise<void>;
}

/** Window event that opens the Add Folder dialog for a chosen folder. */
export const ADD_FOLDER_EVENT = "rawtonic-add-folder";

/** Whether a library's workspace handles Add Folder (it listens). */
let listening = 0;
export function listenForAddFolder(handler: (detail: AddFolderDetail) => void): () => void {
  const listener = (e: Event) => handler((e as CustomEvent<AddFolderDetail>).detail);
  window.addEventListener(ADD_FOLDER_EVENT, listener);
  listening++;
  return () => {
    window.removeEventListener(ADD_FOLDER_EVENT, listener);
    listening--;
  };
}

/** Opens the Add Folder dialog for `folder`; false when no library's
 * workspace is there to show it (then the caller just shows the folder). */
export function requestAddFolder(detail: AddFolderDetail): boolean {
  if (listening <= 0) return false;
  window.dispatchEvent(new CustomEvent<AddFolderDetail>(ADD_FOLDER_EVENT, { detail }));
  return true;
}

const DISMISSED = "rawtonic.addFolder.bannerDismissed";

function dismissedKeys(): string[] {
  try {
    const value = JSON.parse(localStorage.getItem(DISMISSED) ?? "[]");
    return Array.isArray(value) ? value.filter((v) => typeof v === "string") : [];
  } catch {
    return [];
  }
}

/** The banner offering to add a listed folder's photographs was put away
 * for this catalog and folder ("Not now" is remembered: it is offered once). */
export function bannerDismissed(catalog: string, folder: string): boolean {
  return dismissedKeys().includes(`${catalog}\n${folder}`);
}

export function dismissBanner(catalog: string, folder: string) {
  const keys = new Set(dismissedKeys());
  keys.add(`${catalog}\n${folder}`);
  try {
    localStorage.setItem(DISMISSED, JSON.stringify([...keys].slice(-500)));
  } catch {
    // A convenience only: the banner comes back next time.
  }
}
