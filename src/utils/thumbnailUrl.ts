// SPDX-License-Identifier: AGPL-3.0-or-later
/**
 * The URL a cached thumbnail is shown from. On Linux and Windows it goes
 * through the app's `rawtonic-thumb` scheme, which converts it into the
 * monitor profile (src-tauri/src/browser_colour.rs: WebKitGTK and WebView2
 * show images as device values); on macOS the asset protocol serves the
 * file and ColorSync matches it to the display. The `mp` query names the monitor transform the
 * URL was made for, so a new one (`monitor-colour-changed`) loads the
 * thumbnails again instead of the webview's cached copies.
 */
import { convertFileSrc } from '@tauri-apps/api/core';
import { listen } from '@tauri-apps/api/event';
import { platform } from '@tauri-apps/plugin-os';

export const THUMBNAIL_SCHEME = 'rawtonic-thumb';
export const MONITOR_COLOUR_CHANGED = 'monitor-colour-changed';

let converting: boolean | null = null;
let generation = 0;

function throughScheme() {
  if (converting === null) {
    try {
      converting = ['linux', 'windows'].includes(platform());
    } catch {
      converting = false;
    }
  }
  return converting;
}

/** The URL of the cached thumbnail file at `path`. */
export function thumbnailFileUrl(path: string): string {
  const file = path.replace(/\\/g, '/');
  return throughScheme() ? `${convertFileSrc(file, THUMBNAIL_SCHEME)}?mp=${generation}` : convertFileSrc(file);
}

/** Whether `url` is served by the thumbnail scheme. */
export function isSchemeThumbnail(url: URL): boolean {
  return url.protocol === `${THUMBNAIL_SCHEME}:` || url.hostname === `${THUMBNAIL_SCHEME}.localhost`;
}

/** `url` for the monitor transform `next` (other URLs as they are). */
export function forMonitorTransform(url: string, next: number): string {
  try {
    const parsed = new URL(url);
    if (!isSchemeThumbnail(parsed)) return url;
    parsed.searchParams.set('mp', String(next));
    return parsed.toString();
  } catch {
    return url;
  }
}

/** A new monitor transform: URLs made from now on name it. */
export function setMonitorTransform(next: number) {
  generation = next;
}

/** Tests: the platform as given. */
export function setThumbnailPlatformForTests(throughScheme: boolean | null) {
  converting = throughScheme;
  generation = 0;
}

/** Calls `handler` after every new monitor transform. Returns the stop. */
export function onMonitorColourChanged(handler: () => void): () => void {
  // Without the native app (tests, `npm run dev`) there is no event.
  const unlisten = listen(MONITOR_COLOUR_CHANGED, () => handler()).catch(() => undefined);
  return () => void unlisten.then((stop) => stop?.());
}
