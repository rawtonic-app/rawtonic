import { getCurrentWebview } from '@tauri-apps/api/webview';
import { getCurrentWindow } from '@tauri-apps/api/window';

// F puts the whole window into the system's full screen and back, as
// Capture One's full screen does: its own Space on macOS, the whole monitor
// without borders on Windows and Linux. The interface stays as it is, so
// work goes on unchanged; hiding the panels is Tab / Shift+Tab.

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
  queue = queue
    .then(async () => {
      const appWindow = getCurrentWindow();
      const target = !(await appWindow.isFullscreen());
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
