import '../i18n';
import i18n from 'i18next';
import { beforeEach, describe, expect, it } from 'vitest';

import { renderErrorMessage } from './renderErrors';

/** The backend's errors (rawtonic_gpu::gpu_processing). */
const textureLimit = 'The image (40000 × 9000 px) is larger than this graphics card can render (16384 px per side).';

beforeEach(() => {
  void i18n.changeLanguage('en');
});

describe('render errors', () => {
  it('translates the texture limit error', () => {
    expect(renderErrorMessage(textureLimit)).toBe(
      'The photo (40000 × 9000 px) is larger than this graphics card can render (16384 px per side).'
    );
    void i18n.changeLanguage('de');
    expect(renderErrorMessage(textureLimit)).toBe(
      'Das Bild (40000 × 9000 px) ist größer, als diese Grafikkarte rendern kann (16384 px pro Seite).'
    );
    expect(renderErrorMessage(new Error(textureLimit))).toBe(
      'Das Bild (40000 × 9000 px) ist größer, als diese Grafikkarte rendern kann (16384 px pro Seite).'
    );
  });

  it('translates known export errors', () => {
    void i18n.changeLanguage('de');
    expect(renderErrorMessage('3 of 12 exports failed')).toBe('3 von 12 Exporten fehlgeschlagen');
    expect(renderErrorMessage('/Volumes/Card/DSC_1.NEF?vc=1 is offline: exporting needs the RAW file')).toBe(
      'DSC_1.NEF ist offline: Zum Exportieren wird die RAW-Datei benötigt.'
    );
    expect(renderErrorMessage("Failed to write file to '/out/a.jpg': No space left on device (os error 28)")).toBe(
      'a.jpg konnte nicht geschrieben werden: No space left on device (os error 28)'
    );
    expect(renderErrorMessage('Export task terminated unexpectedly')).toBe('Der Export wurde unerwartet beendet.');
    expect(
      renderErrorMessage(
        'Not enough graphics memory for this photo. Close other programs that use the graphics card and export this photo again.'
      )
    ).toBe(
      'Nicht genug Grafikspeicher für dieses Foto. Schließe andere Programme, die die Grafikkarte nutzen, und exportiere dieses Foto erneut.'
    );
    expect(
      renderErrorMessage(
        'This photo could not be rendered on this graphics chip: it stopped responding (out of memory). Quit and reopen Rawtonic, then export this photo again.'
      )
    ).toBe(
      'Dieses Foto konnte auf diesem Grafikchip nicht gerendert werden: Er reagiert nicht mehr (Speicher voll). Beende Rawtonic, öffne es erneut und exportiere dieses Foto noch einmal.'
    );
    expect(
      renderErrorMessage(
        "This photo's edits are too complex for this graphics chip's driver to export. Remove some edits (effects, local adjustments) and export it again, or export it on another computer."
      )
    ).toBe(
      'Die Bearbeitung dieses Fotos ist zu umfangreich, um sie mit dem Treiber dieses Grafikchips zu exportieren. Entferne einige Bearbeitungen (Effekte, lokale Korrekturen) und exportiere erneut, oder exportiere das Foto auf einem anderen Computer.'
    );
    expect(renderErrorMessage('Unreadable export request')).toBe(
      'Die gespeicherten Einstellungen dieses Exports konnten nicht gelesen werden.'
    );
    void i18n.changeLanguage('en');
    expect(renderErrorMessage('3 of 12 exports failed')).toBe('3 of 12 exports failed');
  });

  it('keeps other errors', () => {
    expect(renderErrorMessage('Thread crashed: panicked')).toBe('Thread crashed: panicked');
    expect(renderErrorMessage('constructor')).toBe('constructor');
  });
});
