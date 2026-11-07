import { describe, expect, it } from 'vitest';
import { sharpenViewFor, sharpenViewWhile } from './sharpenView';

describe('sharpening view', () => {
  it('shows the mask for Masking and the photo in grey for Amount, Radius and Detail', () => {
    expect(sharpenViewFor('sharpenMasking')).toBe('mask');
    for (const key of ['sharpness', 'sharpenRadius', 'sharpenDetail']) expect(sharpenViewFor(key)).toBe('grey');
    for (const key of ['sharpnessThreshold', 'sharpnessFalloff', 'exposure']) expect(sharpenViewFor(key)).toBeNull();
  });
  it('shows only while the slider is dragged with Option held', () => {
    expect(sharpenViewWhile('sharpenMasking', true, true)).toBe('mask');
    expect(sharpenViewWhile('sharpenMasking', true, false)).toBeNull();
    expect(sharpenViewWhile('sharpenMasking', false, true)).toBeNull();
    expect(sharpenViewWhile('exposure', true, true)).toBeNull();
  });
});
