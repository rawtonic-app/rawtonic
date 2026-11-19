import '../i18n';
import i18n from 'i18next';
import { beforeEach, describe, expect, it, vi } from 'vitest';
import { version } from '@tauri-apps/plugin-os';
import type { AppSettings } from '../components/ui/AppProperties';

import { renderErrorMessage } from './renderErrors';
import { useSettingsStore } from '../store/useSettingsStore';

vi.mock('@tauri-apps/plugin-os', () => ({ platform: () => 'macos', version: vi.fn() }));

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

it('localizes the persisted Nikon High Efficiency export reason and adds the Mac option', async () => {
  const { osPlatform, appSettings } = useSettingsStore.getState();
  const error =
    "This NEF uses Nikon's High Efficiency compression. Its codec (TicoRAW) is patent-protected, so Rawtonic cannot decode it. Convert the file to DNG with Adobe DNG Converter, or set the camera to Lossless compressed RAW.";
  try {
    await i18n.changeLanguage('de');
    useSettingsStore.setState({ osPlatform: 'linux', appSettings: null });
    vi.mocked(version).mockReturnValue('27.0.1');
    expect(renderErrorMessage(error)).toContain('ist patentgeschützt');
    expect(renderErrorMessage(error)).not.toContain('macOS 27');

    useSettingsStore.setState({ osPlatform: 'macos' });
    vi.mocked(version).mockReturnValue('26.6.0');
    expect(renderErrorMessage(error)).not.toContain('macOS 27');

    vi.mocked(version).mockReturnValue('27.0.1');
    const setting = i18n.t('settings.processing.preprocessing.appleRaw9');
    expect(renderErrorMessage(error)).toContain(
      `Ab macOS 27 kannst du versuchen, „${setting}“ in den Einstellungen zu aktivieren.`
    );

    useSettingsStore.setState({ appSettings: { ...appSettings, useAppleRaw9: true } as AppSettings });
    expect(renderErrorMessage(error)).not.toContain('macOS 27');
  } finally {
    useSettingsStore.setState({ osPlatform, appSettings });
    await i18n.changeLanguage('en');
  }
});
