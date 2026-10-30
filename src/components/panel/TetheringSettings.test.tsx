import '../../i18n';
import { useState } from 'react';
import { afterEach, expect, it, vi } from 'vitest';
import { cleanup, fireEvent, render, screen } from '@testing-library/react';
import TetheringSettings, { TETHER_CAMERAS_URL } from './TetheringSettings';
import type { AppSettings } from '../ui/AppProperties';

vi.mock('@tauri-apps/plugin-shell', () => ({ open: vi.fn(() => Promise.resolve()) }));
afterEach(cleanup);

it('offers Canon cameras off by default, with a plain warning and the compatibility page', () => {
  const saved = vi.fn();
  const openUrl = vi.fn(() => Promise.resolve());
  function Host() {
    const [settings, setSettings] = useState({} as AppSettings);
    return (
      <TetheringSettings
        appSettings={settings}
        onSettingsChange={(s) => {
          saved(s);
          setSettings(s);
        }}
        openUrl={openUrl}
      />
    );
  }
  render(<Host />);
  expect(screen.getByText('Experimental: Canon cameras (help test)')).toBeTruthy();
  const toggle = screen.getByRole('checkbox', { name: 'Connect Canon cameras' }) as HTMLInputElement;
  expect(toggle.checked).toBe(false);
  const warning = screen.getByTestId('canon-warning').textContent ?? '';
  expect(warning).toMatch(/Not tested yet/);
  expect(warning).toMatch(/only on the camera’s memory card, so keep a card in the camera/);
  expect(warning).toMatch(/never deletes anything on the camera or the card/);
  fireEvent.click(screen.getByRole('button', { name: /Which cameras work, and how to report yours/ }));
  expect(openUrl).toHaveBeenCalledWith(TETHER_CAMERAS_URL);
  fireEvent.click(toggle);
  expect(saved).toHaveBeenLastCalledWith({ tetherExperimentalCanon: true });
  expect((screen.getByRole('checkbox', { name: 'Connect Canon cameras' }) as HTMLInputElement).checked).toBe(true);
});
