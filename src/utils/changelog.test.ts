// SPDX-License-Identifier: AGPL-3.0-or-later
import { expect, it } from 'vitest';
import { changelogSection, isNewerVersion } from './changelog';
import changelog from '../../CHANGELOG.md?raw';

it('finds a version section of the bundled CHANGELOG.md and stops at the next one', () => {
  const section = changelogSection(changelog, '0.1.0-alpha.7.1');
  expect(section).toMatch(/^A fix release for alpha\.7/);
  expect(section).toContain('AI masks show on the first open');
  expect(section).not.toContain('## 0.1.0-alpha.7 ');
  // alpha.7 is not alpha.7.1's prefix match.
  expect(changelogSection(changelog, '0.1.0-alpha.7')).toMatch(/^The seventh alpha/);
  expect(changelogSection(changelog, '9.9.9')).toBeNull();
  expect(changelogSection('## 1.0.0\n\n## 0.9.0\n- x', '1.0.0')).toBeNull();
});

it('orders versions by semantic-version precedence', () => {
  const ordered = ['0.1.0-alpha.3', '0.1.0-alpha.7', '0.1.0-alpha.7.1', '0.1.0-alpha.10', '0.1.0-beta.1', '0.1.0', '0.1.1', '1.0.0'];
  for (let i = 0; i < ordered.length; i++) {
    for (let j = 0; j < ordered.length; j++) {
      expect(isNewerVersion(ordered[i], ordered[j]), `${ordered[i]} > ${ordered[j]}`).toBe(i > j);
    }
  }
  expect(isNewerVersion('1.0.0+build.2', '1.0.0+build.1')).toBe(false);
  expect(isNewerVersion('garbage', '0.1.0')).toBe(false);
});
