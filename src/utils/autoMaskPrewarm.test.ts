import { describe, expect, it } from 'vitest';
import { autoMaskPrewarmPath } from './autoMaskPrewarm';

describe('autoMaskPrewarmPath', () => {
  const decoded = { path: '/s/a.ARW', isReady: true, isDecoded: true };

  it('asks for the decoded photo while the layers panel is open with Auto Mask on', () => {
    expect(autoMaskPrewarmPath(true, true, decoded)).toBe('/s/a.ARW');
  });

  it('waits for the full decode when the photo is shown from its proxy', () => {
    const proxy = { ...decoded, isDecoded: false };
    expect(autoMaskPrewarmPath(true, true, proxy)).toBeNull();
    // The decode arriving changes the result, so the editor's effect runs again.
    expect(autoMaskPrewarmPath(true, true, { ...proxy, isDecoded: true })).toBe('/s/a.ARW');
  });

  it('asks for nothing without Auto Mask, the layers panel or a photo', () => {
    expect(autoMaskPrewarmPath(true, false, decoded)).toBeNull();
    expect(autoMaskPrewarmPath(false, true, decoded)).toBeNull();
    expect(autoMaskPrewarmPath(true, true, null)).toBeNull();
    expect(autoMaskPrewarmPath(true, true, { ...decoded, isReady: false })).toBeNull();
  });
});
