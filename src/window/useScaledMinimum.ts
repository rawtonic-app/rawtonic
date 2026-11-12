import { useEffect } from 'react';
import { isTauri } from '@tauri-apps/api/core';
import { LogicalSize } from '@tauri-apps/api/dpi';
import { currentMonitor, getCurrentWindow } from '@tauri-apps/api/window';

/** Keep usable chrome space without forcing the window beyond the work area. */
export function useScaledMinimum(scale: number, platform: string | null) {
  useEffect(() => {
    if (!isTauri() || platform === 'macos' || !platform) return;
    let disposed = false;
    const window = getCurrentWindow();
    const safeScale = Number.isFinite(scale) ? Math.max(1, scale) : 1;
    void Promise.all([currentMonitor(), window.innerSize(), window.outerSize()])
      .then(([monitor, inner, outer]) => {
        if (disposed) return;
        const available = monitor
          ? {
              width: (monitor.workArea.size.width - Math.max(0, outer.width - inner.width)) / monitor.scaleFactor,
              height: (monitor.workArea.size.height - Math.max(0, outer.height - inner.height)) / monitor.scaleFactor
            }
          : { width: 1000, height: 600 };
        return window.setMinSize(
          new LogicalSize(Math.min(1000 * safeScale, available.width), Math.min(600 * safeScale, available.height))
        );
      })
      .catch(console.error);
    return () => {
      disposed = true;
    };
  }, [scale, platform]);
}
