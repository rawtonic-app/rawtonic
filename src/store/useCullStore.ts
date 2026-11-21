// SPDX-License-Identifier: AGPL-3.0-or-later
/**
 * Assisted culling's suggestions per photograph (`rawtonic_catalog::cull`):
 * misfires and documents scored from the content embeddings, closed eyes
 * from the eye check, minus what the photographer dismissed. Read from the
 * catalog for the listed photographs and again whenever scores, eyes or
 * dismissals change. A suggestion only: nothing here rates, flags, rejects
 * or removes a photograph.
 */
import { invoke } from '@tauri-apps/api/core';
import { create } from 'zustand';
import { Invokes, type CullCriterion } from '../components/ui/AppProperties';

export const CRITERIA: CullCriterion[] = ['misfire', 'document', 'eyesClosed'];

/** `cull_suggestions`: one photograph. */
export interface CullPhoto {
  suggested: CullCriterion[];
  dismissed: CullCriterion[];
  /** Misfire and document scores are current (else not read yet). */
  scored: boolean;
}

/** How many paths one request asks about. */
const BATCH = 2000;

interface CullState {
  /** By path as listed; a path without an entry is not in the library. */
  photos: Record<string, CullPhoto>;
  /** Bumped when scores, eyes or dismissals changed. */
  generation: number;
  load(directory: string, paths: string[]): Promise<void>;
  changed(): void;
  reset(): void;
}

export const useCullStore = create<CullState>((set, get) => ({
  photos: {},
  generation: 0,
  async load(directory, paths) {
    const asked = get().generation;
    const photos: Record<string, CullPhoto> = {};
    for (let at = 0; at < paths.length; at += BATCH) {
      Object.assign(
        photos,
        await invoke<Record<string, CullPhoto>>(Invokes.CullSuggestions, {
          directory,
          paths: paths.slice(at, at + BATCH)
        })
      );
    }
    // A change meanwhile asks again; this answer may predate it.
    if (get().generation === asked) set({ photos });
  },
  changed: () => set((s) => ({ generation: s.generation + 1 })),
  reset: () => set({ photos: {} })
}));

/** What the panel counts among `paths`: per criterion the photographs it
 * suggests, and how many have current scores. */
export function cullCounts(photos: Record<string, CullPhoto>, paths: string[]) {
  const suggested: Record<CullCriterion, string[]> = { misfire: [], document: [], eyesClosed: [] };
  let scored = 0;
  let known = 0;
  for (const path of paths) {
    const photo = photos[path];
    if (!photo) continue;
    known += 1;
    if (photo.scored) scored += 1;
    for (const criterion of photo.suggested) suggested[criterion].push(path);
  }
  return { suggested, scored, known };
}

/** Dismisses (or with `dismissed` false shows again) `criterion` for
 * `paths`; the paths whose state changed. */
export async function dismissSuggestion(
  directory: string,
  paths: string[],
  criterion: CullCriterion,
  dismissed: boolean
): Promise<string[]> {
  const changed = await invoke<string[]>(Invokes.CullDismiss, { directory, paths, criterion, dismissed });
  useCullStore.getState().changed();
  return changed;
}
