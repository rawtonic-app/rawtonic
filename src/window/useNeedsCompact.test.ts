import { expect, it } from 'vitest';
import { needsCompact } from './useNeedsCompact';

it('depends only on the two widths, in both directions', () => {
  expect(needsCompact(360, 320)).toBe(true);
  expect(needsCompact(360, 200)).toBe(true);
  expect(needsCompact(360, 376)).toBe(false);
  expect(needsCompact(360, 2560)).toBe(false);
  expect(needsCompact(360, 320)).toBe(true);
});
it('does nothing before anything is measured', () => {
  expect(needsCompact(0, 320)).toBe(false);
  expect(needsCompact(360, 0)).toBe(false);
});
