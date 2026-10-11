import { create } from "zustand";

/** Shares the one top bar between the session (left: job, ingest) and the
 * workspace (middle: Library/Develop, right: export, preferences), and lets
 * workspace controls flag photographs through the session's ordered writes. */
interface SessionBarState {
  center: HTMLElement | null;
  end: HTMLElement | null;
  /** Pick (1), reject (-1) or clear (0) the targeted photographs. */
  mark: ((flag: number) => Promise<void> | undefined) | null;
  set(patch: Partial<Omit<SessionBarState, "set">>): void;
}

export const useSessionBar = create<SessionBarState>((set) => ({
  center: null,
  end: null,
  mark: null,
  set: (patch) => set(patch),
}));
