import { expect, it } from 'vitest';
import { hasOwnWork, suggestedKeep, type DuplicateMember } from './duplicates';

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
  copies: 0,
  described: false,
  companion: null,
  compared: 'tiff',
  edited: false,
  ...part,
});

it('counts edits, culling, labels, keywords and virtual copies as work of its own', () => {
  expect(hasOwnWork(member('a'))).toBe(false);
  for (const part of [
    { edited: true },
    { rating: 1 },
    { flag: -1 },
    { label: 'red' },
    { keywords: 2 },
    { copies: 1 },
    { described: true },
    { companion: '/a.JPG' },
  ])
    expect(hasOwnWork(member('a', part))).toBe(true);
});

it('keeps every photo with work of its own, else the oldest import', () => {
  // The backend lists a group oldest import first.
  expect([...suggestedKeep([member('old'), member('new')])]).toEqual(['old']);
  expect([...suggestedKeep([member('old'), member('rated', { rating: 3 })])]).toEqual(['rated']);
  expect([...suggestedKeep([member('edited', { edited: true }), member('old'), member('kw', { keywords: 1 })])]).toEqual([
    'edited',
    'kw',
  ]);
});
