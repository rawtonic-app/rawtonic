import { expect, it } from 'vitest';
import { withRecentLibrary } from './recentLibraries';
it('remembers up to ten unique libraries in most recent order', () => {
  const before = Array.from({ length: 10 }, (_, index) => `/library/${index}`);
  expect(withRecentLibrary(before, before[4])).toEqual([before[4], ...before.filter((p) => p !== before[4])]);
  expect(withRecentLibrary(before, '/new')).toEqual(['/new', ...before.slice(0, 9)]);
  expect(withRecentLibrary(undefined, '/first')).toEqual(['/first']);
});
