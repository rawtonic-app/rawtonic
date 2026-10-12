import { getCurrentWindow } from '@tauri-apps/api/window';

// F puts the whole window into the system's full screen and back, as
// Capture One's full screen does: its own Space on macOS, the whole monitor
// without borders on Windows and Linux. The interface stays as it is, so
// work goes on unchanged; hiding the panels is Tab / Shift+Tab.

/** Window changes run one after another, so quick presses keep their order. */
let queue: Promise<void> = Promise.resolve();

/** Toggles the window's full screen. */
export function toggleWindowFullScreen(): Promise<void> {
  queue = queue
    .then(async () => {
      const appWindow = getCurrentWindow();
      await appWindow.setFullscreen(!(await appWindow.isFullscreen()));
    })
    .catch((error) => {
      console.warn('Full screen change failed:', error);
    });
  return queue;
}
