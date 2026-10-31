// SPDX-License-Identifier: AGPL-3.0-or-later
/**
 * Find Duplicates, stage 1 (src-tauri/src/duplicates.rs): photographs of a
 * library whose files hold identical image data, whatever their names,
 * folders, metadata or edits. A background pass reads what is not compared
 * yet (progress and Stop in the activity centre); the review lists the
 * groups and trashes only what the user marked, through the journaled file
 * operations (one undoable step). Nothing is merged. A photo inside another
 * project's or catalog's folder belongs to it and cannot be marked here.
 */
import { invoke } from '@tauri-apps/api/core';
import { create } from 'zustand';
import { Invokes } from '../components/ui/AppProperties';

export const DUPLICATES_PROGRESS = 'duplicates-progress';
export const DUPLICATES_FINISHED = 'duplicates-finished';

export interface DuplicatesProgress {
  run: number;
  directory: string;
  done: number;
  total: number;
}

export interface DuplicatesFinished {
  run: number;
  directory: string;
  photos: number;
  hashed: number;
  unreadable: string[];
  cancelled: boolean;
  error: string | null;
  seconds: number;
}

export interface DuplicateMember {
  id: string;
  path: string;
  size: number;
  /** Effective capture time, `YYYY-MM-DD HH:MM:SS`. */
  captured: string | null;
  /** When it came into the library (UTC, `YYYY-MM-DD HH:MM:SS`). */
  imported: string | null;
  rating: number;
  flag: number;
  label: string;
  keywords: number;
  /** Albums it is in (smart albums aside). */
  albums: number;
  copies: number;
  /** It has a title, caption, creator, copyright or IPTC field. */
  described: boolean;
  /** Its RAW+JPEG companion (trashed with it). */
  companion: string | null;
  /** What was compared: the image data of its container, or `file`. */
  compared: string;
  /** The other project or catalog whose folder holds the file: it stays. */
  owner: { directory: string; name: string } | null;
  /** The file lies inside this library's own folder. */
  ownFolder: boolean;
  edited: boolean;
  /** Pre-marked for the Trash: the copy with the most work stays (on a tie
   * the oldest import); only copies inside this library's folder are marked. */
  suggested: boolean;
}

export type DuplicateGroup = DuplicateMember[];

export const startDuplicatesPass = (directory: string) => invoke<number>(Invokes.DuplicatesStart, { directory });
export const cancelDuplicatesPass = (run: number) => invoke(Invokes.DuplicatesCancel, { run });
export const duplicateGroups = (directory: string) => invoke<DuplicateGroup[]>(Invokes.DuplicatesGroups, { directory });
/** A photo marked for the Trash that must stay: its image data changed
 * since compared, its paired JPEG has no identical copy that stays, or
 * another project or catalog owns it. */
export interface Refused {
  path: string;
  reason: 'changed' | 'companion' | 'owned';
}
/** Photos of `trash` that must stay (empty: go ahead). */
export const confirmDuplicates = (directory: string, keep: string[], trash: string[]) =>
  invoke<Refused[]>(Invokes.DuplicatesConfirm, { directory, keep, trash });

/** Work done on a photograph (or a file that goes with it) that trashing
 * it would set aside. */
export const hasOwnWork = (member: DuplicateMember): boolean =>
  member.edited ||
  member.rating > 0 ||
  member.flag !== 0 ||
  member.label !== '' ||
  member.keywords > 0 ||
  member.albums > 0 ||
  member.copies > 0 ||
  member.described ||
  member.companion !== null;

/** The review's open state; the activity centre's Review opens it too. */
export const useDuplicatesDialog = create<{ open: boolean; show(): void; close(): void }>((set) => ({
  open: false,
  show: () => set({ open: true }),
  close: () => set({ open: false }),
}));
