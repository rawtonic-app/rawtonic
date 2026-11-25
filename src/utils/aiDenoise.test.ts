import { describe, expect, it } from 'vitest';
import { PasteMode, pastedAdjustments, type Adjustments } from './adjustments';
import {
  aiDenoiseAmount,
  aiDenoiseToApply,
  developmentSignature,
  withAiDenoiseQualityOnTurnOn,
  withXTransAiDenoiseVersion,
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

  it('develops again when the AI Denoise quality changes with AI Denoise on', () => {
    const full = { ...decoded, aiDenoise: 60, aiDenoiseVersion: 5 };
    const fast = { aiDenoise: 60, aiDenoiseVersion: 5, aiDenoiseQuality: 'fast' as const };
    expect(aiDenoiseToApply(full, fast, false)).toBe(60);
    // A recipe without the key renders Full: the Full source is its own.
    expect(aiDenoiseToApply(full, { aiDenoise: 60, aiDenoiseVersion: 5 }, false)).toBeNull();
    expect(aiDenoiseToApply({ ...full, aiDenoiseQuality: 'fast' }, fast, false)).toBeNull();
    expect(aiDenoiseToApply({ ...full, aiDenoiseQuality: 'fast' }, { ...fast, aiDenoiseQuality: 'full' }, false)).toBe(
      60
    );
    // Off: the quality does not matter.
    expect(aiDenoiseToApply(decoded, { ...fast, aiDenoise: 0 }, false)).toBeNull();
    expect(sourceDevelopToApply({ ...full, colorNoiseMultiscale: true }, fast, false)).toEqual({
      aiDenoise: 60,
      aiDenoiseVersion: 5,
      aiDenoiseQuality: 'fast',
      multiscale: true
    });
    // A quality that failed is not retried; the other one is.
    const failed = {
      ...full,
      aiDenoise: 0,
      aiDenoiseFailed: 60,
      aiDenoiseFailedVersion: 5,
      aiDenoiseFailedQuality: 'fast' as const
    };
    expect(aiDenoiseToApply(failed, fast, false)).toBeNull();
    expect(aiDenoiseToApply(failed, { ...fast, aiDenoiseQuality: 'full' }, false)).toBe(60);
  });

  it("gives a recipe that turns AI Denoise on the computer's quality, and nothing else", () => {
    expect(withAiDenoiseQualityOnTurnOn({ aiDenoise: 0 }, { aiDenoise: 50 }, 'fast')).toEqual({
      aiDenoise: 50,
      aiDenoiseQuality: 'fast'
    });
    expect(withAiDenoiseQualityOnTurnOn({}, { aiDenoise: 50 }, 'full')).toEqual({
      aiDenoise: 50,
      aiDenoiseQuality: 'full'
    });
    // Already on (a recipe from before the key renders Full): kept as it renders.
    expect(withAiDenoiseQualityOnTurnOn({ aiDenoise: 40 }, { aiDenoise: 50 }, 'fast')).toEqual({ aiDenoise: 50 });
    // A chosen quality stays; an unknown default writes nothing.
    expect(
      withAiDenoiseQualityOnTurnOn({ aiDenoise: 0 }, { aiDenoise: 50, aiDenoiseQuality: 'full' as const }, 'fast')
    ).toEqual({ aiDenoise: 50, aiDenoiseQuality: 'full' });
    expect(withAiDenoiseQualityOnTurnOn({ aiDenoise: 0 }, { aiDenoise: 50 }, null)).toEqual({ aiDenoise: 50 });
    // A recipe from before the key turned off says Full, so turning it on
    // again renders Full, not this computer's default.
    const off = withAiDenoiseQualityOnTurnOn({ aiDenoise: 40 }, { aiDenoise: 0 }, 'fast');
    expect(off).toEqual({ aiDenoise: 0, aiDenoiseQuality: 'full' });
    expect(withAiDenoiseQualityOnTurnOn(off, { ...off, aiDenoise: 40 }, 'fast')).toEqual({
      aiDenoise: 40,
      aiDenoiseQuality: 'full'
    });
    // A change that removes the key itself (the Noise reduction reset) keeps it removed.
    const reset = { aiDenoise: 0, aiDenoiseQuality: undefined };
    expect(withAiDenoiseQualityOnTurnOn({ aiDenoise: 40 }, { ...reset }, 'fast', reset)).toEqual(reset);
    expect(withAiDenoiseQualityOnTurnOn({ aiDenoise: 0 }, { aiDenoise: 0, exposure: 1 }, 'fast')).toEqual({
      aiDenoise: 0,
      exposure: 1
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

describe('withXTransAiDenoiseVersion', () => {
  it('brings an older X-Trans edit to the X-Trans version when its AI Denoise amount is changed', () => {
    const older = { aiDenoise: 50, aiDenoiseVersion: 5 };
    expect(withXTransAiDenoiseVersion(older, { ...older, aiDenoise: 51 }, true)).toEqual({
      aiDenoise: 51,
      aiDenoiseVersion: 6
    });
    // Turned off and on again, or on from a recipe without the key.
    expect(withXTransAiDenoiseVersion(older, { ...older, aiDenoise: 0 }, true).aiDenoiseVersion).toBe(6);
    const turnedOn: { aiDenoise: number; aiDenoiseVersion?: number } = { aiDenoise: 40 };
    expect(withXTransAiDenoiseVersion({}, turnedOn, true).aiDenoiseVersion).toBe(6);
  });

  it('keeps Bayer photos, unchanged amounts, newer versions and versions the change sets', () => {
    const older = { aiDenoise: 50, aiDenoiseVersion: 5 };
    const moved = { ...older, aiDenoise: 60 };
    expect(withXTransAiDenoiseVersion(older, moved, false)).toBe(moved);
    const other = { ...older, exposure: 1 };
    expect(withXTransAiDenoiseVersion(older, other, true)).toBe(other);
    const current = { aiDenoise: 70, aiDenoiseVersion: 6 };
    expect(withXTransAiDenoiseVersion({ aiDenoise: 60, aiDenoiseVersion: 6 }, current, true)).toBe(current);
    // A reset or paste that sets the version keeps it.
    const pasted = { aiDenoise: 30, aiDenoiseVersion: 4 };
    expect(withXTransAiDenoiseVersion(older, pasted, true, { aiDenoise: 30, aiDenoiseVersion: 4 })).toBe(pasted);
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
    expect(sourceDevelopToApply({ ...oldBase, offline: 'full' }, { ...v2, aiDenoise: 40 }, false)).toBeNull();
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

  it('develops again when the texture rule for dead photosites flips with the render version', () => {
    const base = {
      ...multiscaleBase,
      deadPixelAdjacent: true,
      defectsIsolated: true,
      unbiasedBlack: true,
      highlightRatios: true
    };
    const isolationOnly = { ...base, defectsTexture: false };
    const withTexture = { ...base, defectsTexture: true };
    // Saved edits of render version 15 keep the isolation test alone.
    expect(sourceDevelopToApply(isolationOnly, { ...v2, renderVersion: 15 }, false)).toBeNull();
    expect(sourceDevelopToApply(withTexture, { ...v2, renderVersion: 16 }, false)).toBeNull();
    // Reset of an older edit (render version 16), and undo back.
    expect(sourceDevelopToApply(isolationOnly, { ...v2, renderVersion: 16 }, false)).toEqual({
      aiDenoise: 0,
      multiscale: true,
      deadPixelAdjacent: true,
      defectsIsolated: true,
      defectsTexture: true,
      unbiasedBlack: true,
      highlightRatios: true
    });
    expect(sourceDevelopToApply(withTexture, { ...v2, renderVersion: 15 }, false)).toEqual({
      aiDenoise: 0,
      multiscale: true,
      deadPixelAdjacent: true,
      defectsIsolated: true,
      defectsTexture: false,
      unbiasedBlack: true,
      highlightRatios: true
    });
    // Failed before: not retried; unknown (preview data) waits.
    expect(
      sourceDevelopToApply({ ...isolationOnly, defectsTextureFailed: true }, { ...v2, renderVersion: 16 }, false)
    ).toBeNull();
    expect(sourceDevelopToApply(base, { ...v2, renderVersion: 16 }, false)).toBeNull();
    // Recipes of either rule never share a source.
    expect(developmentSignature({ ...v2, renderVersion: 15 }, true)).not.toBe(
      developmentSignature({ ...v2, renderVersion: 16 }, true)
    );
  });

  it('develops again when the rule for dead photosites at black flips with the render version', () => {
    const base = {
      ...multiscaleBase,
      deadPixelAdjacent: true,
      defectsIsolated: true,
      defectsTexture: true,
      unbiasedBlack: true,
      highlightRatios: true
    };
    const textureOnly = { ...base, defectsBlack: false };
    const withBlack = { ...base, defectsBlack: true };
    // Saved edits of render version 16 keep the texture rule alone.
    expect(sourceDevelopToApply(textureOnly, { ...v2, renderVersion: 16 }, false)).toBeNull();
    expect(sourceDevelopToApply(withBlack, { ...v2, renderVersion: 17 }, false)).toBeNull();
    // Reset of an older edit (render version 17), and undo back.
    expect(sourceDevelopToApply(textureOnly, { ...v2, renderVersion: 17 }, false)).toEqual({
      aiDenoise: 0,
      multiscale: true,
      deadPixelAdjacent: true,
      defectsIsolated: true,
      defectsTexture: true,
      defectsBlack: true,
      unbiasedBlack: true,
      highlightRatios: true
    });
    expect(sourceDevelopToApply(withBlack, { ...v2, renderVersion: 16 }, false)).toEqual({
      aiDenoise: 0,
      multiscale: true,
      deadPixelAdjacent: true,
      defectsIsolated: true,
      defectsTexture: true,
      defectsBlack: false,
      unbiasedBlack: true,
      highlightRatios: true
    });
    // Failed before: not retried; unknown (preview data) waits.
    expect(
      sourceDevelopToApply({ ...textureOnly, defectsBlackFailed: true }, { ...v2, renderVersion: 17 }, false)
    ).toBeNull();
    expect(sourceDevelopToApply(base, { ...v2, renderVersion: 17 }, false)).toBeNull();
    // Recipes of either rule never share a source.
    expect(developmentSignature({ ...v2, renderVersion: 16 }, true)).not.toBe(
      developmentSignature({ ...v2, renderVersion: 17 }, true)
    );
  });

  it('develops again when the green colour-ratio repair flips with the render version', () => {
    const base = {
      ...multiscaleBase,
      deadPixelAdjacent: true,
      defectsIsolated: true,
      defectsTexture: true,
      defectsBlack: true,
      unbiasedBlack: true,
      highlightRatios: true
    };
    const blackOnly = { ...base, defectsCross: false };
    const withCross = { ...base, defectsCross: true };
    // Saved edits of render version 17 keep their repair.
    expect(sourceDevelopToApply(blackOnly, { ...v2, renderVersion: 17 }, false)).toBeNull();
    expect(sourceDevelopToApply(withCross, { ...v2, renderVersion: 18 }, false)).toBeNull();
    // Reset of an older edit (render version 18), and undo back.
    expect(sourceDevelopToApply(blackOnly, { ...v2, renderVersion: 18 }, false)).toEqual({
      aiDenoise: 0,
      multiscale: true,
      deadPixelAdjacent: true,
      defectsIsolated: true,
      defectsTexture: true,
      defectsBlack: true,
      defectsCross: true,
      unbiasedBlack: true,
      highlightRatios: true
    });
    expect(sourceDevelopToApply(withCross, { ...v2, renderVersion: 17 }, false)).toEqual({
      aiDenoise: 0,
      multiscale: true,
      deadPixelAdjacent: true,
      defectsIsolated: true,
      defectsTexture: true,
      defectsBlack: true,
      defectsCross: false,
      unbiasedBlack: true,
      highlightRatios: true
    });
    // Failed before: not retried; unknown (preview data) waits.
    expect(
      sourceDevelopToApply({ ...blackOnly, defectsCrossFailed: true }, { ...v2, renderVersion: 18 }, false)
    ).toBeNull();
    expect(sourceDevelopToApply(base, { ...v2, renderVersion: 18 }, false)).toBeNull();
    // Recipes of either repair never share a source.
    expect(developmentSignature({ ...v2, renderVersion: 17 }, true)).not.toBe(
      developmentSignature({ ...v2, renderVersion: 18 }, true)
    );
  });

  it('develops again when the median of three dead repairs flips with the render version', () => {
    const base = {
      ...multiscaleBase,
      deadPixelAdjacent: true,
      defectsIsolated: true,
      defectsTexture: true,
      defectsBlack: true,
      defectsCross: true,
      unbiasedBlack: true,
      highlightRatios: true
    };
    const crossOnly = { ...base, defectsMedian: false };
    const withMedian = { ...base, defectsMedian: true };
    // Saved edits of render version 18 keep their repair.
    expect(sourceDevelopToApply(crossOnly, { ...v2, renderVersion: 18 }, false)).toBeNull();
    expect(sourceDevelopToApply(withMedian, { ...v2, renderVersion: 19 }, false)).toBeNull();
    // Reset of an older edit (render version 19), and undo back.
    expect(sourceDevelopToApply(crossOnly, { ...v2, renderVersion: 19 }, false)).toEqual({
      aiDenoise: 0,
      multiscale: true,
      deadPixelAdjacent: true,
      defectsIsolated: true,
      defectsTexture: true,
      defectsBlack: true,
      defectsCross: true,
      defectsMedian: true,
      unbiasedBlack: true,
      highlightRatios: true
    });
    expect(sourceDevelopToApply(withMedian, { ...v2, renderVersion: 18 }, false)).toEqual({
      aiDenoise: 0,
      multiscale: true,
      deadPixelAdjacent: true,
      defectsIsolated: true,
      defectsTexture: true,
      defectsBlack: true,
      defectsCross: true,
      defectsMedian: false,
      unbiasedBlack: true,
      highlightRatios: true
    });
    // Failed before: not retried; unknown (preview data) waits.
    expect(
      sourceDevelopToApply({ ...crossOnly, defectsMedianFailed: true }, { ...v2, renderVersion: 19 }, false)
    ).toBeNull();
    expect(sourceDevelopToApply(base, { ...v2, renderVersion: 19 }, false)).toBeNull();
    // Recipes of either repair never share a source.
    expect(developmentSignature({ ...v2, renderVersion: 18 }, true)).not.toBe(
      developmentSignature({ ...v2, renderVersion: 19 }, true)
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
  it('tells Fast apart from Full with AI Denoise on, and Full from a recipe without the key not', () => {
    const at = (extra: object) => developmentSignature({ aiDenoise: 50, aiDenoiseVersion: 5, ...extra }, true);
    expect(at({ aiDenoiseQuality: 'fast' })).not.toBe(at({ aiDenoiseQuality: 'full' }));
    expect(at({ aiDenoiseQuality: 'full' })).toBe(at({}));
    const off = (q: 'fast' | 'full') =>
      developmentSignature({ aiDenoise: 0, aiDenoiseVersion: 5, aiDenoiseQuality: q }, true);
    expect(off('fast')).toBe(off('full'));
  });

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
    const preview = { isRaw: true, isDecoded: false, path: '/p/a.DNG' };
    expect(warpPlanesMatter(preview)).toBe(true);
    // An offline photograph never decodes: the same.
    expect(warpPlanesMatter({ ...preview, isDecoded: true, offline: {} as never })).toBe(true);
    // A virtual copy of an offline DNG is its source's file.
    expect(warpPlanesMatter({ ...preview, isDecoded: true, offline: {} as never, path: '/p/a.DNG?vc=abc123' })).toBe(
      true
    );
    // Formats that cannot carry the lens opcodes are compared at once.
    expect(warpPlanesMatter({ ...preview, path: '/p/a.NEF' })).toBe(false);
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
