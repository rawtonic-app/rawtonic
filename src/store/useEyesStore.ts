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
/** Eye focus as read (soft or missed are one call), or the photographer's
 * (never "uncertain"). */
export type FocusState = 'sharp' | 'soft' | 'uncertain';
export type FocusCall = Exclude<FocusState, 'uncertain'>;

export interface EyeFace {
  face: number;
  /** x, y, width, height as fractions of the upright, uncropped photograph. */
  bbox: [number, number, number, number];
  state: EyeState;
  ear: number | null;
  accessories: number | null;
  overrideState: EyeCall | null;
  /** Null when read before eye focus was. */
  focus: FocusState | null;
  /** The edge sigma behind it (px at a fixed eye distance). */
  focusSigma: number | null;
  focusOverride: FocusCall | null;
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
  /** Faces whose eyes are soft. */
  soft: number;
}

/** The call that counts: the photographer's, else the reader's. */
export const effectiveEyes = (face: EyeFace): EyeState => face.overrideState ?? face.state;
/** The eye-focus call that counts (null: not read). */
export const effectiveFocus = (face: EyeFace): FocusState | null => face.focusOverride ?? face.focus;

/** How many paths one summary request asks about. */
const SUMMARY_BATCH = 2000;

/** The face the Loupe's strip points at on the open photograph: the one
 * hovered or focused there, else the one pinned by a click. */
export interface FaceMark {
  path: string;
  faces: EyeFace[];
  hovered: number | null;
  pinned: number | null;
}

/** The index of the face whose box the photograph shows, if any. */
export const markedFace = (mark: FaceMark | null): number | null =>
  mark ? (mark.hovered ?? mark.pinned) : null;

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
  /** The Loupe strip's face box (null: none shown). */
  faceMark: FaceMark | null;
  /** The strip's faces of `path` (a new photograph starts unmarked). */
  showFaces(path: string, faces: EyeFace[] | null): void;
  hoverFace(path: string, index: number | null): void;
  /** Pins `index` (null unpins). */
  pinFace(path: string, index: number | null): void;
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
  faceMark: null,
  showFaces: (path, faces) =>
    set((s) => {
      if (!faces?.length) return { faceMark: null };
      const mark = s.faceMark?.path === path ? s.faceMark : null;
      // The same photograph read again keeps its marks on the same faces.
      const keep = (i: number | null | undefined) => {
        const id = i == null ? undefined : mark?.faces[i]?.face;
        const at = id === undefined ? -1 : faces.findIndex((f) => f.face === id);
        return at < 0 ? null : at;
      };
      return { faceMark: { path, faces, hovered: keep(mark?.hovered), pinned: keep(mark?.pinned) } };
    }),
  hoverFace: (path, index) =>
    set((s) => (s.faceMark?.path === path ? { faceMark: { ...s.faceMark, hovered: index } } : {})),
  pinFace: (path, index) =>
    set((s) => (s.faceMark?.path === path ? { faceMark: { ...s.faceMark, pinned: index } } : {})),
}));

/** Sets the photographer's call on one face (null: the reading counts again). */
export async function setFaceEyes(directory: string, path: string, face: number, state: EyeCall | null) {
  await invoke(Invokes.SetFaceEyes, { directory, path, face, state });
  useEyesStore.getState().changed();
}

/** Sets the photographer's eye-focus call on one face (null: the reading
 * counts again). */
export async function setFaceFocus(directory: string, path: string, face: number, focus: FocusCall | null) {
  await invoke(Invokes.SetFaceFocus, { directory, path, face, focus });
  useEyesStore.getState().changed();
}
