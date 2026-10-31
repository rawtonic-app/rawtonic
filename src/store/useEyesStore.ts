// SPDX-License-Identifier: AGPL-3.0-or-later
/**
 * Eyes per face for culling (`face_eyes`): the Library's per-photograph
 * summary, read from the catalog for the listed photographs and again
 * whenever eyes are read or called. A suggestion only: nothing here rates,
 * flags, rejects or removes a photograph.
 */
import { invoke } from '@tauri-apps/api/core';
import { create } from 'zustand';
import { Invokes } from '../components/ui/AppProperties';

/** A reader's call, or the photographer's (never "uncertain"). */
export type EyeState = 'open' | 'closed' | 'notVisible' | 'uncertain';
export type EyeCall = Exclude<EyeState, 'uncertain'>;

export interface EyeFace {
  face: number;
  /** x, y, width, height as fractions of the upright, uncropped photograph. */
  bbox: [number, number, number, number];
  state: EyeState;
  ear: number | null;
  accessories: number | null;
  overrideState: EyeCall | null;
}

export interface PhotoEyes {
  /** Null while not read. */
  faces: EyeFace[] | null;
  error: string | null;
}

export interface EyeSummary {
  faces: number;
  closed: number;
  notVisible: number;
  uncertain: number;
}

/** The call that counts: the photographer's, else the reader's. */
export const effectiveEyes = (face: EyeFace): EyeState => face.overrideState ?? face.state;

/** How many paths one summary request asks about. */
const SUMMARY_BATCH = 2000;

interface EyesState {
  /** By path as listed; a path without an entry is not read (yet). */
  summaries: Record<string, EyeSummary>;
  /** Bumped when eyes were read or called: open views read theirs again. */
  generation: number;
  /** Reads the summaries of `paths` in the library at `directory` (the
   * listed photographs replace what was there). */
  load(directory: string, paths: string[]): Promise<void>;
  changed(): void;
  reset(): void;
}

export const useEyesStore = create<EyesState>((set, get) => ({
  summaries: {},
  generation: 0,
  async load(directory, paths) {
    const asked = get().generation;
    const summaries: Record<string, EyeSummary> = {};
    for (let at = 0; at < paths.length; at += SUMMARY_BATCH) {
      Object.assign(
        summaries,
        await invoke<Record<string, EyeSummary>>(Invokes.PhotoEyeSummaries, {
          directory,
          paths: paths.slice(at, at + SUMMARY_BATCH),
        }),
      );
    }
    // A change meanwhile asks again; this answer may predate it.
    if (get().generation === asked) set({ summaries });
  },
  changed: () => set((s) => ({ generation: s.generation + 1 })),
  reset: () => set({ summaries: {} }),
}));

/** Sets the photographer's call on one face (null: the reading counts again). */
export async function setFaceEyes(directory: string, path: string, face: number, state: EyeCall | null) {
  await invoke(Invokes.SetFaceEyes, { directory, path, face, state });
  useEyesStore.getState().changed();
}
