import { describe, expect, it } from 'vitest';
import { SOURCE_FRAME_VERSION, initialAdjustmentsFor, normalizeLoadedAdjustments, type Adjustments } from './adjustments';
import { editorRecipe } from './neverEdited';
import { withNewEditFrame, withSourceFrameOf } from './sourceFrame';
import { pasteInto } from './settingsClipboard';

describe('the frame of the RAW (ADR 0020, "new photos only")', () => {
  it('opens a never-edited photo with the newest frame, an edit with its own', () => {
    expect(editorRecipe(null, true, 5088, 3392, null).sourceFrameVersion).toBe(SOURCE_FRAME_VERSION);
    expect(editorRecipe({}, true, 5088, 3392, null).sourceFrameVersion).toBe(SOURCE_FRAME_VERSION);
    expect(initialAdjustmentsFor(false).sourceFrameVersion).toBe(SOURCE_FRAME_VERSION);
    // An edit from before the key develops the first frame (no key), one
    // with the key its own, a Reset's (only the key) its own.
    const legacy = { exposure: 1, lensCorrectionMode: 'camera' } as unknown as Adjustments;
    expect(normalizeLoadedAdjustments(legacy, true).sourceFrameVersion).toBeUndefined();
    expect(normalizeLoadedAdjustments({ ...legacy, sourceFrameVersion: 1 }, true).sourceFrameVersion).toBe(1);
    expect(normalizeLoadedAdjustments({ sourceFrameVersion: 0 } as Adjustments, true)).toMatchObject({
      sourceFrameVersion: 0,
      // ...and otherwise renders as a new edit.
      renderVersion: initialAdjustmentsFor(true).renderVersion,
    });
  });

  it('keeps the photo\'s frame on a recipe that replaces its edit', () => {
    const reset = { ...initialAdjustmentsFor(true) };
    expect(withSourceFrameOf(reset, { sourceFrameVersion: 0 }).sourceFrameVersion).toBe(0);
    expect('sourceFrameVersion' in withSourceFrameOf(reset, {})).toBe(false);
    expect(withSourceFrameOf<Partial<Adjustments>>({ exposure: 1 }, { sourceFrameVersion: 1 }).sourceFrameVersion).toBe(1);
  });

  it('starts a new edit on what is shown', () => {
    const fresh = initialAdjustmentsFor(true);
    expect(withNewEditFrame(fresh, null)).toBe(fresh);
    expect(withNewEditFrame(fresh, 0).sourceFrameVersion).toBe(0);
    // A paste onto a reset photo keeps its frame; onto a new one the newest.
    expect(pasteInto({ sourceFrameVersion: 0 } as Adjustments, { exposure: 1 }, { samePhoto: false, replaceLayers: true })?.sourceFrameVersion).toBe(0);
    expect(pasteInto(null, { exposure: 1 }, { samePhoto: false, replaceLayers: true })?.sourceFrameVersion).toBe(SOURCE_FRAME_VERSION);
  });
});
