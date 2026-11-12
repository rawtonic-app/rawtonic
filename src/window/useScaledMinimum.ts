import { useEffect } from 'react';
import { isTauri } from '@tauri-apps/api/core';
import { LogicalSize, PhysicalPosition } from '@tauri-apps/api/dpi';
import { currentMonitor, getCurrentWindow } from '@tauri-apps/api/window';

/** Physical-pixel placement after a minimum-size change. */
export function fitMinimumPosition(
  position: { x: number; y: number },
  size: { width: number; height: number },
  area: { position: { x: number; y: number }; size: { width: number; height: number } }
) {
  return {
    x: Math.max(area.position.x, Math.min(position.x, area.position.x + Math.max(0, area.size.width - size.width))),
    y: Math.max(area.position.y, Math.min(position.y, area.position.y + Math.max(0, area.size.height - size.height)))
  };
}

/** Keep usable chrome space without forcing the window beyond the work area. */
export function useScaledMinimum(scale: number, platform: string | null) {
  useEffect(() => {
    if (!isTauri() || platform === 'macos' || !platform) return;
    let disposed = false;
    const appWindow = getCurrentWindow();
    let generation = 0;
    let lastSize = '';
    const safeScale = Number.isFinite(scale) ? Math.max(1, scale) : 1;
    const update = (reposition = false) => {
      const request = ++generation;
      void Promise.all([currentMonitor(), appWindow.innerSize(), appWindow.outerSize()])
        .then(([monitor, inner, outer]) => {
          if (disposed || request !== generation) return;
          const available = monitor
            ? {
                width: (monitor.workArea.size.width - Math.max(0, outer.width - inner.width)) / monitor.scaleFactor,
                height: (monitor.workArea.size.height - Math.max(0, outer.height - inner.height)) / monitor.scaleFactor
              }
            : { width: 1000, height: 600 };
          const width = Math.min(1000 * safeScale, available.width);
          const height = Math.min(600 * safeScale, available.height);
          const key = width + ':' + height;
          if (key === lastSize) return;
          lastSize = key;
          return appWindow.setMinSize(new LogicalSize(width, height)).then(async () => {
            if (!reposition || !monitor || disposed || request !== generation) return;
            const [position, size, maximized, fullscreen] = await Promise.all([
              appWindow.outerPosition(),
              appWindow.outerSize(),
              appWindow.isMaximized(),
              appWindow.isFullscreen()
            ]);
            if (maximized || fullscreen) return;
            if (disposed || request !== generation) return;
            const fitted = fitMinimumPosition(position, size, monitor.workArea);
            if (fitted.x !== position.x || fitted.y !== position.y)
              await appWindow.setPosition(new PhysicalPosition(fitted.x, fitted.y));
          });
        })
        .catch(console.error);
    };
    update(true);
    const listeners = [appWindow.onMoved(() => update()), appWindow.onScaleChanged(() => update())];
    for (const listener of listeners) void listener.catch(console.error);
    return () => {
      disposed = true;
      for (const listener of listeners) void listener.then((stop) => stop()).catch(console.error);
    };
  }, [scale, platform]);
}
