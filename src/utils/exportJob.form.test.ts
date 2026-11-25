import { beforeEach, expect, it } from 'vitest';
import { readFormDraft, writeFormDraft } from './exportJob';
import { DEFAULT_RECIPE_DRAFT } from './exportRecipes';

beforeEach(() => localStorage.clear());

it('brings back a chosen watermark file and font, and a stored folder', () => {
  const draft = {
    ...DEFAULT_RECIPE_DRAFT,
    watermark: true,
    watermarkPath: '/marks/logo.png',
    watermarkFont: 'Inter',
    folder: '/out'
  };
  writeFormDraft('/Shoots/A', draft);
  expect(readFormDraft('/Shoots/A')).toEqual(draft);
});

it('ignores unknown keys, wrong types and other libraries', () => {
  localStorage.setItem('rawtonic.export.form:/Shoots/A', JSON.stringify({ quality: 'high', extra: 1, format: 'png' }));
  expect(readFormDraft('/Shoots/A')).toEqual({ ...DEFAULT_RECIPE_DRAFT, format: 'png' });
  expect(readFormDraft('/Shoots/B')).toBeNull();
});

it('brings back a JPEG file size limit and drops a nonsensical one', () => {
  const draft = { ...DEFAULT_RECIPE_DRAFT, maxFileKb: 500 };
  writeFormDraft('/Shoots/A', draft);
  expect(readFormDraft('/Shoots/A')?.maxFileKb).toBe(500);
  localStorage.setItem('rawtonic.export.form:/Shoots/A', JSON.stringify({ maxFileKb: 'big' }));
  expect(readFormDraft('/Shoots/A')?.maxFileKb).toBeNull();
  localStorage.setItem('rawtonic.export.form:/Shoots/A', JSON.stringify({ maxFileKb: 0 }));
  expect(readFormDraft('/Shoots/A')?.maxFileKb).toBeNull();
});

it("remembers Super Resolution's quality and keeps Full for forms from before the choice", () => {
  writeFormDraft('/Shoots/Q', { ...DEFAULT_RECIPE_DRAFT, superResolution: true, superResolutionQuality: 'full' });
  expect(readFormDraft('/Shoots/Q')?.superResolutionQuality).toBe('full');
  writeFormDraft('/Shoots/Q', { ...DEFAULT_RECIPE_DRAFT, superResolution: true, superResolutionQuality: 'fast' });
  expect(readFormDraft('/Shoots/Q')?.superResolutionQuality).toBe('fast');
  // Remembered before the choice existed: Full, never the machine default.
  writeFormDraft('/Shoots/Q', { ...DEFAULT_RECIPE_DRAFT, superResolution: true });
  expect(readFormDraft('/Shoots/Q')?.superResolutionQuality).toBe('full');
  // Without Super Resolution the new-recipe default is still open.
  writeFormDraft('/Shoots/Q', { ...DEFAULT_RECIPE_DRAFT });
  expect(readFormDraft('/Shoots/Q')?.superResolutionQuality).toBeUndefined();
});
