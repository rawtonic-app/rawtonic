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
