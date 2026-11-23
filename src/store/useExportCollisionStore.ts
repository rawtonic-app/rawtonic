import { create } from 'zustand';
import type { CollisionSummary } from '../components/ui/ExportImportProperties';

/** The answer to "If a file exists: Ask" for one export run. */
export type CollisionAnswer = 'keepBoth' | 'skip' | 'replaceIfChanged';

interface CollisionQuestion {
  summary: CollisionSummary;
  resolve: (answer: CollisionAnswer | null) => void;
}

interface CollisionState {
  /** The question on screen (`ExportCollisionDialog`), if any. */
  pending: CollisionQuestion | null;
  /** Shows the question and resolves with the answer; null = cancelled. */
  ask: (summary: CollisionSummary) => Promise<CollisionAnswer | null>;
  answer: (answer: CollisionAnswer | null) => void;
}

export const useExportCollisionStore = create<CollisionState>((set, get) => ({
  pending: null,
  ask: (summary) =>
    new Promise((resolve) => {
      // A question still open is answered "cancel": one export, one question.
      get().pending?.resolve(null);
      set({ pending: { summary, resolve } });
    }),
  answer: (answer) => {
    const { pending } = get();
    set({ pending: null });
    pending?.resolve(answer);
  }
}));
