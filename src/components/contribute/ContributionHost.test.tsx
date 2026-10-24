import '../../i18n';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { act, cleanup, fireEvent, render, screen, waitFor } from '@testing-library/react';
import { invoke } from '@tauri-apps/api/core';
import ContributionHost from './ContributionHost';
import { useSettingsStore } from '../../store/useSettingsStore';
import type { AppSettings } from '../ui/AppProperties';
import type { CrashRecord } from './reports';

vi.mock('@tauri-apps/api/core', () => ({ invoke: vi.fn() }));
vi.mock('@tauri-apps/api/event', () => ({ listen: vi.fn(() => Promise.resolve(() => {})) }));
vi.mock('@tauri-apps/plugin-shell', () => ({ open: vi.fn() }));

const record = (id: string, asked = false): CrashRecord => ({
  id,
  crashedOn: '2026-10-23',
  created: 1792800000,
  app: { version: '0.1.0-alpha.4', os: 'macos', osVersion: '26.5', arch: 'aarch64' },
  message: 'index out of bounds',
  location: 'decode.rs:12:5',
  backtrace: '0: rawdecode::open (decode.rs:12:5)',
  logTail: 'opening <path>.CR3',
  asked,
});

let pending: CrashRecord[] = [];
const calls = () => vi.mocked(invoke).mock.calls.map(([command, args]) => [command, args]);
const handleSettingsChange = vi.fn((settings: AppSettings) => {
  useSettingsStore.setState({ appSettings: settings });
  return Promise.resolve();
});

const start = async (settings: Partial<AppSettings> = {}) => {
  useSettingsStore.setState({ appSettings: settings as AppSettings, handleSettingsChange });
  render(<ContributionHost />);
  await act(async () => {});
};

beforeEach(() => {
  pending = [];
  handleSettingsChange.mockClear();
  vi.mocked(invoke).mockReset();
  vi.mocked(invoke).mockImplementation((command: string) => {
    if (command === 'crash_reports_pending') return Promise.resolve(pending);
    if (command === 'contribution_available') return Promise.resolve(false);
    return Promise.resolve(undefined);
  });
});
afterEach(cleanup);

describe('the crash question at start', () => {
  it('asks once about a new crash and marks it asked; Not now keeps the record', async () => {
    pending = [record('b'), record('a', true)];
    await start();
    expect(await screen.findByText('Rawtonic quit unexpectedly last time. Send a report?')).toBeTruthy();
    expect(calls()).toContainEqual(['crash_report_mark_asked', { id: 'b' }]);
    fireEvent.click(screen.getByRole('button', { name: 'Not now' }));
    expect(screen.queryByText(/quit unexpectedly last time/)).toBeNull();
    expect(calls().some(([command]) => command === 'crash_report_remove' || command === 'crash_reports_set_enabled')).toBe(false);
  });

  it('does not ask about crashes asked before, or when the user chose Never', async () => {
    pending = [record('a', true)];
    await start();
    expect(screen.queryByText(/quit unexpectedly last time/)).toBeNull();
    cleanup();
    vi.mocked(invoke).mockClear();
    pending = [record('b')];
    await start({ crashReports: false });
    expect(screen.queryByText(/quit unexpectedly last time/)).toBeNull();
    expect(calls().some(([command]) => command === 'crash_reports_pending')).toBe(false);
  });

  it('Never turns the offer off and deletes the records', async () => {
    pending = [record('b')];
    await start();
    fireEvent.click(await screen.findByRole('button', { name: 'Never' }));
    await waitFor(() => expect(calls()).toContainEqual(['crash_reports_set_enabled', { enabled: false }]));
    expect(handleSettingsChange).toHaveBeenCalledWith(expect.objectContaining({ crashReports: false }));
  });

  it('Review opens the crash report with its fields', async () => {
    pending = [record('b')];
    await start();
    fireEvent.click(await screen.findByRole('button', { name: 'Review report…' }));
    expect(await screen.findByText('Crash report')).toBeTruthy();
    expect(screen.getByText('decode.rs:12:5')).toBeTruthy();
    // The message can be corrected before it is sent.
    expect((screen.getByRole('textbox', { name: /Error message/ }) as HTMLTextAreaElement).value).toBe('index out of bounds');
    expect(screen.getByRole('checkbox', { name: /Backtrace/ })).toBeTruthy();
  });
});
