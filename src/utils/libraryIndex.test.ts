import { expect, it } from 'vitest';
import type { ImageFile } from '../components/ui/AppProperties';
import { libraryImagePath } from './libraryIndex';

it('resolves aliases and exact spellings, refreshes with each list, and caches misses', () => {
  const plain = 'C:\\shoot\\a.raw';
  const canonical = `\\\\?\\${plain}`;
  const files = [{ path: plain }] as ImageFile[];
  expect(libraryImagePath(files, canonical)).toBe(plain);
  // Once indexed, neither repeated misses nor aliases read the list again.
  Object.defineProperty(files[0], 'path', {
    get: () => {
      throw new Error('rescanned');
    }
  });
  expect(libraryImagePath(files, canonical)).toBe(plain);
  expect(libraryImagePath(files, '/unknown.raw')).toBe('/unknown.raw');
  const next = [{ path: canonical }] as ImageFile[];
  expect(libraryImagePath(next, plain)).toBe(canonical);
  const both = [{ path: canonical }, { path: plain }] as ImageFile[];
  expect(libraryImagePath(both, plain)).toBe(plain);
  expect(libraryImagePath(both, canonical)).toBe(canonical);
});
