import { expect, it } from 'vitest';
import { hasOwnWork, type DuplicateMember } from './duplicates';

const member = (path: string, part: Partial<DuplicateMember> = {}): DuplicateMember => ({
  id: path,
  path,
  size: 10,
  captured: null,
  imported: null,
  rating: 0,
  flag: 0,
  label: '',
  keywords: 0,
  albums: 0,
  copies: 0,
  described: false,
  companion: null,
  compared: 'tiff',
  owner: null,
  ownFolder: true,
  edited: false,
  suggested: false,
  ...part,
});

it('counts edits, culling, labels, keywords, albums and virtual copies as work of its own', () => {
  expect(hasOwnWork(member('a'))).toBe(false);
  for (const part of [
    { edited: true },
    { rating: 1 },
    { flag: -1 },
    { label: 'red' },
    { keywords: 2 },
    { albums: 1 },
    { copies: 1 },
    { described: true },
    { companion: '/a.JPG' },
  ])
    expect(hasOwnWork(member('a', part))).toBe(true);
});
