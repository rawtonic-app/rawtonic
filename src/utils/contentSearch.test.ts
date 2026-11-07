import { beforeEach, expect, it } from 'vitest';
import { downloadSize, readSearchMode, sortByMatch, writeSearchMode } from './contentSearch';

beforeEach(() => localStorage.clear());

it('remembers the search mode per library, Text by default', () => {
  expect(readSearchMode('/A')).toBe('text');
  writeSearchMode('/A', 'content');
  expect(readSearchMode('/A')).toBe('content');
  expect(readSearchMode('/B')).toBe('text');
  expect(readSearchMode(null)).toBe('text');
  localStorage.setItem('rawtonic.search.mode:/B', 'nonsense');
  expect(readSearchMode('/B')).toBe('text');
});

it('sorts ranked items first by place and keeps the order of the rest', () => {
  const ranks = new Map([
    ['c', 0],
    ['a', 1]
  ]);
  expect(sortByMatch(['a', 'b', 'c', 'd'], ranks, (x) => x)).toEqual(['c', 'a', 'b', 'd']);
  expect(sortByMatch(['d', 'b'], ranks, (x) => x)).toEqual(['d', 'b']);
});

it('shows download sizes in decimal gigabytes', () => {
  expect(downloadSize(1_535_588_149, 'en')).toBe('1.5 GB');
  expect(downloadSize(1_163_804_427, 'en')).toBe('1.2 GB');
  expect(downloadSize(1_163_804_427, 'de')).toBe('1,2 GB');
  expect(downloadSize(34_363_136, 'en')).toBe('34 MB');
});
