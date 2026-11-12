import { act, cleanup, renderHook, waitFor } from '@testing-library/react';
import { afterEach, beforeEach, expect, it, vi } from 'vitest';
const { setMinSize, currentMonitor, moved, scaleChanged } = vi.hoisted(() => ({
  setMinSize: vi.fn().mockResolvedValue(undefined),
  currentMonitor: vi.fn(),
  moved: vi.fn(),
  scaleChanged: vi.fn()
}));
vi.mock('@tauri-apps/api/core', () => ({ isTauri: () => true }));
vi.mock('@tauri-apps/api/window', () => ({
  currentMonitor,
  getCurrentWindow: () => ({
    setMinSize,
    onMoved: async (callback: () => void) => {
      moved.mockImplementation(callback);
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
import { useScaledMinimum } from './useScaledMinimum';
afterEach(cleanup);
beforeEach(() => {
  setMinSize.mockClear();
  currentMonitor.mockResolvedValue({ scaleFactor: 1, workArea: { size: { width: 1920, height: 1080 } } });
});
it.each([1, 1.12, 1.25])('scales the usable window floor at %s', async (scale) => {
  renderHook(() => useScaledMinimum(scale, 'windows'));
  await waitFor(() =>
    expect(setMinSize).toHaveBeenCalledExactlyOnceWith(
      expect.objectContaining({ width: 1000 * scale, height: 600 * scale })
    )
  );
});
it('caps the floor at the monitor work area minus the frame', async () => {
  currentMonitor.mockResolvedValue({ scaleFactor: 1.5, workArea: { size: { width: 1920, height: 1041 } } });
  renderHook(() => useScaledMinimum(1.25, 'windows'));
  await waitFor(() =>
    expect(setMinSize).toHaveBeenCalledExactlyOnceWith(
      expect.objectContaining({ width: 1250, height: (1041 - 16) / 1.5 })
    )
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
  currentMonitor.mockResolvedValue({ scaleFactor: 1.5, workArea: { size: { width: 1920, height: 1041 } } });
  await act(async () => moved());
  await waitFor(() =>
    expect(setMinSize).toHaveBeenLastCalledWith(expect.objectContaining({ height: (1041 - 16) / 1.5 }))
  );
  currentMonitor.mockResolvedValue({ scaleFactor: 1, workArea: { size: { width: 1920, height: 1080 } } });
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
