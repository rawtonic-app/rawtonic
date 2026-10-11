import { describe, expect, it } from 'vitest';
import { aiDenoiseToApply } from './aiDenoise';

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
