import '../i18n';
import { expect, it } from 'vitest';
import { loadErrorText } from './loadError';

it("explains a file the decoder cannot read instead of quoting it", () => {
  const text = loadErrorText(
    "Error: No decoder found, model '', make: '', mode: ''",
    '/p/Capture/4.dng',
  );
  expect(text).toContain('4.dng');
  expect(text).not.toContain('decoder');
  expect(text).toMatch(/damaged|support/i);
});

it("names the file and the reason for other failures", () => {
  const text = loadErrorText('Error: Permission denied (os error 13)', '/p/a.jpg');
  expect(text).toContain('a.jpg');
  expect(text).toContain('Permission denied');
});
