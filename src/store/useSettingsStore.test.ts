import { beforeEach, expect, it, vi } from 'vitest';
import { invoke } from '@tauri-apps/api/core';
import { saveSettings } from './useSettingsStore';
import { AppSettings, Invokes } from '../components/ui/AppProperties';

vi.mock('@tauri-apps/api/core', () => ({ invoke: vi.fn() }));
vi.mock('@tauri-apps/plugin-os', () => ({ platform: () => 'macos' }));

const snapshot = (theme: string) => ({ theme }) as unknown as AppSettings;
const flush = () => new Promise((resolve) => setTimeout(resolve, 0));

beforeEach(() => vi.clearAllMocks());

it('sends a save at once, and the next only after the earlier one finished', async () => {
  const finish: Array<(value?: unknown) => void> = [];
  const fail: Array<(reason: unknown) => void> = [];
  vi.mocked(invoke).mockImplementation(
    () =>
      new Promise((resolve, reject) => {
        finish.push(resolve);
        fail.push(reject);
      })
  );
  const first = saveSettings(snapshot('light'));
  expect(invoke).toHaveBeenCalledTimes(1);
  const second = saveSettings(snapshot('dark'));
  const third = saveSettings(snapshot('grey'));
  await flush();
  expect(invoke).toHaveBeenCalledTimes(1);
  // A failed save still lets the next one go, and reports its own failure.
  fail[0]('disk full');
  await expect(first).rejects.toBe('disk full');
  await flush();
  expect(invoke).toHaveBeenCalledTimes(2);
  finish[1]();
  await second;
  await flush();
  finish[2]();
  await third;
  expect(vi.mocked(invoke).mock.calls.map(([, args]) => (args as { settings: AppSettings }).settings.theme)).toEqual([
    'light',
    'dark',
    'grey'
  ]);
  expect(vi.mocked(invoke).mock.calls.every(([command]) => command === Invokes.SaveSettings)).toBe(true);
  // Nothing under way any more: the next save goes out at once.
  await flush();
  saveSettings(snapshot('black'));
  expect(invoke).toHaveBeenCalledTimes(4);
});
