import { reportBrowserPreviewLoad } from '../hooks/usePerformanceDiagnostics';
import type { InteractivePatch } from '../store/useEditorStore';

/**
 * Slider-drag drafts of the browser renderer (Linux) as an <img> over the
 * viewer, outside React. As an SVG <image> each draft was decoded on WebKit's
 * main thread while painting (notes: benchmark-results/
 * linux-slider-drag-2026-10-17/); here `decode()` runs on WebKit's decoder
 * threads and a draft replaces the shown one only once it is decoded, newest
 * job first. Stale drafts never cover a
 * final frame: `retireDraftPatches` drops every decode still in flight.
 */
let host: HTMLDivElement | null = null;
let shownJob = 0;
// Decodes started before a retire or clear are dropped when they finish.
let generation = 0;

export function registerDraftPatchHost(element: HTMLDivElement | null) {
  if (host && host !== element) host.replaceChildren();
  host = element;
}

/** Shows `patch` (job `job`) once decoded; false without a viewer to show it in. */
export function showDraftPatch(patch: InteractivePatch, job: number): boolean {
  const target = host;
  if (!target) return false;
  const started = generation;
  const image = new Image();
  image.decoding = 'async';
  image.src = patch.url;
  image.decode().then(
    () => {
      if (host !== target || started !== generation || job <= shownJob) return revoke(patch.url);
      shownJob = job;
      Object.assign(image.style, {
        position: 'absolute',
        left: `${patch.normX * 100}%`,
        top: `${patch.normY * 100}%`,
        width: `${patch.normW * 100}%`,
        height: `${patch.normH * 100}%`,
      });
      const previous = target.firstElementChild as HTMLImageElement | null;
      target.replaceChildren(image);
      if (previous) revoke(previous.src);
      reportBrowserPreviewLoad(patch.url, true);
    },
    () => revoke(patch.url),
  );
  return true;
}

/** Drafts still decoding will not be shown (a final frame is on its way).
 * The shown draft stays until `clearDraftPatch`, so the photo does not flash
 * back to the old base while the final frame loads. */
export function retireDraftPatches() {
  generation++;
}

export function clearDraftPatch() {
  generation++;
  const previous = host?.firstElementChild as HTMLImageElement | null;
  host?.replaceChildren();
  if (previous) revoke(previous.src);
}

function revoke(url: string) {
  if (url.startsWith('blob:')) URL.revokeObjectURL(url);
}
