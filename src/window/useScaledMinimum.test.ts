import { renderHook } from '@testing-library/react';
import { expect, it, vi } from 'vitest';
const { setMinSize } = vi.hoisted(() => ({ setMinSize: vi.fn().mockResolvedValue(undefined) }));
vi.mock('@tauri-apps/api/core', () => ({ isTauri: () => true }));
vi.mock('@tauri-apps/api/window', () => ({ getCurrentWindow: () => ({ setMinSize }) }));
import { useScaledMinimum } from './useScaledMinimum';

it.each([1, 1.12, 1.25])('scales the usable window floor at %s', (scale) => {
  renderHook(() => useScaledMinimum(scale, 'windows'));
  expect(setMinSize).toHaveBeenLastCalledWith(expect.objectContaining({ width: 1000 * scale, height: 600 * scale }));
});
it('updates the floor when Preferences changes the interface size', () => {
  const { rerender } = renderHook(({ scale }) => useScaledMinimum(scale, 'windows'), { initialProps: { scale: 1.25 } });
  rerender({ scale: 1 });
  expect(setMinSize).toHaveBeenLastCalledWith(expect.objectContaining({ width: 1000, height: 600 }));
});
