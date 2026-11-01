import { invoke } from "@tauri-apps/api/core";
import { Invokes } from "../components/ui/AppProperties";

/** Session and catalog folders the app keeps for itself (mask bitmaps,
 * previews), not photographs. */
const INTERNAL_SESSION_FOLDERS = new Set(['masks', 'previews', 'smart-previews']);

/** The session folder rows of a session summary; the default ones inside it
 * when the summary predates configurable folders (before G38). */
export function sessionFoldersOrDefault(directory: string, folders: SessionFolder[] | undefined): SessionFolder[] {
  if (folders?.length) return folders;
  const root = directory.replace(/[\\/]+$/, "");
  const roles: Array<[FolderRole, string]> = [["capture", "Capture"], ["selects", "Selects"], ["output", "Output"]];
  return roles.map(([role, name]) => ({ role, stored: name, path: `${root}/${name}`, inside: true, available: true, recreated: true }));
}

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

/** A session's Capture, Selects or Output folder (G38). */
export type FolderRole = "capture" | "selects" | "output";

export interface SessionFolder {
  role: FolderRole;
  /** As stored: relative inside the session, absolute outside it. */
  stored: string;
  /** Where it is now. */
  path: string;
  /** Inside the session folder (moves with it). */
  inside: boolean;
  /** There now (false: deleted, or its drive is not connected). */
  available: boolean;
  /** A folder the app may make again when it is missing: only one it made
   * with the session (its default inside the session). Any other configured
   * folder that is missing — also one inside the session whose parent was
   * renamed — is reported, never made at its old path. */
  recreated: boolean;
}

export const FOLDER_ROLES: FolderRole[] = ["capture", "selects", "output"];

/** The folder row of `role` (the first one, so a stale row from another
 * session never wins). */
export function sessionFolderRow(folders: SessionFolder[] | undefined, role: FolderRole): SessionFolder | undefined {
  return folders?.find((f) => f.role === role);
}

/** The path the Folder row for `role` can open without the app making
 * anything: its folder when it is there. Otherwise, only while the app may
 * make it again (a default folder it gave the session), the default place
 * the dialog offers; any other missing folder has no place to show. */
export function openableFolderPath(folders: SessionFolder[] | undefined, role: FolderRole, defaultPath: string): string | null {
  const folder = sessionFolderRow(folders, role);
  if (!folder) return defaultPath;
  if (folder.available) return folder.path;
  return folder.recreated ? defaultPath : null;
}

/** The folder with `role` of a session summary; the default inside it when
 * the summary predates configurable folders. */
export function sessionFolderPath(
  summary: { directory: string; folders?: SessionFolder[]; manifest: { capture: string } },
  role: FolderRole,
): string {
  const folder = summary.folders?.find((f) => f.role === role);;
  if (folder) return folder.path;
  const root = summary.directory.replace(/[\\/]+$/, "");
  const name = role === "capture" ? summary.manifest.capture : role === "selects" ? "Selects" : "Output";
  return `${root}/${name}`;
}

/** The session's folders outside it that are there: shown in the Folders
 * panel beside the session's own tree. */
export function outsideFolders(folders: SessionFolder[] | undefined): string[] {
  return [...new Set((folders ?? []).filter((f) => !f.inside && f.available).map((f) => f.path))];
}

/** The session's folders that are not there (deleted, their drive away, or a
 * parent renamed), as the Folders panel shows them. */
export function unavailableFolders(folders: SessionFolder[] | undefined): SessionFolder[] {
  return (folders ?? []).filter((f) => !f.available);
}

/** The folder with `role` to put files in now. Rejects, naming it, when it
 * is outside the session and not there (a drive not connected): nothing is
 * written elsewhere instead. */
export function usableSessionFolder(directory: string, role: FolderRole): Promise<string> {
  return invoke<string>(Invokes.SessionUsableFolder, { directory, role });
}

/** What "Move contents" did: items moved (in one or more undoable
 * operations) and why it stopped, if it did. */
export interface ContentsMoved {
  items: number;
  operations: number;
  error?: string;
}

type ContentsMover = (from: string, to: string) => Promise<ContentsMoved>;
let mover: ContentsMover | null = null;

/** The Library's file operations register how a folder's contents move
 * (saved edits first, each operation an Undo step, paths followed). */
export function registerContentsMover(next: ContentsMover | null) {
  mover = next;
}

/** Moves everything in `from` into `to` through the Library's file
 * operations (Session Folders' "Move contents"). */
export function moveFolderContents(from: string, to: string): Promise<ContentsMoved> {
  if (!mover) return Promise.resolve({ items: 0, operations: 0, error: "The Library is not ready" });
  return mover(from, to);
}
