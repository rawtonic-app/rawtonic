import { getCurrentWindow } from '@tauri-apps/api/window';

// The photo-only view (F) shows the photograph alone in native full screen,
// as Lightroom's F does: its own Space on macOS, the whole monitor without
// borders on Windows and Linux. Leaving it restores the window as it was: a
// window that was already in full screen before F stays in full screen.

/** Whether F put the window into full screen (and so takes it out again). */
let ownsWindow = false;
/** Window changes run one after another, so quick presses keep their order. */
let queue: Promise<void> = Promise.resolve();

function enqueue(step: () => Promise<void>): Promise<void> {
  queue = queue.then(step).catch((error) => {
    console.warn('Full screen change failed:', error);
  });
  return queue;
}

/**
 * Puts the window into full screen for the photo-only view, unless it is
 * already. `stillWanted` is asked once the earlier changes are done, so a
 * view left again meanwhile does not take the window into full screen.
 */
export function enterPhotoFullScreen(stillWanted: () => boolean): Promise<void> {
  return enqueue(async () => {
    if (!stillWanted() || ownsWindow) return;
    const appWindow = getCurrentWindow();
    if (await appWindow.isFullscreen()) return;
    if (!stillWanted()) return;
    ownsWindow = true;
    await appWindow.setFullscreen(true);
  });
}

/** Takes the window out of full screen if the photo-only view put it there. */
export function leavePhotoFullScreen(): Promise<void> {
  return enqueue(async () => {
    if (!ownsWindow) return;
    ownsWindow = false;
    await getCurrentWindow().setFullscreen(false);
  });
}

/**
 * The window left full screen by other means (the green button, ⌃⌘F, the
 * window manager): nothing is left to restore.
 */
export function forgetPhotoFullScreen(): void {
  ownsWindow = false;
}

/** Test helper: whether the photo-only view owns the window's full screen. */
export function photoFullScreenOwnsWindow(): boolean {
  return ownsWindow;
}

/** Test helper: resolves once every queued window change is done. */
export function photoFullScreenSettled(): Promise<void> {
  return queue;
}
