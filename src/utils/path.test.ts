// SPDX-License-Identifier: AGPL-3.0-or-later
import { expect, it } from 'vitest';
import { dirname, joinPath } from './path';
import { pathKey } from './pathKey';

it.each([
  ['/jobs/Acme', ['Capture', 'a.raf'], '/jobs/Acme/Capture/a.raf'],
  ['/', ['Capture', 'a.raf'], '/Capture/a.raf'],
  ['C:\\', ['Capture', 'a.raf'], 'C:\\Capture\\a.raf'],
  ['C:\\Jobs\\Acme', ['Capture', 'a.raf'], 'C:\\Jobs\\Acme\\Capture\\a.raf'],
  ['\\\\server\\share', ['Capture', 'a.raf'], '\\\\server\\share\\Capture\\a.raf'],
  ['\\\\?\\C:\\Jobs\\Acme', ['Capture'], '\\\\?\\C:\\Jobs\\Acme\\Capture'],
  ['\\\\?\\UNC\\nas\\share', ['Capture'], '\\\\?\\UNC\\nas\\share\\Capture'],
  ['/jobs/Acme///', ['//Capture/', '', '/a.raf'], '/jobs/Acme/Capture/a.raf'],
  ['C:\\Jobs\\Acme\\\\', ['/Capture\\', 'Day 1/a.raf'], 'C:\\Jobs\\Acme\\Capture\\Day 1\\a.raf'],
  ['C:/Jobs/Acme', ['Capture', 'a.raf'], 'C:/Jobs/Acme/Capture/a.raf'],
  ['C:\\Jobs/Acme\\', ['\\Capture', 'a.raf'], 'C:/Jobs/Acme/Capture/a.raf'],
  ['/jobs/Acme', ['Capture', 'a\\b.raf'], '/jobs/Acme/Capture/a\\b.raf']
])('joins %s and %j as %s', (base, parts, expected) => {
  expect(joinPath(base, ...parts)).toBe(expected);
});

it('keeps a drive root when finding the parent of a photograph', () => {
  expect(dirname('C:\\a.raf')).toBe('C:\\');
  expect(dirname('\\\\?\\C:\\a.raf')).toBe('\\\\?\\C:\\');
  expect(dirname('/a.raf')).toBe('/');
});

it('compares a Windows join against both plain and legacy path spellings', () => {
  const expected = pathKey(joinPath('C:\\Jobs\\Acme', 'Capture', 'a.raf'));
  expect(pathKey('C:\\Jobs\\Acme\\Capture\\a.raf')).toBe(expected);
  expect(pathKey('C:\\Jobs\\Acme/Capture/a.raf')).toBe(expected);
  expect(pathKey('\\\\?\\C:\\Jobs\\Acme\\Capture\\a.raf')).toBe(expected);
});
