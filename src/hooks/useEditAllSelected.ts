import { useEffect, useRef } from 'react';
import debounce from 'lodash.debounce';
import { invoke } from '@tauri-apps/api/core';
import { Invokes } from '../components/ui/AppProperties';
import { useEditorStore } from '../store/useEditorStore';
import { useMultiViewStore } from '../store/useMultiViewStore';
import { useSettingsStore } from '../store/useSettingsStore';
import { onRecipeWritten, registerRecipeFlush, useEditorActions } from './useEditorActions';
import { changedCopyableKeys } from '../utils/multiView';
import { samePhotograph, withoutDetections } from '../utils/settingsClipboard';
import { frameDrawnMasks } from '../utils/maskGeometry';
import { globalImageCache } from '../utils/ImageLRUCache';
import { registerLibrarySwitchReset } from '../utils/quit';
import {
  sameAdjustmentValue,
  withClarityVersion,
  withGrainVersion,
  withLumaNoiseVersion,
  withSharpenVersion,
  type Adjustments,
} from '../utils/adjustments';

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

type ApplyToOthers = ReturnType<typeof useEditorActions>['applyAdjustmentsToPaths'];

/** Writes not settled yet, of every run. */
const writing = new Set<Promise<unknown>>();
/** What failed to be written to which photograph, of every run (also one
 * switched off since), with the photograph it came from: tried again by
 * every recipe flush until written, superseded or discarded. */
const unwritten = new Map<string, { changes: Partial<Adjustments>; from: string; apply: ApplyToOthers }>();
/** Runs' gathered edits, written at once by a recipe flush. */
const gathered = new Set<() => void>();

/** Settings written since a failure supersede it. */
onRecipeWritten((path, keys) => {
  const left = unwritten.get(path);
  if (!left) return;
  const rest = keys
    ? Object.fromEntries(Object.entries(left.changes).filter(([key]) => !keys.includes(key)))
    : {};
  if (Object.keys(rest).length > 0) unwritten.set(path, { ...left, changes: rest });
  else unwritten.delete(path);
});

/** Writes `changes` from `from` to `others`; keeps what failed for a retry
 * (what succeeded supersedes older failures, `onRecipeWritten`). */
function writeToOthers(apply: ApplyToOthers, changes: Partial<Adjustments>, others: string[], from: string) {
  const done = apply(changes, others, { openingNow: true, pasteFrom: from, replaceLayers: true });
  if (!done) return null;
  const settled = done.then((failed) => {
    for (const path of others) {
      if (!failed.includes(path)) continue;
      const left = unwritten.get(path);
      unwritten.set(path, { changes: { ...left?.changes, ...changes }, from, apply });
    }
  });
  writing.add(settled);
  void settled.then(() => writing.delete(settled));
  return done;
}

// Exporting, quitting or switching library (`flushRecipeSaves`): what the
// runs gathered is written now, into the library it was made in; what
// failed is tried again, and what still fails is named (an export stops; a
// switch asks Retry, Discard or Cancel, utils/quit.ts).
registerRecipeFlush(async () => {
  gathered.forEach((write) => write());
  while (writing.size > 0) await Promise.all(writing);
  for (const [path, { changes, from, apply }] of [...unwritten]) {
    // Superseded meanwhile.
    if (unwritten.get(path)?.changes !== changes) continue;
    unwritten.delete(path);
    const done = writeToOthers(apply, changes, [path], from);
    if (done) await done;
  }
  while (writing.size > 0) await Promise.all(writing);
  return [...unwritten.keys()];
});
// Past a switch's barrier they were written or discarded.
registerLibrarySwitchReset(() => {
  unwritten.clear();
  return () => {};
});

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
    // The photographs selected while the pending edits were made: they get
    // them even when the selection changes before they are written.
    let targets = new Set<string>();
    // Written to the others but not confirmed yet: still shown live.
    let inflight: Partial<Adjustments> = {};
    let writes = 0;
    let lastWrite: Promise<unknown> | null = null;
    // A library switch is past its barrier: edits made now belong to the
    // library being left and are not written into the next one.
    let switching = false;
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
      // The cells develop the values with the primary's sharpening response
      // (inferred after drawn masks are left out of the live patch).
      const shown = others.length > 0 ? livePatch({ ...inflight, ...pending }) : null;
      const patch = shown
        ? withClarityVersion(
            withGrainVersion(withLumaNoiseVersion(withSharpenVersion(shown, source?.adjustments), source?.adjustments), source?.adjustments),
            source?.adjustments,
          )
        : null;
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
      if (switching) {
        console.warn('Dropped Edit all selected changes made while the library was switched.');
        pending = {};
        targets = new Set();
        return;
      }
      const changes = pending;
      const from = source;
      const others = [...targets].filter((path) => path !== from?.path);
      pending = {};
      targets = new Set();
      if (!from || Object.keys(changes).length === 0) return;
      if (others.length === 0) return;
      const shownBefore = inflight;
      inflight = { ...inflight, ...changes };
      if (changes.masks) {
        changes.masks = frameDrawnMasks(changes.masks, from.adjustments, from.width, from.height);
      }
      const thisWrite = ++writes;
      // Written at the switch to another primary (see below): that one may
      // be one of the others, still opening.
      // The others get the primary's layers, with their AI detections made
      // on each of them (never the primary's bitmaps).
      const done = writeToOthers(applyRef.current, withClarityVersion(withGrainVersion(withLumaNoiseVersion(withSharpenVersion(changes, from.adjustments), from.adjustments), from.adjustments), from.adjustments), others, from.path);
      if (!done) {
        // Nothing was written: the cells show their recipes again.
        if (thisWrite === writes) inflight = shownBefore;
        sendLive();
        return;
      }
      lastWrite = done;
      void done.then(() => {
        // Written: the others show it from their recipes now.
        if (thisWrite === writes) inflight = {};
        useMultiViewStore.getState().recipesSaved();
        sendLive();
      });
    }, EDIT_ALL_SETTLE_MS);
    const writeGathered = () => flush.flush();
    gathered.add(writeGathered);
    // Past the barrier: written or discarded; nothing gathered from now
    // until the switch ends is written into the next library.
    const unregisterReset = registerLibrarySwitchReset(() => {
      switching = true;
      flush.cancel();
      pending = {};
      targets = new Set();
      return () => {
        switching = false;
      };
    });
    const unsubscribe = useEditorStore.subscribe((state, previous) => {
      if (switching) return;
      // Another primary: what the last one gathered is written now, before
      // the new one's loader reads its recipe (it may be one of the others).
      if (state.selectedImage?.path !== previous.selectedImage?.path) flush.flush();
      if (state.adjustments === previous.adjustments) return;
      const image = state.selectedImage;
      // Only edits: not a photograph being opened or its recipe arriving,
      // nor the loader adapting the recipe as it updates the photograph
      // (the full decode's size changing the default aspect ratio).
      if (
        !image?.isReady ||
        !previous.selectedImage?.isReady ||
        previous.selectedImage.path !== image.path ||
        image !== previous.selectedImage
      ) {
        return;
      }
      const changed = changedCopyableKeys(previous.adjustments, state.adjustments);
      // A setting the primary no longer has (As Shot over a custom white
      // balance) is taken away from the others too: `null` reaches them.
      for (const key of Object.keys(changed) as (keyof Adjustments)[]) {
        if (changed[key] === undefined) (changed as Record<string, unknown>)[key] = null;
      }
      // An AI detection on the primary is no edit of it: the other
      // photographs detect their own (sent, it would make them wait again,
      // and back). Its virtual copies share its pixels and take it.
      if (
        changed.masks &&
        sameAdjustmentValue(withoutDetections(previous.adjustments.masks), withoutDetections(state.adjustments.masks))
      ) {
        delete changed.masks;
        const copies = selectionRef.current.filter((path) => path !== image.path && samePhotograph(path, image.path));
        if (copies.length > 0) {
          copies.forEach((path) => globalImageCache.delete(path));
          // Framed by the primary, as every sync is (legacy unframed shapes).
          const masks = frameDrawnMasks(state.adjustments.masks, state.adjustments, image.width, image.height);
          void applyRef.current({ masks }, copies, {
            openingNow: true,
            pasteFrom: image.path,
            replaceLayers: true,
          });
        }
      }
      if (Object.keys(changed).length === 0) return;
      if (source && source.path !== image.path) flush.flush();
      source = { path: image.path, adjustments: state.adjustments, width: image.width, height: image.height };
      pending = { ...pending, ...changed };
      selectionRef.current.forEach((path) => {
        if (path === image.path) return;
        targets.add(path);
        // Opened before the write, it must read its recipe, not a cached one.
        globalImageCache.delete(path);
      });
      flush();
      sendLive();
    });
    return () => {
      unsubscribe();
      // Switching it off keeps what was already done (for the photographs
      // selected when it was done; what fails is kept for the next flush).
      flush.flush();
      gathered.delete(writeGathered);
      unregisterReset();
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
