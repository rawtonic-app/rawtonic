import '../../i18n';
import { afterEach, describe, expect, it, vi } from 'vitest';
import { cleanup, render, screen } from '@testing-library/react';
import { invoke } from '@tauri-apps/api/core';
import UpdateSettings from './UpdateSettings';
import CrashReportSettings from './CrashReportSettings';
import type { AppSettings } from '../ui/AppProperties';

vi.mock('@tauri-apps/api/app', () => ({ getVersion: vi.fn(() => Promise.resolve('0.1.0')) }));
vi.mock('@tauri-apps/api/core', () => ({ invoke: vi.fn(() => Promise.resolve()) }));
afterEach(cleanup);

describe('update and crash report preferences', () => {
  it('says Flatpak delivers the updates, with no switches, inside a Flatpak', async () => {
    vi.mocked(invoke).mockResolvedValueOnce(true);
    render(<UpdateSettings appSettings={{} as AppSettings} onSettingsChange={vi.fn()} />);
    expect(await screen.findByText(/flatpak update cc\.rawtonic\.editor/)).toBeTruthy();
    expect(screen.queryByRole('checkbox')).toBeNull();
    expect(screen.queryByRole('button')).toBeNull();
  });
  it('puts each preference on its own row, its name left and its control right', () => {
    render(
      <>
        <UpdateSettings appSettings={{} as AppSettings} onSettingsChange={vi.fn()} />
        <CrashReportSettings appSettings={{} as AppSettings} onSettingsChange={vi.fn()} />
      </>
    );
    const names = [
      'Check for updates at startup',
      'Include pre-releases',
      "Show what's new after an update",
      'Offer to send a crash report after Rawtonic quit unexpectedly'
    ];
    for (const name of names) {
      const row = screen.getByRole('checkbox', { name }).closest('.pref-row');
      expect(row?.querySelector('.pref-label')?.textContent).toBe(name);
      expect(row?.querySelector('.pref-control')?.contains(screen.getByRole('checkbox', { name }))).toBe(true);
    }
    const check = screen.getByRole('button', { name: 'Check now' });
    expect(check.closest('.pref-row')?.querySelector('.pref-label')?.textContent).toBe('Check for updates');
  });
});
