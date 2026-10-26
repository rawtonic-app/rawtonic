import { describe, expect, it } from 'vitest';
import { frameOf, frameUpdateAvailable, requestedSourceFrame, sourceFrameToOpen, withSourceFrameOf } from './sourceFrame';
import { SOURCE_FRAME_VERSION } from './adjustments';

/** An EOS R5 Mark II APS-C file: version 1 frames it anew. */
const r5Crop = [0, 1];
/** Every other file: all versions frame it alike. */
const other = [0, 0];
const decoded = (sourceFrame: number, sourceFrames: number[]) => ({
  isDecoded: true, isRaw: true, offline: null, sourceFrame, sourceFrames,
});

describe('the frame of the RAW a recipe develops', () => {
  it('reads the recipe key: missing is the first frame, unknown versions the newest', () => {
    expect(requestedSourceFrame({})).toBe(0);
    expect(requestedSourceFrame({ sourceFrameVersion: 1 })).toBe(1);
    expect(requestedSourceFrame({ sourceFrameVersion: 99 })).toBe(SOURCE_FRAME_VERSION);
    expect(frameOf(r5Crop, 1)).toBe(1);
    expect(frameOf(other, 1)).toBe(0);
    expect(frameOf(undefined, 1)).toBe(1);
    expect(frameOf(r5Crop, 7)).toBe(1);
  });

  it('opens the photograph again when the recipe develops another frame than its source shows', () => {
    // Update frame, or redo across it.
    expect(sourceFrameToOpen(decoded(0, r5Crop), { sourceFrameVersion: 1 }, false)).toBe(1);
    // Undo across it.
    expect(sourceFrameToOpen(decoded(1, r5Crop), {}, false)).toBe(0);
    expect(sourceFrameToOpen(decoded(1, r5Crop), { sourceFrameVersion: 1 }, false)).toBeNull();
    // Files every version frames alike: nothing to open again.
    expect(sourceFrameToOpen(decoded(0, other), { sourceFrameVersion: 1 }, false)).toBeNull();
    // Not during a drag, before the decode, offline or while it opens again.
    expect(sourceFrameToOpen(decoded(0, r5Crop), { sourceFrameVersion: 1 }, true)).toBeNull();
    expect(sourceFrameToOpen({ ...decoded(0, r5Crop), isDecoded: false }, { sourceFrameVersion: 1 }, false)).toBeNull();
    expect(sourceFrameToOpen({ ...decoded(0, r5Crop), offline: 'full' }, { sourceFrameVersion: 1 }, false)).toBeNull();
    expect(sourceFrameToOpen({ ...decoded(0, r5Crop), reopenFrame: 1 }, { sourceFrameVersion: 1 }, false)).toBeNull();
  });

  it('offers Update frame only where the newest frame is another than the recipe’s', () => {
    expect(frameUpdateAvailable(decoded(0, r5Crop), {})).toBe(true);
    expect(frameUpdateAvailable(decoded(1, r5Crop), { sourceFrameVersion: 1 })).toBe(false);
    expect(frameUpdateAvailable(decoded(0, other), {})).toBe(false);
    expect(frameUpdateAvailable({ ...decoded(0, r5Crop), offline: 'standard' }, {})).toBe(false);
    expect(frameUpdateAvailable({ ...decoded(0, r5Crop), sourceFrames: undefined }, {})).toBe(false);
  });

  it('makes a new edit on previews of an earlier frame in that frame', () => {
    const fresh = { exposure: 0, sourceFrameVersion: SOURCE_FRAME_VERSION };
    expect(withSourceFrameOf(fresh, 0, r5Crop)).toEqual({ exposure: 0, sourceFrameVersion: 0 });
    expect(withSourceFrameOf(fresh, 1, r5Crop)).toBe(fresh);
    expect(withSourceFrameOf(fresh, 0, other)).toBe(fresh);
    expect(withSourceFrameOf(fresh, null, r5Crop)).toBe(fresh);
  });
});
