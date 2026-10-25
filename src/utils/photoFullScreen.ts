import { getCurrentWebview } from '@tauri-apps/api/webview';
import { getCurrentWindow } from '@tauri-apps/api/window';

// The window's full screen (F11; macOS also ⌃⌘F and the View menu): its own
// Space on macOS, the whole monitor without borders on Windows and Linux.
// The interface stays as it is; hiding the panels is Tab / Shift+Tab. The
// photograph-only full screen (F) also takes the whole screen through it.

/** Window changes run one after another, so quick presses keep their order. */
let queue: Promise<void> = Promise.resolve();

const pause = (ms: number) => new Promise((resolve) => setTimeout(resolve, ms));

/**
 * Gives the keyboard back to the page. macOS's full screen transition leaves
 * the web view without keyboard focus, so the next F (or any shortcut) went
 * nowhere until a click.
 */
export async function refocusWebview(): Promise<void> {
  try {
    await getCurrentWindow().setFocus();
    await getCurrentWebview().setFocus();
  } catch (error) {
    console.warn('Refocusing the window failed:', error);
  }
}

/** Toggles the window's full screen. */
export function toggleWindowFullScreen(): Promise<void> {
  return changeWindowFullScreen((current) => !current);
}

/** Whether the photograph-only full screen put the window into full screen
 * itself (so leaving it gives the window back as it was). */
let windowTakenByPhoto = false;

/**
 * The photograph-only full screen covers the whole screen: on entering, a
 * window that is not in full screen goes into it; on leaving, only a window
 * it put there comes back. One already in full screen stays as it is.
 */
export function followPhotoFullScreen(on: boolean): Promise<void> {
  return changeWindowFullScreen((current) => {
    if (on) {
      windowTakenByPhoto = !current;
      return true;
    }
    const target = windowTakenByPhoto ? false : current;
    windowTakenByPhoto = false;
    return target;
  });
}

function changeWindowFullScreen(next: (current: boolean) => boolean): Promise<void> {
  queue = queue
    .then(async () => {
      const appWindow = getCurrentWindow();
      const current = await appWindow.isFullscreen();
      const target = next(current);
      if (target === current) return;
      await appWindow.setFullscreen(target);
      // The transition is animated on macOS: wait until the window reports
      // the new state (at most ~2 s), then take the keyboard focus again.
      for (let i = 0; i < 20 && (await appWindow.isFullscreen()) !== target; i++) await pause(100);
      await pause(50);
      await refocusWebview();
    })
    .catch((error) => {
      console.warn('Full screen change failed:', error);
    });
  return queue;
}
