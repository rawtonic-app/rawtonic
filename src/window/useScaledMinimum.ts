import { useEffect } from 'react';
import { invoke, isTauri } from '@tauri-apps/api/core';
import { PhysicalPosition } from '@tauri-apps/api/dpi';
import { Invokes } from '../components/ui/AppProperties';
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

type Monitor = { scaleFactor: number; workArea: { size: { width: number; height: number } } };

/**
 * The logical client minimum: 1000×600 times the interface size, cut to the
 * monitor's work area minus the normal window's frame (physical `frame`), in
 * whole logical pixels so the window's outer size never rounds past the work
 * area. Without a monitor: the base minimum.
 */
export function scaledMinimum(scale: number, monitor: Monitor | null, frame: { width: number; height: number }) {
  const safeScale = Number.isFinite(scale) ? Math.max(1, scale) : 1;
  if (!monitor) return { width: 1000, height: 600 };
  const fit = (area: number, border: number) => Math.floor((area - Math.max(0, border)) / monitor.scaleFactor);
  return {
    width: Math.min(Math.round(1000 * safeScale), fit(monitor.workArea.size.width, frame.width)),
    height: Math.min(Math.round(600 * safeScale), fit(monitor.workArea.size.height, frame.height))
  };
}

/**
 * Keep usable chrome space without forcing the window beyond the work area.
 *
 * The minimum changes only while the window is in its normal state
 * (`set_window_minimum`, window_limits.rs): tao's
 * `set_min_inner_size` re-applies the current inner size, which takes a
 * maximized window out of maximize at its maximized size (G76/WQG: at 125 %
 * on a 1280×752 work area the cap differs between the normal and the
 * maximized frame, so every maximize turned into a normal window that hung
 * off the screen). A change while maximized waits for the restore.
 */
export function useScaledMinimum(scale: number, platform: string | null) {
  useEffect(() => {
    if (!isTauri() || platform === 'macos' || !platform) return;
    let disposed = false;
    const appWindow = getCurrentWindow();
    let generation = 0;
    let lastSize = '';
    let timer: ReturnType<typeof setTimeout> | undefined;
    const update = (reposition = false) => {
      const request = ++generation;
      void Promise.all([
        currentMonitor(),
        appWindow.innerSize(),
        appWindow.outerSize(),
        appWindow.isMaximized(),
        appWindow.isMinimized(),
        appWindow.isFullscreen()
      ])
        .then(([monitor, inner, outer, maximized, minimized, fullscreen]) => {
          if (disposed || request !== generation) return;
          if (maximized || minimized || fullscreen) return;
          const { width, height } = scaledMinimum(scale, monitor, {
            width: outer.width - inner.width,
            height: outer.height - inner.height
          });
          const key = width + ':' + height;
          if (key === lastSize) return;
          lastSize = key;
          // Native: checks the window state and sets the minimum in one step
          // on the UI thread; false when the window was maximized meanwhile.
          return invoke<boolean>(Invokes.SetWindowMinimum, { width, height })
            .catch((error: unknown) => {
              lastSize = '';
              throw error;
            })
            .then(async (applied) => {
              // Not applied: the restore tries again.
              if (!applied) lastSize = '';
              if (!applied || !reposition || !monitor || disposed || request !== generation) return;
              const [position, size, nowMaximized] = await Promise.all([
                appWindow.outerPosition(),
                appWindow.outerSize(),
                appWindow.isMaximized()
              ]);
              if (nowMaximized || disposed || request !== generation) return;
              const fitted = fitMinimumPosition(position, size, monitor.workArea);
              if (fitted.x !== position.x || fitted.y !== position.y)
                await appWindow.setPosition(new PhysicalPosition(fitted.x, fitted.y));
            });
        })
        .catch(console.error);
    };
    // Moves and resizes come in bursts (dragging, the restore animation).
    const later = () => {
      clearTimeout(timer);
      timer = setTimeout(() => update(), 120);
    };
    update(true);
    const listeners = [appWindow.onMoved(later), appWindow.onResized(later), appWindow.onScaleChanged(later)];
    for (const listener of listeners) void listener.catch(console.error);
    return () => {
      disposed = true;
      clearTimeout(timer);
      for (const listener of listeners) void listener.then((stop) => stop()).catch(console.error);
    };
  }, [scale, platform]);
}
