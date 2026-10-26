import { invoke } from '@tauri-apps/api/core';
import { Invokes } from '../components/ui/AppProperties';
import { useLibraryStore } from '../store/useLibraryStore';
import type { SelectView } from './offline';

/**
 * The session bar's XMP review (`src-tauri/src/xmp_review.rs`): photographs
 * whose XMP sidecar is *pending* (the library is newer and the sidecar not
 * written yet: offline, read-only, a failed write) or in *conflict* (the
 * sidecar changed outside Rawtonic since the last sync).
 */

/** `activeAlbumId` of the grid view of the photographs to review. */
export const XMP_VIEW_ID = 'session-xmp';

export type XmpState = 'pending' | 'conflict';

export interface XmpReviewEntry {
  path: string;
  state: XmpState;
  /** The photograph's file or drive is not there: written when it is back. */
  offline: boolean;
  volume: string | null;
  /** Its virtual copies (copy ids), listed in the grid with it. */
  copies: string[];
  /** Its edit is only in the editor sidecar beside it, not in the library
   * yet: Use XMP is not offered until it is edited in Develop. */
  edit_not_adopted: boolean;
}

/** Opaque to the webview: handed back to `session_xmp_undo`. */
export type XmpUndoStep = Record<string, unknown>;

export interface XmpReport {
  done: string[];
  offline: string[];
  conflicts: string[];
  failed: Array<{ path: string; reason: string }>;
  /** Use XMP: what undoes it. Undo: the steps that could not be undone. */
  undo: XmpUndoStep[];
  /** Left out of Use XMP (`XmpReviewEntry.edit_not_adopted`). */
  edit_not_adopted: string[];
}

/** The grid paths of the entries, with their virtual copies. */
export function xmpViewPaths(entries: XmpReviewEntry[]): string[] {
  return entries.flatMap((entry) => [entry.path, ...entry.copies.map((copy) => `${entry.path}?vc=${copy}`)]);
}

export function xmpCounts(entries: XmpReviewEntry[]) {
  const pending = entries.filter((entry) => entry.state === 'pending');
  return {
    pending: pending.length,
    offline: pending.filter((entry) => entry.offline).length,
    conflicts: entries.filter((entry) => entry.state === 'conflict').length,
  };
}

/** Shows the photographs whose XMP is pending or in conflict in the grid
 * (as the Missing Photographs view: listed again on each refresh, so file
 * operations and resolutions show). Returns them. */
export async function openXmpView(
  directory: string,
  name: string,
  select: SelectView,
  preserveEditor = false,
): Promise<XmpReviewEntry[]> {
  const asked = useLibraryStore.getState().sessionDirectory;
  const entries = (await invoke<XmpReviewEntry[]>(Invokes.SessionXmpReview, { directory })) ?? [];
  if (useLibraryStore.getState().sessionDirectory === asked) select(XMP_VIEW_ID, name, xmpViewPaths(entries), preserveEditor);
  return entries;
}
