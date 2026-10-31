import { describe, expect, it } from 'vitest';
import type { Crop } from 'react-image-crop';
import { SOURCE_FRAME_VERSION, type Adjustments } from './adjustments';
import { cameraCropFor, cropResetValues, withCameraCrop, type CameraCrops } from './cameraCrop';
import { editorRecipe } from './neverEdited';
import { pasteInto } from './settingsClipboard';

// Samsung EX2F 16:9 on its 4000 × 3000 sensor (crates/rawdecode/src/camera_crop.rs).
const sixteenNine: Crop = { unit: 'px', x: 0, y: 376, width: 4000, height: 2248 };
const crops: CameraCrops = [sixteenNine, sixteenNine];

describe('the aspect ratio set in the camera as the crop new edits start with', () => {
  it('opens a never-edited RAW, or a Reset, cropped to the camera frame', () => {
    const fresh = editorRecipe(null, true, 4000, 3000, null, crops);
    expect(fresh.crop).toEqual(sixteenNine);
    expect(fresh.aspectRatio).toBeCloseTo(4000 / 2248);
    const reset = editorRecipe({ sourceFrameVersion: 0 }, true, 4000, 3000, null, crops);
    expect(reset.crop).toEqual(sixteenNine);
  });

  it('leaves an edit with its own crop, none included', () => {
    const edited = editorRecipe({ exposure: 1, crop: null }, true, 4000, 3000, null, crops);
    expect(edited.crop).toBeNull();
    expect(edited.aspectRatio).toBeCloseTo(4 / 3);
    const own: Crop = { unit: 'px', x: 10, y: 10, width: 100, height: 100 };
    expect(editorRecipe({ exposure: 1, crop: own }, true, 4000, 3000, null, crops).crop).toEqual(own);
    // Not a RAW, or nothing known: uncropped.
    expect(editorRecipe(null, false, 4000, 3000, null, crops).crop).toBeNull();
    expect(editorRecipe(null, true, 4000, 3000, null, null).crop).toBeNull();
  });

  it('takes the crop of the frame the recipe develops', () => {
    expect(cameraCropFor([null, sixteenNine], 1)).toEqual(sixteenNine);
    expect(cameraCropFor([null, sixteenNine], 0)).toBeNull();
    expect(cameraCropFor([], SOURCE_FRAME_VERSION)).toBeNull();
    const back = withCameraCrop<Partial<Adjustments>>({ crop: sixteenNine, sourceFrameVersion: 0 }, [null], 4000, 3000);
    expect([back.crop, back.aspectRatio]).toEqual([null, 4000 / 3000]);
  });

  it('starts a paste onto a new recipe from it; a pasted crop replaces it', () => {
    const options = { samePhoto: false, replaceLayers: true, cameraCrop: sixteenNine };
    expect(pasteInto(null, { exposure: 1 }, options)?.crop).toEqual(sixteenNine);
    const other: Crop = { unit: 'px', x: 0, y: 0, width: 300, height: 300 };
    expect(pasteInto(null, { crop: other }, options)?.crop).toEqual(other);
    // An edit keeps its own.
    expect(pasteInto({ exposure: 2 } as never, { exposure: 1 }, options)?.crop).toBeUndefined();
  });

  it('resets a crop to the camera frame, turned with the photo', () => {
    const image = { isRaw: true, cameraCrops: crops };
    expect(cropResetValues(image, 1, 0)).toEqual({ crop: sixteenNine, aspectRatio: 4000 / 2248 });
    // A quarter turn: the centred frame on the turned photo (3000 × 4000).
    expect(cropResetValues(image, 1, 1)).toEqual({
      crop: { unit: 'px', x: 376, y: 0, width: 2248, height: 4000 },
      aspectRatio: 2248 / 4000,
    });
    expect(cropResetValues(image, 1, 2).crop).toEqual(sixteenNine);
    expect(cropResetValues({ isRaw: false, cameraCrops: crops }, 1, 0)).toEqual({ crop: null, aspectRatio: null });
    expect(cropResetValues(null, 1, 0)).toEqual({ crop: null, aspectRatio: null });
  });
});
