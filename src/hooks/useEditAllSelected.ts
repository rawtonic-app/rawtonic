import { useEffect, useRef } from 'react';
import debounce from 'lodash.debounce';
import { invoke } from '@tauri-apps/api/core';
import { Invokes } from '../components/ui/AppProperties';
import { useEditorStore } from '../store/useEditorStore';
import { useMultiViewStore } from '../store/useMultiViewStore';
import { useSettingsStore } from '../store/useSettingsStore';
import { useEditorActions } from './useEditorActions';
import { changedCopyableKeys } from '../utils/multiView';
import { frameDrawnMasks } from '../utils/maskGeometry';
import type { Adjustments } from '../utils/adjustments';

/** How long edits of the primary gather before they are written to the
 * other photographs (a slider drag writes once it pauses or ends). */
export const EDIT_ALL_SETTLE_MS = 250;

/** Shortest time between two live updates sent to the other cells (one
 * display frame); the native side develops them between the primary's
 * frames anyway. */
export const EDIT_ALL_LIVE_MS = 16;

/** The changes the other cells show live: everything but drawn masks, which
 * are framed for each photograph when written. Null: nothing to show. */
export function livePatch(changes: Partial<Adjustments>): Partial<Adjustments> | null {
  const { masks: _masks, ...rest } = changes;
  return Object.keys(rest).length > 0 ? rest : null;
}

/** The Edit-all run that last sent a live patch (a run that ends clears it
 * only when no newer run has sent its own). */
let liveOwner = 0;
let liveRuns = 0;

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
    const run = ++liveRuns;
    let pending: Partial<Adjustments> = {};
    // Written to the others but not confirmed yet: still shown live.
    let inflight: Partial<Adjustments> = {};
    let writes = 0;
    let lastWrite: Promise<unknown> | null = null;
    let source: { path: string; adjustments: Adjustments; width?: number; height?: number } | null = null;
    // Live: while the primary is edited the other cells follow, developed
    // natively between the primary's frames (multi_view.rs live_step).
    let sent = 'null';
    let lastSentAt = -Infinity;
    let liveTimer: ReturnType<typeof setTimeout> | null = null;
    const sendLiveNow = () => {
      liveTimer = null;
      lastSentAt = performance.now();
      const others = selectionRef.current.filter((path) => path !== source?.path);
      const patch = others.length > 0 ? livePatch({ ...inflight, ...pending }) : null;
      const key = JSON.stringify(patch);
      if (key === sent) return;
      sent = key;
      if (patch) liveOwner = run;
      void invoke(Invokes.SetMultiViewLive, { patch })?.catch(() => {});
    };
    const sendLive = () => {
      // Linux browser cells refresh after the settled recipe write.
      if (useSettingsStore.getState().osPlatform === 'linux') return;
      if (liveTimer !== null) return;
      const wait = EDIT_ALL_LIVE_MS - (performance.now() - lastSentAt);
      if (wait <= 0) sendLiveNow();
      else liveTimer = setTimeout(sendLiveNow, wait);
    };
    const flush = debounce(() => {
      const changes = pending;
      const from = source;
      pending = {};
      if (!from || Object.keys(changes).length === 0) return;
      const others = selectionRef.current.filter((path) => path !== from.path);
      if (others.length === 0) return;
      inflight = { ...inflight, ...changes };
      if (changes.masks) {
        changes.masks = frameDrawnMasks(changes.masks, from.adjustments, from.width, from.height);
      }
      const write = ++writes;
      const done = applyRef.current(changes, others);
      lastWrite = done ?? null;
      void done?.then(() => {
        // Written: the others show it from their recipes now.
        if (write === writes) inflight = {};
        useMultiViewStore.getState().recipesSaved();
        sendLive();
      });
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
      sendLive();
    });
    return () => {
      unsubscribe();
      // Switching it off keeps what was already done.
      flush.flush();
      if (liveTimer !== null) clearTimeout(liveTimer);
      // The live patch goes once the last write is in (else the cells would
      // show the old recipes for a moment), unless a newer run took over.
      const finish = () => {
        if (liveOwner !== run || sent === 'null') return;
        liveOwner = 0;
        void invoke(Invokes.SetMultiViewLive, { patch: null })?.catch(() => {});
      };
      if (lastWrite) void lastWrite.finally(finish);
      else finish();
    };
  }, [enabled]);
}
