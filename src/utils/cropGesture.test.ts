import { describe, expect, it } from 'vitest';
import { renderHook } from '@testing-library/react';
import { cropEditCancelled, cropGestures, useCropGestures } from './cropGesture';

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

  it('counts cancelling the crop edit as a gesture', () => {
    const before = cropGestures().started;
    cropEditCancelled();
    expect(cropGestures()).toEqual({ started: before + 1, active: false });
  });
});
