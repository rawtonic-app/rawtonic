import { invoke } from '@tauri-apps/api/core';
import type { TFunction } from 'i18next';
import { Images } from 'lucide-react';
import { toast } from 'react-toastify';
import { Invokes, type Option, type PreviewBuildProgress } from '../components/ui/AppProperties';

/**
 * The Develop view's long edge in device pixels as far as the webview can
 * tell before a photograph was shown: the Standard preview size for "Auto".
 * Develop's viewer takes most of the window; the native side uses the
 * viewer's real size once it has rendered a photograph.
 */
export function viewerDimHint(): number {
  const dpr = window.devicePixelRatio || 1;
  return Math.round(Math.max(window.innerWidth * 0.75, window.innerHeight * 0.85) * dpr);
}

/** Builds the Standard previews of `paths` in the background (with `full`, their 1:1 previews too). */
export function buildPreviews(paths: string[], full: boolean): Promise<PreviewBuildProgress> {
  return invoke<PreviewBuildProgress>(Invokes.BuildPreviews, { paths, full, viewerDim: viewerDimHint() });
}

/** Discards the 1:1 previews of `paths` (with `fullOnly` false, their Standard previews too). */
export function discardPreviews(paths: string[], fullOnly: boolean): Promise<number> {
  return invoke<number>(Invokes.DiscardPreviews, { paths, fullOnly });
}

/** "12.3 MB": decimal units, as Finder shows sizes. */
export function formatBytes(bytes: number): string {
  if (bytes < 1000) return `${bytes} B`;
  const units = ['kB', 'MB', 'GB', 'TB'];
  let value = bytes / 1000;
  let unit = 0;
  while (value >= 1000 && unit < units.length - 1) {
    value /= 1000;
    unit += 1;
  }
  return `${value >= 100 ? value.toFixed(0) : value.toFixed(1)} ${units[unit]}`;
}

/** The thumbnail menu's Previews submenu for the photographs in `paths`. */
export function previewsMenu(t: TFunction, paths: string[]): Option {
  const count = paths.length;
  const withCount = (label: string) => t('previews.menu.withCount', { label, n: count });
  const run = (action: () => Promise<unknown>) => () => {
    action().catch((err) => toast.error(t('previews.toasts.failed', { err: String(err) })));
  };
  const discard = (fullOnly: boolean) =>
    run(async () => {
      const removed = await discardPreviews(paths, fullOnly);
      toast.info(t('previews.toasts.discarded', { files: removed }));
    });
  return {
    label: t('previews.menu.title'),
    icon: Images,
    disabled: count === 0,
    submenu: [
      {
        label: withCount(t('previews.menu.buildStandard')),
        onClick: run(() => buildPreviews(paths, false)),
      },
      { label: withCount(t('previews.menu.buildFull')), onClick: run(() => buildPreviews(paths, true)) },
      { label: withCount(t('previews.menu.discardFull')), onClick: discard(true) },
      { label: withCount(t('previews.menu.discardAll')), onClick: discard(false) },
    ],
  };
}

/**
 * The long edge (px) the Develop preview renders at for a viewer of
 * `display` CSS px. Without grain: the display size (device pixels with full
 * DPI rendering) with 25 % headroom, in steps of 256 px, the full size from
 * 80 % of it. With grain: exactly the display's device pixels, so each
 * screen pixel shows one rendered pixel. Grain is a per-pixel texture: a
 * smaller render shown enlarged (the default, 1536 px for a 2070 px fit
 * view) showed 0.58× the grain strength of an export scaled to that size,
 * a larger one shown reduced 0.87× (notes: reference/grain.md).
 */
export function previewTargetResolution({
  display,
  dpr,
  fullDpi,
  zoomMultiplier,
  original,
  grain,
}: {
  display: { width: number; height: number };
  dpr: number;
  fullDpi: boolean;
  zoomMultiplier: number;
  original: { width: number; height: number } | null;
  grain: boolean;
}): number {
  const origMax = original && original.width > 0 && original.height > 0 ? Math.max(original.width, original.height) : 0;
  const shown = Math.max(display.width, display.height) * zoomMultiplier;
  if (grain) {
    const target = Math.max(Math.round(shown * dpr), 512);
    return origMax ? Math.min(target, origMax) : target;
  }
  let targetRes = Math.max(shown * (fullDpi ? dpr : 1) * 1.25, 512);
  if (origMax) {
    targetRes = Math.min(targetRes, origMax);
    if (targetRes >= origMax * 0.8) targetRes = origMax;
  }
  if (origMax && targetRes !== origMax) targetRes = Math.ceil(targetRes / 256) * 256;
  return Math.round(targetRes);
}
