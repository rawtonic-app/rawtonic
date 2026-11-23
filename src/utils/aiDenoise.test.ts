import { describe, expect, it } from 'vitest';
import { PasteMode, pastedAdjustments, type Adjustments } from './adjustments';
import {
  aiDenoiseAmount,
  aiDenoiseToApply,
  developmentSignature,
  sourceDevelopToApply,
  storedSliderValue,
  warpPlanesMatter
} from './aiDenoise';

const decoded = { isDecoded: true, isRaw: true, aiDenoise: 0 };

describe('aiDenoiseToApply', () => {
  it('develops again when the recipe amount differs from the source', () => {
    expect(aiDenoiseToApply(decoded, { aiDenoise: 60 }, false)).toBe(60);
    expect(aiDenoiseToApply({ ...decoded, aiDenoise: 60 }, { aiDenoise: 0 }, false)).toBe(0);
  });

  it('does nothing when the source already has it, during a drag, or before the decode', () => {
    expect(aiDenoiseToApply({ ...decoded, aiDenoise: 60 }, { aiDenoise: 60 }, false)).toBeNull();
    expect(aiDenoiseToApply(decoded, { aiDenoise: 60 }, true)).toBeNull();
    expect(aiDenoiseToApply({ ...decoded, isDecoded: false }, { aiDenoise: 60 }, false)).toBeNull();
    expect(aiDenoiseToApply(decoded, {}, false)).toBeNull();
  });

  it('treats a switched-off Noise reduction tool as no AI Denoise', () => {
    const recipe = { aiDenoise: 60, sectionVisibility: { noiseReduction: false } };
    expect(aiDenoiseToApply(decoded, recipe, false)).toBeNull();
    expect(aiDenoiseToApply({ ...decoded, aiDenoise: 60 }, recipe, false)).toBe(0);
  });

  it('develops again when the AI Denoise version changes with AI Denoise on', () => {
    const v1Source = { ...decoded, aiDenoise: 60 };
    expect(aiDenoiseToApply(v1Source, { aiDenoise: 60, aiDenoiseVersion: 2 }, false)).toBe(60);
    expect(aiDenoiseToApply(v1Source, { aiDenoise: 60, aiDenoiseVersion: 1 }, false)).toBeNull();
    expect(
      aiDenoiseToApply({ ...v1Source, aiDenoiseVersion: 2 }, { aiDenoise: 60, aiDenoiseVersion: 2 }, false)
    ).toBeNull();
    // Off: the version does not matter.
    expect(aiDenoiseToApply(decoded, { aiDenoise: 0, aiDenoiseVersion: 2 }, false)).toBeNull();
    expect(
      sourceDevelopToApply({ ...v1Source, colorNoiseMultiscale: true }, { aiDenoise: 60, aiDenoiseVersion: 2 }, false)
    ).toEqual({
      aiDenoise: 60,
      aiDenoiseVersion: 2,
      multiscale: true
    });
  });

  it('retries a failed amount with another version', () => {
    const failed = { ...decoded, aiDenoiseFailed: 60, aiDenoiseFailedVersion: 3 };
    expect(aiDenoiseToApply(failed, { aiDenoise: 60, aiDenoiseVersion: 3 }, false)).toBeNull();
    expect(aiDenoiseToApply(failed, { aiDenoise: 60, aiDenoiseVersion: 2 }, false)).toBe(60);
  });

  it('skips non-RAW photos and an amount that failed before', () => {
    expect(aiDenoiseToApply({ ...decoded, isRaw: false }, { aiDenoise: 60 }, false)).toBeNull();
    expect(aiDenoiseToApply({ ...decoded, aiDenoiseFailed: 60 }, { aiDenoise: 60 }, false)).toBeNull();
    expect(aiDenoiseToApply({ ...decoded, aiDenoiseFailed: 60 }, { aiDenoise: 70 }, false)).toBe(70);
  });
});

describe('sourceDevelopToApply', () => {
  const v2 = { aiDenoise: 0, colorNoiseVersion: 2, colorNoiseReduction: 25 };
  const v1 = { aiDenoise: 0, colorNoiseReduction: 25 };
  const multiscaleBase = { ...decoded, colorNoiseMultiscale: true };
  const oldBase = { ...decoded, colorNoiseMultiscale: false };

  it('does nothing while the source matches the recipe', () => {
    expect(sourceDevelopToApply(multiscaleBase, v2, false)).toBeNull();
    // Versions 2 to 11 share the base image: no new development.
    expect(sourceDevelopToApply(multiscaleBase, { ...v2, colorNoiseVersion: 3 }, false)).toBeNull();
    expect(sourceDevelopToApply(multiscaleBase, { ...v2, colorNoiseVersion: 4 }, false)).toBeNull();
    expect(sourceDevelopToApply(multiscaleBase, { ...v2, colorNoiseVersion: 5 }, false)).toBeNull();
    expect(sourceDevelopToApply(multiscaleBase, { ...v2, colorNoiseVersion: 6 }, false)).toBeNull();
    expect(sourceDevelopToApply(multiscaleBase, { ...v2, colorNoiseVersion: 7 }, false)).toBeNull();
    expect(sourceDevelopToApply(multiscaleBase, { ...v2, colorNoiseVersion: 8 }, false)).toBeNull();
    expect(sourceDevelopToApply(multiscaleBase, { ...v2, colorNoiseVersion: 9 }, false)).toBeNull();
    expect(sourceDevelopToApply(multiscaleBase, { ...v2, colorNoiseVersion: 10 }, false)).toBeNull();
    expect(sourceDevelopToApply(multiscaleBase, { ...v2, colorNoiseVersion: 11 }, false)).toBeNull();
    expect(sourceDevelopToApply(oldBase, v1, false)).toBeNull();
    expect(sourceDevelopToApply(oldBase, { ...v1, colorNoiseVersion: 1 }, false)).toBeNull();
  });

  it('develops again when the colour noise filter flips (reset, paste, preset)', () => {
    expect(sourceDevelopToApply(oldBase, v2, false)).toEqual({ aiDenoise: 0, multiscale: true });
    expect(sourceDevelopToApply(multiscaleBase, v1, false)).toEqual({ aiDenoise: 0, multiscale: false });
    // Keeps the source's AI Denoise amount.
    expect(sourceDevelopToApply({ ...oldBase, aiDenoise: 40 }, { ...v2, aiDenoise: 40 }, false)).toEqual({
      aiDenoise: 40,
      multiscale: true
    });
  });

  it('develops AI Denoise for the source filter and both at once', () => {
    expect(sourceDevelopToApply(multiscaleBase, { ...v2, aiDenoise: 60 }, false)).toEqual({
      aiDenoise: 60,
      multiscale: true
    });
    expect(sourceDevelopToApply(oldBase, { ...v2, aiDenoise: 60 }, false)).toEqual({
      aiDenoise: 60,
      multiscale: true
    });
  });

  it('waits for the decode and the end of a drag, skips non-RAWs, offline and failed filters', () => {
    expect(sourceDevelopToApply(oldBase, v2, true)).toBeNull();
    expect(sourceDevelopToApply({ ...oldBase, isDecoded: false }, v2, false)).toBeNull();
    expect(sourceDevelopToApply({ ...oldBase, isRaw: false }, v2, false)).toBeNull();
    expect(sourceDevelopToApply({ ...oldBase, offline: 'full' }, v2, false)).toBeNull();
    expect(sourceDevelopToApply({ ...oldBase, colorNoiseMultiscaleFailed: true }, v2, false)).toBeNull();
    // Unknown source filter: AI Denoise alone, the backend takes the stored recipe's filter.
    expect(sourceDevelopToApply(decoded, v2, false)).toBeNull();
    expect(sourceDevelopToApply(decoded, { ...v2, aiDenoise: 60 }, false)).toEqual({
      aiDenoise: 60,
      multiscale: undefined
    });
  });

  it('develops again when the dead-photosite test flips with the render version', () => {
    const firstTest = { ...multiscaleBase, deadPixelAdjacent: false };
    const secondTest = { ...multiscaleBase, deadPixelAdjacent: true };
    // Saved edits before render version 3 keep the first test.
    expect(sourceDevelopToApply(firstTest, { ...v2, renderVersion: 2 }, false)).toBeNull();
    expect(sourceDevelopToApply(firstTest, v2, false)).toBeNull();
    expect(sourceDevelopToApply(secondTest, { ...v2, renderVersion: 3 }, false)).toBeNull();
    // Reset of an older edit (render version 3), and undo back.
    expect(sourceDevelopToApply(firstTest, { ...v2, renderVersion: 3 }, false)).toEqual({
      aiDenoise: 0,
      multiscale: true,
      deadPixelAdjacent: true
    });
    expect(sourceDevelopToApply(secondTest, { ...v2, renderVersion: 2 }, false)).toEqual({
      aiDenoise: 0,
      multiscale: true,
      deadPixelAdjacent: false
    });
    // A test that failed before is not retried; unknown (preview data) waits.
    expect(
      sourceDevelopToApply({ ...firstTest, deadPixelAdjacentFailed: true }, { ...v2, renderVersion: 3 }, false)
    ).toBeNull();
    expect(sourceDevelopToApply(multiscaleBase, { ...v2, renderVersion: 3 }, false)).toBeNull();
  });

  it('develops again when the isolation test for hot and dead photosites flips with the render version', () => {
    const base = { ...multiscaleBase, deadPixelAdjacent: true, unbiasedBlack: true, highlightRatios: true };
    const ratioTest = { ...base, defectsIsolated: false };
    const isolationTest = { ...base, defectsIsolated: true };
    // Saved edits before render version 15 keep the ratio tests.
    expect(sourceDevelopToApply(ratioTest, { ...v2, renderVersion: 14 }, false)).toBeNull();
    expect(sourceDevelopToApply(isolationTest, { ...v2, renderVersion: 15 }, false)).toBeNull();
    // Reset of an older edit (render version 15), and undo back.
    expect(sourceDevelopToApply(ratioTest, { ...v2, renderVersion: 15 }, false)).toEqual({
      aiDenoise: 0,
      multiscale: true,
      deadPixelAdjacent: true,
      defectsIsolated: true,
      unbiasedBlack: true,
      highlightRatios: true
    });
    expect(sourceDevelopToApply(isolationTest, { ...v2, renderVersion: 14 }, false)).toEqual({
      aiDenoise: 0,
      multiscale: true,
      deadPixelAdjacent: true,
      defectsIsolated: false,
      unbiasedBlack: true,
      highlightRatios: true
    });
    // Failed before: not retried; unknown (preview data) waits.
    expect(
      sourceDevelopToApply({ ...ratioTest, defectsIsolatedFailed: true }, { ...v2, renderVersion: 15 }, false)
    ).toBeNull();
    expect(sourceDevelopToApply(base, { ...v2, renderVersion: 15 }, false)).toBeNull();
    // Recipes of either test never share a source.
    expect(developmentSignature({ ...v2, renderVersion: 14 }, true)).not.toBe(
      developmentSignature({ ...v2, renderVersion: 15 }, true)
    );
  });

  it('develops again when the black handling flips with the render version', () => {
    const clipped = { ...multiscaleBase, deadPixelAdjacent: true, unbiasedBlack: false };
    const unbiased = { ...multiscaleBase, deadPixelAdjacent: true, unbiasedBlack: true };
    // Saved edits before render version 8 keep the clipped blacks.
    expect(sourceDevelopToApply(clipped, { ...v2, renderVersion: 7 }, false)).toBeNull();
    expect(sourceDevelopToApply(unbiased, { ...v2, renderVersion: 8 }, false)).toBeNull();
    // Reset of an older edit (render version 8), and undo back.
    expect(sourceDevelopToApply(clipped, { ...v2, renderVersion: 8 }, false)).toEqual({
      aiDenoise: 0,
      multiscale: true,
      deadPixelAdjacent: true,
      unbiasedBlack: true
    });
    expect(sourceDevelopToApply(unbiased, { ...v2, renderVersion: 7 }, false)).toEqual({
      aiDenoise: 0,
      multiscale: true,
      deadPixelAdjacent: true,
      unbiasedBlack: false
    });
    // Failed before: not retried.
    expect(
      sourceDevelopToApply({ ...clipped, unbiasedBlackFailed: true }, { ...v2, renderVersion: 8 }, false)
    ).toBeNull();
  });

  it('develops again when the highlight rebuild flips with the render version', () => {
    const base = { ...multiscaleBase, deadPixelAdjacent: true, unbiasedBlack: true };
    const offsets = { ...base, highlightRatios: false };
    const ratios = { ...base, highlightRatios: true };
    // Saved edits before render version 9 keep the offset rebuild.
    expect(sourceDevelopToApply(offsets, { ...v2, renderVersion: 8 }, false)).toBeNull();
    expect(sourceDevelopToApply(ratios, { ...v2, renderVersion: 9 }, false)).toBeNull();
    // Reset of an older edit (render version 9), and undo back.
    expect(sourceDevelopToApply(offsets, { ...v2, renderVersion: 9 }, false)).toEqual({
      aiDenoise: 0,
      multiscale: true,
      deadPixelAdjacent: true,
      unbiasedBlack: true,
      highlightRatios: true
    });
    expect(sourceDevelopToApply(ratios, { ...v2, renderVersion: 8 }, false)).toEqual({
      aiDenoise: 0,
      multiscale: true,
      deadPixelAdjacent: true,
      unbiasedBlack: true,
      highlightRatios: false
    });
    // Failed before: not retried; unknown (preview data) waits.
    expect(
      sourceDevelopToApply({ ...offsets, highlightRatiosFailed: true }, { ...v2, renderVersion: 9 }, false)
    ).toBeNull();
    expect(sourceDevelopToApply(base, { ...v2, renderVersion: 9 }, false)).toBeNull();
  });

  it("develops again when the border's black flips with the render version", () => {
    const base = { ...multiscaleBase, deadPixelAdjacent: true, unbiasedBlack: true, highlightRatios: true };
    const decoder = { ...base, borderBlack: false };
    const border = { ...base, borderBlack: true };
    // Saved edits before render version 11 keep the 8-level margin.
    expect(sourceDevelopToApply(decoder, { ...v2, renderVersion: 10 }, false)).toBeNull();
    expect(sourceDevelopToApply(border, { ...v2, renderVersion: 11 }, false)).toBeNull();
    // Reset of an older edit (render version 11), and undo back.
    expect(sourceDevelopToApply(decoder, { ...v2, renderVersion: 11 }, false)).toEqual({
      aiDenoise: 0,
      multiscale: true,
      deadPixelAdjacent: true,
      unbiasedBlack: true,
      borderBlack: true,
      highlightRatios: true
    });
    expect(sourceDevelopToApply(border, { ...v2, renderVersion: 10 }, false)).toEqual({
      aiDenoise: 0,
      multiscale: true,
      deadPixelAdjacent: true,
      unbiasedBlack: true,
      borderBlack: false,
      highlightRatios: true
    });
    // Failed before: not retried; unknown (preview data) waits.
    expect(
      sourceDevelopToApply({ ...decoder, borderBlackFailed: true }, { ...v2, renderVersion: 11 }, false)
    ).toBeNull();
    expect(sourceDevelopToApply(base, { ...v2, renderVersion: 11 }, false)).toBeNull();
  });

  it("develops a DNG again when its lens warp's planes flip with the render version", () => {
    const base = {
      ...multiscaleBase,
      deadPixelAdjacent: true,
      unbiasedBlack: true,
      borderBlack: true,
      highlightRatios: true
    };
    const output = { ...base, cameraPlaneWarp: false };
    const camera = { ...base, cameraPlaneWarp: true };
    // Saved edits before render version 14 keep the output-channel warp.
    expect(sourceDevelopToApply(output, { ...v2, renderVersion: 13 }, false)).toBeNull();
    expect(sourceDevelopToApply(camera, { ...v2, renderVersion: 14 }, false)).toBeNull();
    // Reset of an older edit (render version 14), and undo back.
    expect(sourceDevelopToApply(output, { ...v2, renderVersion: 14 }, false)).toEqual({
      aiDenoise: 0,
      multiscale: true,
      deadPixelAdjacent: true,
      unbiasedBlack: true,
      borderBlack: true,
      highlightRatios: true,
      cameraPlaneWarp: true
    });
    expect(sourceDevelopToApply(camera, { ...v2, renderVersion: 13 }, false)).toEqual({
      aiDenoise: 0,
      multiscale: true,
      deadPixelAdjacent: true,
      unbiasedBlack: true,
      borderBlack: true,
      highlightRatios: true,
      cameraPlaneWarp: false
    });
    // Failed before: not retried; other formats (unknown) never develop
    // again for it.
    expect(
      sourceDevelopToApply({ ...output, cameraPlaneWarpFailed: true }, { ...v2, renderVersion: 14 }, false)
    ).toBeNull();
    expect(sourceDevelopToApply(base, { ...v2, renderVersion: 14 }, false)).toBeNull();
  });
});

describe('developmentSignature', () => {
  const recipe = { aiDenoise: 0, aiDenoiseVersion: 1, colorNoiseVersion: 2 };
  it("tells a lens warp's planes apart only where they change the decode", () => {
    const v13 = { ...recipe, renderVersion: 13 };
    const v14 = { ...recipe, renderVersion: 14 };
    // A DNG whose warp differs per plane: a version 13 snapshot does not
    // share the version 14 edit's source...
    const affected = { cameraPlaneWarp: true };
    expect(warpPlanesMatter(affected)).toBe(true);
    expect(warpPlanesMatter({ cameraPlaneWarp: false })).toBe(true);
    expect(developmentSignature(v13, true, warpPlanesMatter(affected))).not.toBe(
      developmentSignature(v14, true, warpPlanesMatter(affected))
    );
    // Preview data of a RAW (which files are affected is not known yet):
    // they differ, until the decode reports.
    expect(warpPlanesMatter({ isRaw: true, isDecoded: false })).toBe(true);
    expect(warpPlanesMatter({ isRaw: true, isDecoded: false, offline: {} as never })).toBe(false);
    // ... while other photographs (no such warp, other formats) still share.
    expect(warpPlanesMatter({})).toBe(false);
    expect(warpPlanesMatter(null)).toBe(false);
    expect(developmentSignature(v13, true, warpPlanesMatter({}))).toBe(
      developmentSignature(v14, true, warpPlanesMatter({}))
    );
    expect(developmentSignature(v13, true)).toBe(developmentSignature(v14, true));
  });
});

describe('storedSliderValue', () => {
  it('stores AI Denoise in whole steps, from fine drags and typed values too', () => {
    expect(storedSliderValue('aiDenoise', 50.4)).toBe(50);
    expect(storedSliderValue('aiDenoise', 50.5)).toBe(51);
    expect(storedSliderValue('aiDenoise', 0.4)).toBe(0);
    expect(storedSliderValue('aiDenoise', 60)).toBe(60);
  });

  it('leaves the other sliders as they are', () => {
    expect(storedSliderValue('exposure', 0.37)).toBe(0.37);
    expect(storedSliderValue('lumaNoiseReduction', 12.4)).toBe(12.4);
  });
});

describe('old fractional AI Denoise amounts', () => {
  const recipe = { aiDenoise: 50.4, aiDenoiseVersion: 4 };

  it('develops the stored amount, as the export reads it', () => {
    expect(aiDenoiseAmount(recipe)).toBe(50.4);
    expect(aiDenoiseToApply({ ...decoded, aiDenoise: 50, aiDenoiseVersion: 4 }, recipe, false)).toBe(50.4);
    expect(sourceDevelopToApply({ ...decoded, aiDenoise: 50, aiDenoiseVersion: 4 }, recipe, false)).toMatchObject({
      aiDenoise: 50.4
    });
  });

  it('keeps a source developed at the stored amount (no endless develop, f32 round trip)', () => {
    // Tauri serializes a Rust f32 as the shortest decimal that round-trips.
    const source = { ...decoded, aiDenoise: 50.4, aiDenoiseVersion: 4 };
    expect(aiDenoiseToApply(source, recipe, false)).toBeNull();
    expect(aiDenoiseToApply({ ...source, aiDenoise: Math.fround(recipe.aiDenoise) }, recipe, false)).toBeNull();
  });

  it('tells the signature of 50 and 50.4 apart: their sources differ', () => {
    expect(developmentSignature(recipe, true)).not.toBe(developmentSignature({ ...recipe, aiDenoise: 50 }, true));
  });

  it('tells Preset Amount fractions apart even below the former tolerance', () => {
    expect(
      aiDenoiseToApply(
        { ...decoded, aiDenoise: 40, aiDenoiseVersion: 4 },
        { aiDenoise: 40.00008, aiDenoiseVersion: 4 },
        false
      )
    ).toBe(40.00008);
  });

  it('is copied as stored by a paste', () => {
    const pasted = pastedAdjustments(recipe as Adjustments, ['aiDenoise', 'aiDenoiseVersion'], PasteMode.Replace);
    expect(pasted.aiDenoise).toBe(50.4);
    expect(pasted.aiDenoiseVersion).toBe(4);
  });
});

describe('a sensor AI Denoise skipped, with Apple RAW 9 on', () => {
  // Its rawler pixels stand in for the recipe's AI Denoise: with it off, the
  // export decodes with Apple's engine, so Develop must develop again.
  const skipped = { ...decoded, aiDenoise: 0, aiDenoiseFailed: 60, aiDenoiseFailedVersion: 4, aiDenoiseStandIn: true };

  it('is developed again when AI Denoise is turned off', () => {
    expect(aiDenoiseToApply(skipped, { aiDenoise: 0, aiDenoiseVersion: 4 }, false)).toBe(0);
  });

  it('is kept while the recipe still asks for the amount that was skipped', () => {
    expect(aiDenoiseToApply(skipped, { aiDenoise: 60, aiDenoiseVersion: 4 }, false)).toBeNull();
  });

  it('is developed again for another amount, then plain once developed', () => {
    expect(aiDenoiseToApply(skipped, { aiDenoise: 70, aiDenoiseVersion: 4 }, false)).toBe(70);
    expect(
      aiDenoiseToApply({ ...skipped, aiDenoiseStandIn: false }, { aiDenoise: 0, aiDenoiseVersion: 4 }, false)
    ).toBeNull();
  });
});

describe('decode preferences changed since the source was developed', () => {
  const stale = { ...decoded, aiDenoise: 40, aiDenoiseVersion: 4, decoderStale: 1 };

  it('develops the source again at the recipe amount, once', () => {
    expect(aiDenoiseToApply(stale, { aiDenoise: 40, aiDenoiseVersion: 4 }, false)).toBe(40);
    expect(aiDenoiseToApply(stale, { aiDenoise: 0 }, false)).toBe(0);
    expect(aiDenoiseToApply({ ...stale, decoderStale: 0 }, { aiDenoise: 40, aiDenoiseVersion: 4 }, false)).toBeNull();
    expect(sourceDevelopToApply(stale, { aiDenoise: 40, aiDenoiseVersion: 4 }, false)).toMatchObject({ aiDenoise: 40 });
  });

  it('waits for a slider drag', () => {
    expect(aiDenoiseToApply(stale, { aiDenoise: 40, aiDenoiseVersion: 4 }, true)).toBeNull();
  });

  it.each(['/a.jpg', '/a.tiff'])('develops %s again without AI Denoise', (path) => {
    const raster = { ...stale, path, isRaw: false };
    const recipe = { aiDenoise: 40, aiDenoiseVersion: 4 };
    expect(sourceDevelopToApply(raster, recipe, false)).toEqual({ aiDenoise: 0 });
    expect(sourceDevelopToApply({ ...raster, decoderStale: 0 }, recipe, false)).toBeNull();
    expect(sourceDevelopToApply(raster, recipe, true)).toBeNull();
  });
});
