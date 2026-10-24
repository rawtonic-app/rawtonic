import '../../i18n';
import { useState } from 'react';
import { afterEach, describe, expect, it, vi } from 'vitest';
import { cleanup, fireEvent, render, screen, waitFor } from '@testing-library/react';
import { invoke } from '@tauri-apps/api/core';
import UsageStatsSettings from './UsageStatsSettings';
import type { AppSettings } from '../ui/AppProperties';

vi.mock('@tauri-apps/api/core', () => ({ invoke: vi.fn(() => Promise.resolve()) }));
afterEach(cleanup);

const next = JSON.stringify(
  {
    schema: 1,
    week: '2026-W43',
    app: { version: '0.1.0-alpha.4', os: 'macos', osVersion: '27.0', arch: 'aarch64' },
    gpuVendor: 'apple',
    cameras: [],
    tools: {},
  },
  null,
  2,
);

describe('usage statistics in Preferences', () => {
  it('is off by default; switching on shows what is sent before anything is', async () => {
    const saved = vi.fn();
    const preview = vi.fn(() => Promise.resolve({ pending: [{ week: '2026-W43', body: next, current: true }], lastSent: null }));
    function Host() {
      const [settings, setSettings] = useState({} as AppSettings);
      return (
        <UsageStatsSettings
          appSettings={settings}
          onSettingsChange={(s) => {
            saved(s);
            setSettings(s);
          }}
          preview={preview}
        />
      );
    }
    render(<Host />);
    const toggle = screen.getByRole('checkbox', { name: /anonymous usage statistics/ }) as HTMLInputElement;
    expect(toggle.checked).toBe(false);
    expect(screen.queryByTestId('usage-stats-preview')).toBeNull();
    fireEvent.click(toggle);
    await waitFor(() => expect(saved).toHaveBeenCalledWith({ usageStats: true }));
    await waitFor(() => expect(invoke).toHaveBeenCalledWith('usage_stats_set_enabled', { enabled: true }));
    expect(await screen.findByText('Week 2026-W43')).toBeTruthy();
    fireEvent.click(toggle);
    await waitFor(() => expect(invoke).toHaveBeenCalledWith('usage_stats_set_enabled', { enabled: false }));
    await screen.findByText('Statistics are off: nothing is kept or sent.');
  });

  it('shows the next send in plain words or as its exact body', async () => {
    const preview = vi.fn(() => Promise.resolve({
        pending: [
          { week: '2026-W42', body: next.replace('2026-W43', '2026-W42'), current: false },
          { week: '2026-W43', body: next, current: true },
        ],
        lastSent: { week: '2026-W41', body: next },
      }));
    render(<UsageStatsSettings appSettings={{ usageStats: true } as AppSettings} onSettingsChange={() => {}} preview={preview} />);
    fireEvent.click(screen.getByRole('button', { name: 'Show what is sent' }));
    expect((await screen.findAllByText('Week 2026-W43')).length).toBe(2);
    expect(screen.getByText('Week 2026-W42')).toBeTruthy();
    expect(screen.getByText('Finished week, sent at the next start:')).toBeTruthy();
    expect(screen.getByText('This week so far (sent after the week ends):')).toBeTruthy();
    expect(screen.getAllByText('Cameras opened: none').length).toBe(3);
    expect(screen.getByText('Last sent (week 2026-W41):')).toBeTruthy();
    fireEvent.click(screen.getByRole('checkbox', { name: 'Show raw data' }));
    expect(screen.getAllByText((_, el) => el?.tagName === 'PRE' && el.textContent === next).length).toBe(2);
  });

  it('while off keeps and shows nothing for the next send', async () => {
    const preview = vi.fn(() => Promise.resolve({ pending: [{ week: '2026-W43', body: next, current: true }], lastSent: null }));
    render(<UsageStatsSettings appSettings={{} as AppSettings} onSettingsChange={() => {}} preview={preview} />);
    fireEvent.click(screen.getByRole('button', { name: 'Show what is sent' }));
    await screen.findByText('Statistics are off: nothing is kept or sent.');
    expect(screen.queryByText('Week 2026-W43')).toBeNull();
  });

  it('says when the choice could not be saved and does not show it as made', async () => {
    const saved = vi.fn();
    const enable = vi.fn((_on: boolean) => Promise.reject(new Error('disk full')));
    render(
      <UsageStatsSettings
        appSettings={{ usageStats: true } as AppSettings}
        onSettingsChange={saved}
        preview={() => Promise.resolve({ pending: [], lastSent: null })}
        enable={enable}
      />,
    );
    fireEvent.click(screen.getByRole('checkbox', { name: /anonymous usage statistics/ }));
    expect((await screen.findByRole('alert')).textContent).toMatch(/could not be saved/);
    expect(enable).toHaveBeenCalledWith(false);
    expect(saved).not.toHaveBeenCalled();
    expect((screen.getByRole('checkbox', { name: /anonymous usage statistics/ }) as HTMLInputElement).checked).toBe(true);
  });
});
