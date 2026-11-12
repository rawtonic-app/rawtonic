import { act, cleanup, renderHook, waitFor } from '@testing-library/react';
import { afterEach, beforeEach, expect, it, vi } from 'vitest';
const { setMinSize, currentMonitor, moved, resized, scaleChanged, windowState } = vi.hoisted(() => ({
  setMinSize: vi.fn().mockResolvedValue(undefined),
  currentMonitor: vi.fn(),
  moved: vi.fn(),
  resized: vi.fn(),
  scaleChanged: vi.fn(),
  windowState: { maximized: false }
}));
vi.mock('@tauri-apps/api/core', () => ({
  isTauri: () => true,
  invoke: (command: string, args: { width: number; height: number }) =>
    command === 'set_window_minimum'
      ? setMinSize(args).then(() => !windowState.maximized)
      : Promise.reject(new Error(command))
}));
vi.mock('@tauri-apps/api/window', () => ({
  currentMonitor,
  getCurrentWindow: () => ({
    setMinSize,
    isMaximized: async () => windowState.maximized,
    isMinimized: async () => false,
    isFullscreen: async () => false,
    outerPosition: async () => ({ x: 0, y: 0 }),
    setPosition: async () => {},
    onMoved: async (callback: () => void) => {
      moved.mockImplementation(callback);
      return vi.fn();
    },
    onResized: async (callback: () => void) => {
      resized.mockImplementation(callback);
      return vi.fn();
    },
    onScaleChanged: async (callback: () => void) => {
      scaleChanged.mockImplementation(callback);
      return vi.fn();
    },
    innerSize: async () => ({ width: 1000, height: 600 }),
    outerSize: async () => ({ width: 1016, height: 616 })
  })
}));
import { fitMinimumPosition, scaledMinimum, useScaledMinimum } from './useScaledMinimum';
afterEach(() => {
  cleanup();
  vi.useRealTimers();
});
beforeEach(() => {
  setMinSize.mockClear();
  windowState.maximized = false;
  currentMonitor.mockResolvedValue({
    scaleFactor: 1,
    workArea: { position: { x: 0, y: 0 }, size: { width: 1920, height: 1080 } }
  });
});
it.each([1, 1.12, 1.25])('scales the usable window floor at %s', async (scale) => {
  renderHook(() => useScaledMinimum(scale, 'windows'));
  await waitFor(() =>
    expect(setMinSize).toHaveBeenCalledExactlyOnceWith(
      expect.objectContaining({ width: Math.round(1000 * scale), height: Math.round(600 * scale) })
    )
  );
});
it('caps the floor at the monitor work area minus the frame', async () => {
  currentMonitor.mockResolvedValue({
    scaleFactor: 1.5,
    workArea: { position: { x: 0, y: 0 }, size: { width: 1920, height: 1041 } }
  });
  renderHook(() => useScaledMinimum(1.25, 'windows'));
  await waitFor(() =>
    expect(setMinSize).toHaveBeenCalledExactlyOnceWith(expect.objectContaining({ width: 1250, height: 683 }))
  );
});
it('updates the floor when Preferences changes the interface size', async () => {
  const { rerender } = renderHook(({ scale }) => useScaledMinimum(scale, 'windows'), { initialProps: { scale: 1.25 } });
  await waitFor(() => expect(setMinSize).toHaveBeenCalledTimes(1));
  rerender({ scale: 1 });
  await waitFor(() =>
    expect(setMinSize).toHaveBeenLastCalledWith(expect.objectContaining({ width: 1000, height: 600 }))
  );
});
it.each(['macos', null])('leaves %s sizing alone', (platform) => {
  renderHook(() => useScaledMinimum(1.25, platform));
  expect(setMinSize).not.toHaveBeenCalled();
});

it('recomputes the cap when moved to the laptop display or its DPI changes', async () => {
  renderHook(() => useScaledMinimum(1.25, 'windows'));
  await waitFor(() => expect(setMinSize).toHaveBeenCalledTimes(1));
  currentMonitor.mockResolvedValue({
    scaleFactor: 1.5,
    workArea: { position: { x: 0, y: 0 }, size: { width: 1920, height: 1041 } }
  });
  await act(async () => moved());
  await waitFor(() => expect(setMinSize).toHaveBeenLastCalledWith(expect.objectContaining({ height: 683 })));
  currentMonitor.mockResolvedValue({
    scaleFactor: 1,
    workArea: { position: { x: 0, y: 0 }, size: { width: 1920, height: 1080 } }
  });
  await act(async () => scaleChanged());
  await waitFor(() =>
    expect(setMinSize).toHaveBeenLastCalledWith(expect.objectContaining({ width: 1250, height: 750 }))
  );
});
it('uses the base minimum when no monitor is known and scale is invalid', async () => {
  currentMonitor.mockResolvedValue(null);
  renderHook(() => useScaledMinimum(Number.NaN, 'windows'));
  await waitFor(() =>
    expect(setMinSize).toHaveBeenCalledExactlyOnceWith(expect.objectContaining({ width: 1000, height: 600 }))
  );
});

it('keeps the 125% minimum fully inside the physical work area', () => {
  expect(
    fitMinimumPosition(
      { x: 100, y: 100 },
      { width: 2526, height: 1504 },
      {
        position: { x: 0, y: 0 },
        size: { width: 2560, height: 1552 }
      }
    )
  ).toEqual({ x: 34, y: 48 });
});

it('preserves an in-bounds position on a monitor left of the primary', () => {
  expect(
    fitMinimumPosition(
      { x: -1800, y: 50 },
      { width: 1250, height: 750 },
      {
        position: { x: -1920, y: 0 },
        size: { width: 1920, height: 1040 }
      }
    )
  ).toEqual({ x: -1800, y: 50 });
});

it('computes the 125% minimum of the Lenovo (2560×1600 at 200%, 1280×752 work area)', () => {
  const lenovo = { scaleFactor: 2, workArea: { size: { width: 2560, height: 1504 } } };
  const frame = { width: 26, height: 26 };
  const minimum = scaledMinimum(1.25, lenovo, frame);
  expect(minimum).toEqual({ width: 1250, height: 739 });
  // Outer size in physical pixels stays inside the work area.
  expect(minimum.width * 2 + frame.width).toBe(2526);
  expect(minimum.height * 2 + frame.height).toBe(1504);
  expect(scaledMinimum(1, lenovo, frame)).toEqual({ width: 1000, height: 600 });
  expect(scaledMinimum(1.12, lenovo, frame)).toEqual({ width: 1120, height: 672 });
});

it('never changes the minimum of a maximized window; the restore applies it', async () => {
  // A minimum change re-applies the inner size, which un-maximizes (tao).
  windowState.maximized = true;
  currentMonitor.mockResolvedValue({
    scaleFactor: 2,
    workArea: { position: { x: 0, y: 0 }, size: { width: 2560, height: 1504 } }
  });
  renderHook(() => useScaledMinimum(1.25, 'windows'));
  await act(async () => {
    moved();
    resized();
    scaleChanged();
    await new Promise((resolve) => setTimeout(resolve, 200));
  });
  expect(setMinSize).not.toHaveBeenCalled();
  windowState.maximized = false;
  await act(async () => resized());
  await waitFor(() =>
    expect(setMinSize).toHaveBeenCalledExactlyOnceWith(expect.objectContaining({ width: 1250, height: 744 }))
  );
});

it('tries again after the restore when a maximize came between the check and the change', async () => {
  // The start restores a maximized window while the first minimum is on its way.
  setMinSize.mockImplementationOnce(async () => {
    windowState.maximized = true;
  });
  renderHook(() => useScaledMinimum(1.25, 'windows'));
  await waitFor(() => expect(setMinSize).toHaveBeenCalledTimes(1));
  windowState.maximized = false;
  await act(async () => resized());
  await waitFor(() => expect(setMinSize).toHaveBeenCalledTimes(2));
  expect(setMinSize).toHaveBeenLastCalledWith(expect.objectContaining({ width: 1250, height: 750 }));
});
