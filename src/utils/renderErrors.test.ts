import '../i18n';
import i18n from 'i18next';
import { beforeEach, describe, expect, it } from 'vitest';

import { renderErrorMessage } from './renderErrors';

/** The backend's errors (rawtonic_gpu::gpu_processing). */
const textureLimit =
  'The image (40000 × 9000 px) is larger than this graphics card can render (16384 px per side).';
const flareLimit =
  "Light flare cannot be rendered on an image of 40000 × 9000 px, larger than this graphics card's limit (16384 px per side). Turn Light flare off to export it.";

beforeEach(() => {
  void i18n.changeLanguage('en');
});

describe('render errors', () => {
  it('translates the texture limit errors', () => {
    expect(renderErrorMessage(textureLimit)).toBe(textureLimit);
    expect(renderErrorMessage(flareLimit)).toBe(flareLimit);
    void i18n.changeLanguage('de');
    expect(renderErrorMessage(textureLimit)).toBe(
      'Das Bild (40000 × 9000 px) ist größer, als diese Grafikkarte rendern kann (16384 px pro Seite).',
    );
    expect(renderErrorMessage(new Error(flareLimit))).toMatch(/^Lichtreflex kann auf einem Bild mit 40000 × 9000 px/);
  });

  it('keeps other errors', () => {
    expect(renderErrorMessage('Export task terminated unexpectedly')).toBe(
      'Export task terminated unexpectedly',
    );
  });
});
