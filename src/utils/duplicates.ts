// SPDX-License-Identifier: AGPL-3.0-or-later
/**
 * Find Duplicates, stage 1 (src-tauri/src/duplicates.rs): photographs of a
 * library whose files hold identical image data, whatever their names,
 * folders, metadata or edits. A background pass reads what is not compared
 * yet (progress and Stop in the activity centre); the review lists the
 * groups and trashes only what the user marked, through the journaled file
 * operations (one undoable step). Nothing is merged.
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
  copies: number;
  /** What was compared: the image data of its container, or `file`. */
  compared: string;
  edited: boolean;
}

export type DuplicateGroup = DuplicateMember[];

export const startDuplicatesPass = (directory: string) => invoke<number>(Invokes.DuplicatesStart, { directory });
export const cancelDuplicatesPass = (run: number) => invoke(Invokes.DuplicatesCancel, { run });
export const duplicateGroups = (directory: string) => invoke<DuplicateGroup[]>(Invokes.DuplicatesGroups, { directory });
/** Paths of `trash` whose image data changed since compared (empty: go ahead). */
export const confirmDuplicates = (directory: string, keep: string[], trash: string[]) =>
  invoke<string[]>(Invokes.DuplicatesConfirm, { directory, keep, trash });

/** Work done on a photograph that trashing it would set aside. */
export const hasOwnWork = (member: DuplicateMember): boolean =>
  member.edited || member.rating > 0 || member.flag !== 0 || member.label !== '' || member.keywords > 0 || member.copies > 0;

/**
 * The photographs a group keeps by default: every one with work of its own
 * (edits, rating, flag, label, keywords, virtual copies), so no work is set
 * aside unasked; when none has any, the oldest import (the group comes
 * oldest first). The rest are suggested for the Trash.
 */
export function suggestedKeep(group: DuplicateGroup): Set<string> {
  const worked = group.filter(hasOwnWork).map((member) => member.path);
  return new Set(worked.length ? worked : group.slice(0, 1).map((member) => member.path));
}

/** The review's open state; the activity centre's Review opens it too. */
export const useDuplicatesDialog = create<{ open: boolean; show(): void; close(): void }>((set) => ({
  open: false,
  show: () => set({ open: true }),
  close: () => set({ open: false }),
}));
