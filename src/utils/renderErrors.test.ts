import '../i18n';
import i18n from 'i18next';
import { beforeEach, describe, expect, it } from 'vitest';

import { renderErrorMessage } from './renderErrors';

/** The backend's errors (rawtonic_gpu::gpu_processing). */
const textureLimit =
  'The image (40000 × 9000 px) is larger than this graphics card can render (16384 px per side).';

beforeEach(() => {
  void i18n.changeLanguage('en');
});

describe('render errors', () => {
  it('translates the texture limit error', () => {
    expect(renderErrorMessage(textureLimit)).toBe('The photo (40000 × 9000 px) is larger than this graphics card can render (16384 px per side).');
    void i18n.changeLanguage('de');
    expect(renderErrorMessage(textureLimit)).toBe(
      'Das Bild (40000 × 9000 px) ist größer, als diese Grafikkarte rendern kann (16384 px pro Seite).',
    );
    expect(renderErrorMessage(new Error(textureLimit))).toBe(
      'Das Bild (40000 × 9000 px) ist größer, als diese Grafikkarte rendern kann (16384 px pro Seite).',
    );
  });

  it('translates known export errors', () => {
    void i18n.changeLanguage('de');
    expect(renderErrorMessage('3 of 12 exports failed')).toBe('3 von 12 Exporten fehlgeschlagen');
    expect(renderErrorMessage('/Volumes/Card/DSC_1.NEF?vc=1 is offline: exporting needs the RAW file')).toBe(
      'DSC_1.NEF ist offline: Zum Exportieren wird die RAW-Datei benötigt.',
    );
    expect(renderErrorMessage("Failed to write file to '/out/a.jpg': No space left on device (os error 28)")).toBe(
      'a.jpg konnte nicht geschrieben werden: No space left on device (os error 28)',
    );
    expect(renderErrorMessage('Export task terminated unexpectedly')).toBe('Der Export wurde unerwartet beendet.');
    expect(renderErrorMessage('Unreadable export request')).toBe(
      'Die gespeicherten Einstellungen dieses Exports konnten nicht gelesen werden.',
    );
    void i18n.changeLanguage('en');
    expect(renderErrorMessage('3 of 12 exports failed')).toBe('3 of 12 exports failed');
  });

  it('keeps other errors', () => {
    expect(renderErrorMessage('Thread crashed: panicked')).toBe('Thread crashed: panicked');
    expect(renderErrorMessage('constructor')).toBe('constructor');
  });
});
