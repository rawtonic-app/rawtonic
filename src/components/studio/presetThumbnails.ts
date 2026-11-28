import { invoke } from '@tauri-apps/api/core';
import { useEffect, useReducer, useRef, useState } from 'react';
import { Invokes } from '../ui/AppProperties';
import { useEditorStore } from '../../store/useEditorStore';
import type { Adjustments } from '../../utils/adjustments';
import { reportPerformance } from '../../hooks/usePerformanceDiagnostics';
import { splitTiles } from './StudioProfileBrowser';

/** A grid tile's edge in points (a 2× screen renders twice that). */
export const PRESET_TILE_POINTS = 96;
/** Tiles per native call: the first rows arrive while later ones render. */
const BATCH = 8;
/** Rendered tiles kept (object URLs of small JPEGs, ~10 KB each). */
const CACHE_LIMIT = 400;

/** One preset's or style's tile: what it would make of the photo. */
export interface PresetTile {
  /** The preset's id (stable across renames). */
  key: string;
  /** Changes with the preset's settings (an Update, an import over it). */
  version: string;
  /** The open photo with the preset, as its hover preview shows it; null
   * when it cannot be shown without developing the RAW again. */
  previewOf(recipe: Adjustments): Adjustments | null;
}

/** `next`'s keys that differ from `base`'s, null for the ones it dropped
 * (`profile_thumbnails`' variant form). */
export function recipeChanges(base: Adjustments, next: Adjustments): Record<string, unknown> {
  const from = base as unknown as Record<string, unknown>;
  const to = next as unknown as Record<string, unknown>;
  const out: Record<string, unknown> = {};
  for (const [key, value] of Object.entries(to)) {
    if (value === from[key]) continue;
    if (value !== undefined && JSON.stringify(value) === JSON.stringify(from[key])) continue;
    out[key] = value === undefined ? null : value;
  }
  for (const key of Object.keys(from)) if (!(key in to) && from[key] !== undefined) out[key] = null;
  return out;
}

/** A short content hash (cyrb53) of a value's JSON: the cache key of a recipe or preset. */
export function contentKey(value: unknown): string {
  const text = JSON.stringify(value) ?? '';
  let h1 = 0xdeadbeef;
  let h2 = 0x41c6ce57;
  for (let i = 0; i < text.length; i++) {
    const c = text.charCodeAt(i);
    h1 = Math.imul(h1 ^ c, 2654435761);
    h2 = Math.imul(h2 ^ c, 1597334677);
  }
  h1 = Math.imul(h1 ^ (h1 >>> 16), 2246822507) ^ Math.imul(h2 ^ (h2 >>> 13), 3266489909);
  h2 = Math.imul(h2 ^ (h2 >>> 16), 2246822507) ^ Math.imul(h1 ^ (h1 >>> 13), 3266489909);
  return `${text.length.toString(36)}-${(h2 >>> 0).toString(36)}${(h1 >>> 0).toString(36)}`;
}

/** Rendered tiles by (photo, recipe, preset, preset version), least recently used first. */
const cache = new Map<string, string>();
/** Tiles the native side could not draw on the shown base (a preset with a crop). */
const unavailable = new Set<string>();

/** Keeps `url` for `key`; beyond the limit drops the least recently used
 * tiles, except the one just kept and those on screen (`onScreen`). */
function remember(key: string, url: string, onScreen: Set<string>) {
  const old = cache.get(key);
  if (old && old !== url) URL.revokeObjectURL(old);
  cache.delete(key);
  cache.set(key, url);
  for (const [k, u] of cache) {
    if (cache.size <= CACHE_LIMIT) break;
    if (k === key || onScreen.has(u)) continue;
    URL.revokeObjectURL(u);
    cache.delete(k);
  }
}

/** Forgets every rendered tile (tests). */
export function clearPresetThumbnailCache() {
  cache.forEach((url) => URL.revokeObjectURL(url));
  cache.clear();
  unavailable.clear();
}

let generations = 0;

/** A pointer held on the preset Amount: tiles wait as during a slider drag. */
let held = false;
const holdListeners = new Set<() => void>();
/** Holds the tiles until the returned function is called (the Amount's
 * pointer comes up): no new batch starts and the one in flight stops. */
export function holdPresetThumbnails(): () => void {
  held = true;
  holdListeners.forEach((listener) => listener());
  return () => {
    if (!held) return;
    held = false;
    holdListeners.forEach((listener) => listener());
  };
}
const paused = () => held || useEditorStore.getState().isSliderDragging;

/**
 * The presets grid's tiles: the open photograph rendered natively with each
 * preset (`preset_thumbnails`, from the shown frame's preview base) for the
 * tiles on screen only, in batches; again after an edit (the old tile stays
 * until the new one arrives), never during a drag. Another photograph, a
 * closed panel or scrolling cancels what is still rendering. Tiles are kept
 * per (photo, recipe, preset version), so going back to one is instant.
 * Returns each shown tile's object URL, or `false` when it has no preview.
 */
export function usePresetThumbnails(
  enabled: boolean,
  imagePath: string | undefined,
  tiles: PresetTile[],
  /** Changes when the shown tiles change (folders opened, search). */
  layoutKey: unknown
): Map<string, string | false> {
  const [, redraw] = useReducer((n: number) => n + 1, 0);
  const tilesRef = useRef(tiles);
  tilesRef.current = tiles;
  // The tiles' identity for the effect: a re-render with the same presets is no new request.
  const tilesSignature = tiles.map((tile) => `${tile.key}:${tile.version}`).join('|');
  // Last tile shown per preset for this photo: kept until its new one arrives.
  const shown = useRef(new Map<string, string | false>());
  const waiting = useRef(false);
  const [visible, setVisible] = useState<Set<string>>(new Set());
  const shownSince = useRef(0);
  const [tick, bump] = useReducer((n: number) => n + 1, 0);
  // The batch being rendered, and whether a drag that began meanwhile stopped it.
  const inFlight = useRef(0);
  const stoppedForDrag = useRef(false);

  // Edits (not during drags) and, while no base is cached, frames. A drag
  // stops the batch in flight; the rest is rendered when it ends.
  useEffect(() => {
    if (!enabled) return;
    const stop = () => {
      if (!inFlight.current) return;
      stoppedForDrag.current = true;
      void invoke(Invokes.CancelPresetThumbnails, { generation: inFlight.current }).catch(() => undefined);
    };
    let last = useEditorStore.getState();
    const unsubscribe = useEditorStore.subscribe((state) => {
      const before = last;
      last = state;
      if (state.isSliderDragging && !before.isSliderDragging) stop();
      if (paused()) return;
      if (
        state.adjustments !== before.adjustments ||
        before.isSliderDragging ||
        (waiting.current && state.previewFrames !== before.previewFrames)
      )
        bump();
    });
    const onHold = () => (held ? stop() : paused() || bump());
    holdListeners.add(onHold);
    return () => {
      unsubscribe();
      holdListeners.delete(onHold);
    };
  }, [enabled]);

  // Which tiles are on screen (a closed panel shows none).
  useEffect(() => {
    if (!enabled) {
      setVisible(new Set());
      return;
    }
    // Cells on screen; a favourite has two (Favorites and its own group).
    const seen = new Set<HTMLElement>();
    let observer: IntersectionObserver | null = null;
    const raf = requestAnimationFrame(() => {
      const cells = document.querySelectorAll<HTMLElement>('[data-preset-tile]');
      if (typeof IntersectionObserver === 'undefined') {
        // No observer (tests): the tiles laid out count as shown.
        setVisible(
          new Set(
            [...cells].filter((cell) => !cell.closest('details:not([open])')).map((cell) => cell.dataset.presetTile!)
          )
        );
        return;
      }
      observer = new IntersectionObserver((entries) => {
        for (const entry of entries) {
          const cell = entry.target as HTMLElement;
          if (entry.isIntersecting) seen.add(cell);
          else seen.delete(cell);
        }
        setVisible(new Set([...seen].flatMap((cell) => cell.dataset.presetTile ?? [])));
      });
      cells.forEach((cell) => observer?.observe(cell));
    });
    return () => {
      cancelAnimationFrame(raf);
      observer?.disconnect();
    };
  }, [enabled, layoutKey]);

  // Another photograph: its own tiles.
  useEffect(() => {
    shown.current = new Map();
    shownSince.current = 0;
    redraw();
  }, [imagePath]);

  useEffect(() => {
    if (!enabled || !imagePath || visible.size === 0) return;
    let gone = false;
    let generation = 0;
    if (!shownSince.current) shownSince.current = performance.now();
    const run = async () => {
      const state = useEditorStore.getState();
      if (gone || paused() || !state.selectedImage?.isReady || state.selectedImage.path !== imagePath) return;
      const recipe = state.adjustments;
      const recipeKey = contentKey(recipe);
      const live = new Set(cache.values());
      const want: { tile: PresetTile; key: string; variant: Record<string, unknown> }[] = [];
      for (const tile of tilesRef.current) {
        if (!visible.has(tile.key)) continue;
        const key = `${imagePath}\0${recipeKey}\0${tile.key}\0${tile.version}`;
        const url = cache.get(key);
        if (url) {
          shown.current.set(tile.key, url);
          continue;
        }
        // Its old tile was dropped from the cache (revoked): not shown any more.
        const old = shown.current.get(tile.key);
        if (typeof old === 'string' && !live.has(old)) shown.current.delete(tile.key);
        if (unavailable.has(key)) {
          shown.current.set(tile.key, false);
          continue;
        }
        const previewed = tile.previewOf(recipe);
        if (!previewed) {
          shown.current.set(tile.key, false);
          continue;
        }
        want.push({ tile, key, variant: recipeChanges(recipe, previewed) });
      }
      redraw();
      const size = Math.round(PRESET_TILE_POINTS * Math.min(2, window.devicePixelRatio || 1));
      for (let at = 0; at < want.length && !gone; at += BATCH) {
        // A drag that started meanwhile has the GPU: the rest waits for its end (`bump`).
        if (paused()) return;
        const batch = want.slice(at, at + BATCH);
        generation = ++generations;
        inFlight.current = generation;
        stoppedForDrag.current = false;
        let answer: ArrayBuffer;
        try {
          answer = await invoke<ArrayBuffer>(Invokes.PresetThumbnails, {
            path: imagePath,
            adjustments: recipe,
            variants: batch.map((item) => item.variant),
            size,
            generation
          });
        } catch (error) {
          console.error('Preset thumbnails failed:', error);
          return;
        } finally {
          if (inFlight.current === generation) inFlight.current = 0;
        }
        if (gone) return;
        const interrupted = stoppedForDrag.current;
        if (!answer || answer.byteLength === 0) {
          // No preview base of this recipe yet: again with the next frame.
          waiting.current = true;
          return;
        }
        waiting.current = false;
        const onScreen = new Set(
          [...visible].map((k) => shown.current.get(k)).filter((u): u is string => typeof u === 'string')
        );
        splitTiles(answer, batch.length).forEach((blob, index) => {
          const { tile, key } = batch[index];
          if (!blob) {
            // Stopped for a drag: rendered after it. Otherwise (this run is
            // current) it cannot be drawn on this base.
            if (interrupted) return;
            unavailable.add(key);
            shown.current.set(tile.key, false);
            return;
          }
          const url = URL.createObjectURL(blob);
          remember(key, url, onScreen);
          shown.current.set(tile.key, url);
          onScreen.add(url);
        });
        redraw();
        if (interrupted) return;
      }
      if (!gone && shownSince.current && [...visible].every((key) => shown.current.has(key))) {
        reportPerformance('presets_grid_visibleTiles', {
          ms: Math.round((performance.now() - shownSince.current) * 10) / 10,
          tiles: visible.size,
          rendered: want.length
        });
        shownSince.current = 0;
      }
    };
    // Edits come in bursts (a click, a pasted recipe): render after them.
    const timer = setTimeout(() => void run(), 60);
    return () => {
      gone = true;
      clearTimeout(timer);
      if (generation) void invoke(Invokes.CancelPresetThumbnails, { generation }).catch(() => undefined);
    };
  }, [enabled, imagePath, visible, tick, tilesSignature]);

  // Hidden (list view, closed panel): the next showing is timed again.
  useEffect(() => {
    if (!enabled || visible.size === 0) shownSince.current = 0;
  }, [enabled, visible]);

  return shown.current;
}
