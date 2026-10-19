import { describe, expect, it } from 'vitest';
import { renderHook } from '@testing-library/react';
import { cropEditCancelled, cropGestures, useCropGestures, useToolGesture } from './cropGesture';

describe('crop gestures', () => {
  it('ends a drag of a component that unmounts during it, once', () => {
    const before = cropGestures();
    const { result, unmount } = renderHook(useCropGestures);
    result.current.start();
    expect(cropGestures().active).toBe(true);
    unmount();
    expect(cropGestures()).toEqual({ started: before.started + 1, active: false });
    // A late end after unmount changes nothing.
    result.current.end();
    expect(cropGestures().active).toBe(false);
  });

  it('ends the open drags when the crop tool closes mid-drag (endAll)', () => {
    const { result } = renderHook(useCropGestures);
    result.current.start();
    result.current.start();
    expect(cropGestures().active).toBe(true);
    result.current.endAll();
    expect(cropGestures().active).toBe(false);
  });

  it("ends a straighten line's gesture when its tool is switched off before release (Escape)", () => {
    const { result, rerender } = renderHook(({ active }) => useToolGesture(active), { initialProps: { active: true } });
    result.current.start();
    expect(cropGestures().active).toBe(true);
    rerender({ active: false });
    expect(cropGestures().active).toBe(false);
  });

  it('counts cancelling the crop edit as a gesture', () => {
    const before = cropGestures().started;
    cropEditCancelled();
    expect(cropGestures()).toEqual({ started: before + 1, active: false });
  });
});
