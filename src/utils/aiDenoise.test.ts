import { describe, expect, it } from 'vitest';
import { aiDenoiseToApply, sourceDevelopToApply } from './aiDenoise';

const decoded = { isDecoded: true, isRaw: true, aiDenoise: 0 };

describe('aiDenoiseToApply', () => {
  it('develops again when the recipe amount differs from the source', () => {
    expect(aiDenoiseToApply(decoded, { aiDenoise: 60 }, false)).toBe(60);
    expect(aiDenoiseToApply({ ...decoded, aiDenoise: 60 }, { aiDenoise: 0 }, false)).toBe(0);
  });

  it('does nothing when the source already has it, during a drag, or before the decode', () => {
    expect(aiDenoiseToApply({ ...decoded, aiDenoise: 60 }, { aiDenoise: 60.4 }, false)).toBeNull();
    expect(aiDenoiseToApply(decoded, { aiDenoise: 60 }, true)).toBeNull();
    expect(aiDenoiseToApply({ ...decoded, isDecoded: false }, { aiDenoise: 60 }, false)).toBeNull();
    expect(aiDenoiseToApply(decoded, {}, false)).toBeNull();
  });

  it('treats a switched-off Noise reduction tool as no AI Denoise', () => {
    const recipe = { aiDenoise: 60, sectionVisibility: { noiseReduction: false } };
    expect(aiDenoiseToApply(decoded, recipe, false)).toBeNull();
    expect(aiDenoiseToApply({ ...decoded, aiDenoise: 60 }, recipe, false)).toBe(0);
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
    // Versions 2 and 3 share the base image: no new development.
    expect(sourceDevelopToApply(multiscaleBase, { ...v2, colorNoiseVersion: 3 }, false)).toBeNull();
    expect(sourceDevelopToApply(oldBase, v1, false)).toBeNull();
    expect(sourceDevelopToApply(oldBase, { ...v1, colorNoiseVersion: 1 }, false)).toBeNull();
  });

  it('develops again when the colour noise filter flips (reset, paste, preset)', () => {
    expect(sourceDevelopToApply(oldBase, v2, false)).toEqual({ aiDenoise: 0, multiscale: true });
    expect(sourceDevelopToApply(multiscaleBase, v1, false)).toEqual({ aiDenoise: 0, multiscale: false });
    // Keeps the source's AI Denoise amount.
    expect(sourceDevelopToApply({ ...oldBase, aiDenoise: 40 }, { ...v2, aiDenoise: 40 }, false)).toEqual({
      aiDenoise: 40,
      multiscale: true,
    });
  });

  it('develops AI Denoise for the source filter and both at once', () => {
    expect(sourceDevelopToApply(multiscaleBase, { ...v2, aiDenoise: 60 }, false)).toEqual({
      aiDenoise: 60,
      multiscale: true,
    });
    expect(sourceDevelopToApply(oldBase, { ...v2, aiDenoise: 60 }, false)).toEqual({
      aiDenoise: 60,
      multiscale: true,
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
      multiscale: undefined,
    });
  });
});
