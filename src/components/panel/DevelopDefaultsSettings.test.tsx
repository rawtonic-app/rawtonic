import '../../i18n';
import { afterEach, expect, it, vi } from 'vitest';
import { cleanup, fireEvent, render, screen } from '@testing-library/react';
import DevelopDefaultsSettings from './DevelopDefaultsSettings';
import type { AppSettings } from '../ui/AppProperties';

vi.mock('@tauri-apps/api/core', () => ({ invoke: vi.fn(() => Promise.resolve([])) }));
afterEach(cleanup);

it('lists the camera defaults, keeps RAW only on by default and removes an entry', () => {
  const save = vi.fn();
  const appSettings = {
    developDefaults: {
      rawOnly: true,
      global: { name: 'House look', settings: { contrast: 10 } },
      cameras: [
        { make: 'Nikon', model: 'Z 9', settings: { grainAmount: 20 }, inherit: true },
        {
          make: 'Nikon',
          model: 'Z 9',
          serial: '3001234',
          name: 'Body look',
          settings: { exposure: 0.2 },
          inherit: false
        }
      ]
    }
  } as unknown as AppSettings;
  render(<DevelopDefaultsSettings appSettings={appSettings} save={save} />);
  expect(screen.getByText('Nikon Z 9')).toBeTruthy();
  expect(screen.getByText('Nikon Z 9 (body 3001234)')).toBeTruthy();
  expect(screen.getByText('Adds to the global default')).toBeTruthy();
  expect(screen.getByText('Body look · Instead of the global default')).toBeTruthy();
  const rawOnly = screen.getByRole('checkbox', { name: 'RAW files only' }) as HTMLInputElement;
  expect(rawOnly.checked).toBe(true);
  fireEvent.click(rawOnly);
  expect(save).toHaveBeenLastCalledWith(expect.objectContaining({ rawOnly: false }));
  fireEvent.click(screen.getAllByRole('button', { name: 'Remove the default for Nikon Z 9' })[0]);
  const next = save.mock.calls.at(-1)![0];
  expect(next.cameras).toHaveLength(1);
  expect(next.cameras[0].serial).toBe('3001234');
  expect(next.global.name).toBe('House look');
});

it('says so when no camera has a default', () => {
  render(<DevelopDefaultsSettings appSettings={{} as AppSettings} save={vi.fn()} />);
  expect(screen.getByText('No camera defaults yet.')).toBeTruthy();
  expect((screen.getByRole('checkbox', { name: 'RAW files only' }) as HTMLInputElement).checked).toBe(true);
});
