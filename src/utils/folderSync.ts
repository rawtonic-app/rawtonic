import { invoke } from '@tauri-apps/api/core';
import { Invokes } from '../components/ui/AppProperties';

/**
 * Synchronize Folder (Lightroom's Synchronize Folder): a scan finds
 * photographs moved, renamed, added or deleted outside Rawtonic, the user
 * reviews it, and only what they accept is applied, each change checked
 * again first. Files on disk are never moved, changed or deleted; a removal
 * only removes the library entry. Mirrors `rawtonic_catalog::folder_sync`.
 */

export interface FileIdentity {
  size: number;
  mtime: number;
  fingerprint: string;
}

export interface SyncMove {
  assetId: string;
  from: string;
  to: string;
  identity: FileIdentity;
  /** The full content hash matched; otherwise size and fingerprint only. */
  verified: boolean;
}

export interface SyncNewFile {
  path: string;
  identity: FileIdentity;
  /** Photographs it is a byte-for-byte copy of. */
  copyOf: string[];
}

export interface SyncContested {
  path: string;
  identity: FileIdentity;
  moves: SyncMove[];
  copyOf: string[];
}

export interface SyncMissing {
  assetId: string;
  path: string;
  volume: string | null;
}

export interface SyncIssue {
  path: string;
  reason: string;
}

export interface SyncPlan {
  folders: string[];
  files: number;
  verifiedMoves: SyncMove[];
  probableMoves: SyncMove[];
  newFiles: SyncNewFile[];
  copies: SyncNewFile[];
  contested: SyncContested[];
  removed: SyncMissing[];
  unavailable: SyncMissing[];
  errors: SyncIssue[];
}

export type SyncAction =
  | { kind: 'move'; assetId: string; from: string; to: string; identity: FileIdentity }
  | { kind: 'add'; path: string; identity: FileIdentity }
  | { kind: 'remove'; assetId: string; path: string };

export interface SyncApplied {
  moved: [string, string][];
  added: string[];
  removed: string[];
  skipped: SyncIssue[];
}

export interface SyncProgress {
  phase: 'listing' | 'checking';
  done: number;
  total: number;
}

/** The event carrying a scan's progress. */
export const SYNC_PROGRESS = 'folder-sync-progress';

/** The groups the review lists, in order; contested ones need a choice. */
export type SyncGroup = 'verifiedMoves' | 'probableMoves' | 'newFiles' | 'copies' | 'removed';
export const SYNC_GROUPS: SyncGroup[] = ['verifiedMoves', 'newFiles', 'probableMoves', 'copies', 'removed'];

/** Groups ticked unless the user unticks them: what the scan proved. Probable
 * moves, copies and removals wait for the user. */
const TICKED: ReadonlySet<SyncGroup> = new Set(['verifiedMoves', 'newFiles']);

/** One row's key in the selection. */
export function rowKey(group: SyncGroup, row: SyncMove | SyncNewFile | SyncMissing): string {
  if (group === 'verifiedMoves' || group === 'probableMoves') {
    const move = row as SyncMove;
    return `move:${move.assetId}:${move.to}`;
  }
  if (group === 'removed') return `remove:${(row as SyncMissing).assetId}`;
  return `add:${(row as SyncNewFile).path}`;
}

export function groupRows(plan: SyncPlan, group: SyncGroup): (SyncMove | SyncNewFile | SyncMissing)[] {
  return plan[group];
}

/** The rows ticked when the review opens. */
export function defaultSelection(plan: SyncPlan): Set<string> {
  const selected = new Set<string>();
  for (const group of SYNC_GROUPS) {
    if (!TICKED.has(group)) continue;
    for (const row of groupRows(plan, group)) selected.add(rowKey(group, row));
  }
  return selected;
}

/** A contested file's choice: one of its moves (by asset id), a new
 * photograph, or leave it. */
export type ContestedChoice = { move: string } | 'add' | 'skip';

/** How many contested files still need a choice. */
export function unresolved(plan: SyncPlan, choices: Map<string, ContestedChoice>): number {
  return plan.contested.filter((c) => !choices.has(c.path)).length;
}

/** The actions the ticked rows and the contested choices make. */
export function syncActions(
  plan: SyncPlan,
  selected: ReadonlySet<string>,
  choices: Map<string, ContestedChoice>,
): SyncAction[] {
  const actions: SyncAction[] = [];
  const move = (m: SyncMove): SyncAction => ({
    kind: 'move',
    assetId: m.assetId,
    from: m.from,
    to: m.to,
    identity: m.identity,
  });
  for (const group of SYNC_GROUPS) {
    for (const row of groupRows(plan, group)) {
      if (!selected.has(rowKey(group, row))) continue;
      if (group === 'verifiedMoves' || group === 'probableMoves') actions.push(move(row as SyncMove));
      else if (group === 'removed') {
        const missing = row as SyncMissing;
        actions.push({ kind: 'remove', assetId: missing.assetId, path: missing.path });
      } else {
        const file = row as SyncNewFile;
        actions.push({ kind: 'add', path: file.path, identity: file.identity });
      }
    }
  }
  for (const contested of plan.contested) {
    const choice = choices.get(contested.path);
    if (!choice || choice === 'skip') continue;
    if (choice === 'add') {
      actions.push({ kind: 'add', path: contested.path, identity: contested.identity });
      continue;
    }
    const chosen = contested.moves.find((m) => m.assetId === choice.move);
    if (chosen) actions.push(move(chosen));
  }
  return actions;
}

/** `path` as the review shows it: relative to the scanned folder that holds
 * it (the dialog's header names that folder), so names and subfolders stay
 * readable in a narrow row; the whole path when no scanned folder holds it. */
export function shownPath(folders: readonly string[], path: string): string {
  let best = '';
  for (const folder of folders) {
    const base = folder.replace(/[\\/]+$/, '');
    if (base.length > best.length && (path.startsWith(`${base}/`) || path.startsWith(`${base}\\`))) best = base;
  }
  return best ? path.slice(best.length + 1) : path;
}

// Natural order, as the folder list sorts names: copy_2 before copy_10.
const naturalOrder = new Intl.Collator(undefined, { numeric: true, sensitivity: 'base' });
const byPath = (a: string, b: string) => naturalOrder.compare(a, b) || (a < b ? -1 : a > b ? 1 : 0);

/** `plan` with every group's rows in natural order of the paths the review
 * shows (a move by where its file is now). */
export function sortPlan(plan: SyncPlan): SyncPlan {
  const sorted = <T>(rows: T[], path: (row: T) => string) => [...rows].sort((a, b) => byPath(path(a), path(b)));
  return {
    ...plan,
    verifiedMoves: sorted(plan.verifiedMoves, (m) => m.to),
    probableMoves: sorted(plan.probableMoves, (m) => m.to),
    newFiles: sorted(plan.newFiles, (f) => f.path),
    copies: sorted(plan.copies, (f) => f.path),
    contested: sorted(plan.contested, (c) => c.path),
    removed: sorted(plan.removed, (m) => m.path),
    unavailable: sorted(plan.unavailable, (m) => m.path),
    errors: sorted(plan.errors, (e) => e.path),
  };
}

/** Whether a scan found anything to act on. */
export function planIsEmpty(plan: SyncPlan): boolean {
  return SYNC_GROUPS.every((group) => plan[group].length === 0) && plan.contested.length === 0;
}

/** Asks the open library's workspace to synchronize `folder` (the whole
 * library with null): it opens the review dialog. */
export const OPEN_FOLDER_SYNC_EVENT = 'rawtonic:open-folder-sync';
export interface OpenFolderSyncDetail {
  folder: string | null;
}
export function requestFolderSync(folder: string | null) {
  window.dispatchEvent(new CustomEvent<OpenFolderSyncDetail>(OPEN_FOLDER_SYNC_EVENT, { detail: { folder } }));
}

/** Fired after a synchronization applied changes (`detail`: the
 * `SyncApplied`), so the folders, the shown photographs, the selection and a
 * photograph open in Develop follow moved ones as after Locate Folder. */
export const FOLDER_SYNC_APPLIED = 'rawtonic:folder-sync-applied';

/** `activeAlbumId` of the Previous Import view. */
export const PREVIOUS_IMPORT_VIEW_ID = 'session-previous-import';
/** Fired after an ingest, a synchronization or a hot folder capture added
 * photographs, so Previous Import counts and shows them. */
export const IMPORTS_CHANGED = 'rawtonic:imports-changed';

export interface ImportBatchInfo {
  id: number;
  kind: 'ingest' | 'add' | 'sync' | 'hotfolder';
  created_at: string;
  count: number;
}
export interface PreviousImport {
  batch: ImportBatchInfo | null;
  paths: string[];
}

export function previousImport(directory: string): Promise<PreviousImport> {
  return invoke<PreviousImport>(Invokes.SessionPreviousImport, { directory });
}

/** Shows Previous Import in the grid; returns how many photographs it holds. */
export async function openPreviousImport(
  directory: string,
  name: string,
  select: (id: string, name: string, paths: string[], preserveEditor?: boolean) => void,
  preserveEditor = false,
): Promise<number> {
  const { paths } = await previousImport(directory);
  select(PREVIOUS_IMPORT_VIEW_ID, name, paths, preserveEditor);
  return paths.length;
}
