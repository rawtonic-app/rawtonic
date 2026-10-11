import { useEffect, useRef } from 'react';
import debounce from 'lodash.debounce';
import { useEditorStore } from '../store/useEditorStore';
import { useMultiViewStore } from '../store/useMultiViewStore';
import { useEditorActions } from './useEditorActions';
import { changedCopyableKeys } from '../utils/multiView';
import { frameDrawnMasks } from '../utils/maskGeometry';
import type { Adjustments } from '../utils/adjustments';

/** How long edits of the primary gather before they are written to the
 * other photographs (a slider drag writes once it pauses or ends). */
export const EDIT_ALL_SETTLE_MS = 250;

/**
 * "Edit all selected" in the multi view (Capture One's Edit All Selected
 * Variants, Lightroom's Auto Sync): while `enabled`, each edit of the open
 * (primary) photograph is applied to the other `selection` photographs as
 * pasting adjustments does: the same values of the copyable keys it changed,
 * drawn masks framed for each photograph. The primary keeps its own undo
 * history; the others are saved like a paste.
 */
export function useEditAllSelected(enabled: boolean, selection: string[]) {
  const { applyAdjustmentsToPaths } = useEditorActions();
  const selectionRef = useRef(selection);
  selectionRef.current = selection;
  const applyRef = useRef(applyAdjustmentsToPaths);
  applyRef.current = applyAdjustmentsToPaths;

  useEffect(() => {
    if (!enabled) return;
    let pending: Partial<Adjustments> = {};
    let source: { path: string; adjustments: Adjustments; width?: number; height?: number } | null = null;
    const flush = debounce(() => {
      const changes = pending;
      const from = source;
      pending = {};
      if (!from || Object.keys(changes).length === 0) return;
      const others = selectionRef.current.filter((path) => path !== from.path);
      if (others.length === 0) return;
      if (changes.masks) {
        changes.masks = frameDrawnMasks(changes.masks, from.adjustments, from.width, from.height);
      }
      void applyRef.current(changes, others)?.then(() => useMultiViewStore.getState().recipesSaved());
    }, EDIT_ALL_SETTLE_MS);
    const unsubscribe = useEditorStore.subscribe((state, previous) => {
      if (state.adjustments === previous.adjustments) return;
      const image = state.selectedImage;
      // Only edits: not a photograph being opened or its recipe arriving.
      if (!image?.isReady || !previous.selectedImage?.isReady || previous.selectedImage.path !== image.path) {
        return;
      }
      const changed = changedCopyableKeys(previous.adjustments, state.adjustments);
      if (Object.keys(changed).length === 0) return;
      if (source && source.path !== image.path) flush.flush();
      source = { path: image.path, adjustments: state.adjustments, width: image.width, height: image.height };
      pending = { ...pending, ...changed };
      flush();
    });
    return () => {
      unsubscribe();
      // Switching it off keeps what was already done.
      flush.flush();
    };
  }, [enabled]);
}
