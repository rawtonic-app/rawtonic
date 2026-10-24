import { useCallback } from "react";
import { invoke } from "@tauri-apps/api/core";
import { CLOSED_MERGE_MODAL, useUIStore } from "../store/useUIStore";
import { Invokes } from "../components/ui/AppProperties";
import type { MergeKind, MergeModalState } from "../store/useUIStore";
import type { CollageSpec } from "../components/modals/CollageModal";
import { noteTool } from '../components/contribute/usageStats';

// Merge job ids (merges.rs): unique per run, also across a webview reload.
let lastMergeJob = Date.now();

/** The merge dialog's state, changed only while it still waits for `job`. */
export function setMergeStateFor(
  job: number,
  update: (state: MergeModalState) => Partial<MergeModalState>,
) {
  useUIStore.getState().setUI((state) =>
    state.mergeModalState.job === job
      ? { mergeModalState: { ...state.mergeModalState, ...update(state.mergeModalState) } }
      : {},
  );
}

/** Closes the merge dialog; a merge still computing or waiting to be saved
 * is dropped. */
export function closeMergeModal() {
  const { job } = useUIStore.getState().mergeModalState;
  if (job !== null) {
    invoke(Invokes.DiscardMerge, { job }).catch((err) =>
      console.error("Failed to discard the merge:", err),
    );
  }
  useUIStore.getState().setUI({ mergeModalState: CLOSED_MERGE_MODAL });
}

export function useProductivityActions(refreshImageList: () => Promise<void>) {
  const setUI = useUIStore((state) => state.setUI);

  const handleRunMerge = useCallback(
    (kind: MergeKind, options: unknown) => {
      const { sourcePaths } = useUIStore.getState().mergeModalState;
      const job = ++lastMergeJob;
      setUI((state) => ({
        mergeModalState: {
          ...state.mergeModalState,
          job,
          isProcessing: true,
          error: null,
          progressMessage: null,
        },
      }));
      noteTool('merge');
      invoke(Invokes.MergePhotos, { paths: sourcePaths, kind, options, job }).catch(
        (err) => {
          setMergeStateFor(job, () => ({
            isProcessing: false,
            error: String(err),
          }));
        },
      );
    },
    [setUI],
  );

  const handleRefinishMerge = useCallback(
    async (options: unknown): Promise<void> => {
      const { job } = useUIStore.getState().mergeModalState;
      if (job === null) throw new Error("There is no merge in memory.");
      setMergeStateFor(job, () => ({ isProcessing: true, error: null }));
      try {
        const r: { base64: string; width: number; height: number } =
          await invoke(Invokes.RefinishMerge, {
            options,
            job,
          });
        setMergeStateFor(job, (state) => ({
          isProcessing: false,
          previewBase64: r.base64,
          report: {
            ...state.report,
            width: r.width,
            height: r.height,
          },
        }));
      } catch (err) {
        setMergeStateFor(job, () => ({ isProcessing: false }));
        throw err;
      }
    },
    [],
  );

  const handleSaveMerge = useCallback(
    async (autoSettings: boolean, fillEdges: boolean): Promise<string> => {
      const { job } = useUIStore.getState().mergeModalState;
      if (job === null) throw new Error("There is no merge to save.");
      const savedPath: string = await invoke(Invokes.SaveMerge, {
        autoSettings,
        fillEdges,
        job,
      });
      await refreshImageList();
      return savedPath;
    },
    [refreshImageList],
  );

  const handleSaveCollage = useCallback(
    async (spec: CollageSpec): Promise<string> => {
      const savedPath: string = await invoke(Invokes.SaveCollage, { spec });
      await refreshImageList();
      return savedPath;
    },
    [refreshImageList],
  );

  return {
    handleRunMerge,
    handleRefinishMerge,
    handleSaveMerge,
    handleSaveCollage,
  };
}
