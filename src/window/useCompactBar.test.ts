import { expect, it } from 'vitest';
import { nextCompact } from './useCompactBar';

it('compacts the top bar when its content overflows and widens back only when it fits again', () => {
  const wide = { compact: false, needed: 0 };
  expect(nextCompact(wide, { clientWidth: 1200, scrollWidth: 1200 })).toBe(wide);
  // 125 % on a 1280 px logical screen: 1024 bar pixels for 1090 of content.
  const compact = nextCompact(wide, { clientWidth: 1024, scrollWidth: 1090 });
  expect(compact).toEqual({ compact: true, needed: 1090 });
  // Compacted content fits; a slightly wider window keeps it compact.
  expect(nextCompact(compact, { clientWidth: 1024, scrollWidth: 1024 })).toBe(compact);
  expect(nextCompact(compact, { clientWidth: 1080, scrollWidth: 1080 })).toBe(compact);
  expect(nextCompact(compact, { clientWidth: 1100, scrollWidth: 1100 })).toEqual({ compact: false, needed: 0 });
});
