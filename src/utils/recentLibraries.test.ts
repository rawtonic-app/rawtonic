import { expect, it } from 'vitest';
import { withMovedLibrary, withRecentLibrary } from './recentLibraries';
it('remembers up to ten unique libraries in most recent order', () => {
  const before = Array.from({ length: 10 }, (_, index) => `/library/${index}`);
  expect(withRecentLibrary(before, before[4])).toEqual([before[4], ...before.filter((p) => p !== before[4])]);
  expect(withRecentLibrary(before, '/new')).toEqual(['/new', ...before.slice(0, 9)]);
  expect(withRecentLibrary(undefined, '/first')).toEqual(['/first']);
});

it('keeps a moved library at its place in the list under its new folder', () => {
  expect(withMovedLibrary(['/a', '/Jobs/S7', '/b'], '/Jobs/S7', '/Archive/S7')).toEqual(['/a', '/Archive/S7', '/b']);
  expect(withMovedLibrary(['/Archive/S7', '/Jobs/S7'], '/Jobs/S7', '/Archive/S7')).toEqual(['/Archive/S7']);
  expect(withMovedLibrary(['/a'], '/Jobs/S7', '/Archive/S7')).toEqual(['/a']);
  expect(withMovedLibrary(undefined, '/Jobs/S7', '/Archive/S7')).toBeUndefined();
});
