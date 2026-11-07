/**
 * The sharpening view (Lightroom's Option-drag on its Detail sliders):
 * while a Sharpening slider is dragged with Option (Alt on Windows and
 * Linux) held, the display shows what the sharpening does: Masking its mask
 * in black and white (white = sharpened), Amount, Radius and Detail the
 * photo in grey, so sharpening is judged without colour. Pressing Option
 * during the drag shows it, letting go of Option or of the slider ends it
 * (notes: reference/ux-sharpen-mask-view-2026-11-07). View only, never in
 * the recipe or an export: the renderer gets it as `sharpenView`
 * (crates/engine `view_value`) and draws it with its own shader variant.
 */
import { listen } from '@tauri-apps/api/event';

export type SharpenView = 'mask' | 'grey';

/**
 * The view's shader variant compiles on first use and the frame meanwhile
 * shows the photo: when the app reports the variant ready
 * (`sharpen-view-ready`, on every platform) and the view is still shown,
 * `render` draws it, also while the pointer holds still. Returns the stop.
 */
export function onSharpenViewReady(shown: () => boolean, render: () => void): () => void {
  // Without the native app (tests, `npm run dev`) there is no event.
  const unlisten = listen('sharpen-view-ready', () => {
    if (shown()) render();
  }).catch(() => undefined);
  return () => void unlisten.then((stop) => stop?.());
}

/** The view each Sharpening slider shows; other sliders show none. */
const VIEWS: Record<string, SharpenView> = {
  sharpness: 'grey',
  sharpenRadius: 'grey',
  sharpenDetail: 'grey',
  sharpenMasking: 'mask'
};

export function sharpenViewFor(key: string): SharpenView | null {
  return VIEWS[key] ?? null;
}

/** What the display shows while the slider of `key` is dragged: its view
 * while Option is held, none otherwise. */
export function sharpenViewWhile(key: string, dragging: boolean, optionHeld: boolean): SharpenView | null {
  return dragging && optionHeld ? sharpenViewFor(key) : null;
}
