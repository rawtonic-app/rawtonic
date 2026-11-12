// SPDX-License-Identifier: AGPL-3.0-or-later
// The first start's shader compile as a running task, and Reset graphics
// settings and retry (G76).
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { invoke } from '@tauri-apps/api/core';
import i18n from 'i18next';
import en from '../../i18n/locales/en.json';
import { Invokes, type AppSettings } from '../ui/AppProperties';
import { useActivityStore } from '../../store/useActivityStore';
import { useSettingsStore } from '../../store/useSettingsStore';
import { resetGraphicsAndRetry, watchGraphicsPreparation } from './gpuBackend';

vi.mock('@tauri-apps/api/core', () => ({ invoke: vi.fn() }));
vi.mock('@tauri-apps/plugin-os', () => ({ platform: () => 'windows' }));
vi.mock('@tauri-apps/plugin-process', () => ({ relaunch: vi.fn(async () => {}) }));

await i18n.init({ lng: 'en', resources: { en: { translation: en } }, interpolation: { escapeValue: false } });

const entry = () => useActivityStore.getState().entries.find((e) => e.id === 'gpu-preparing');

beforeEach(() => {
  useActivityStore.setState({ entries: [] });
  vi.mocked(invoke).mockReset();
  vi.mocked(invoke).mockResolvedValue(undefined);
});

afterEach(() => {
  vi.useRealTimers();
});

describe('Preparing graphics', () => {
  it('is not shown when the compile is quick (a warm start)', async () => {
    vi.useFakeTimers();
    const done = watchGraphicsPreparation(() => Promise.resolve(true), 1500);
    await vi.advanceTimersByTimeAsync(10);
    await done;
    await vi.advanceTimersByTimeAsync(2000);
    expect(entry()).toBeUndefined();
  });

  it('runs while a slow compile lasts and ends under Done with its time', async () => {
    vi.useFakeTimers();
    let finish: (compiled: boolean) => void = () => {};
    const done = watchGraphicsPreparation(() => new Promise<boolean>((resolve) => (finish = resolve)), 1500);
    await vi.advanceTimersByTimeAsync(1600);
    expect(entry()?.status).toBe('running');
    expect(entry()?.title).toBe('Preparing graphics for this computer');
    await vi.advanceTimersByTimeAsync(120_000);
    finish(true);
    await done;
    expect(entry()?.status).toBe('done');
    expect(entry()?.title).toBe('Graphics prepared for this computer in 122 s');
  });

  it('goes without a result when the GPU did not start', async () => {
    vi.useFakeTimers();
    let finish: (compiled: boolean) => void = () => {};
    const done = watchGraphicsPreparation(() => new Promise<boolean>((resolve) => (finish = resolve)), 1500);
    await vi.advanceTimersByTimeAsync(1600);
    finish(false);
    await done;
    expect(entry()).toBeUndefined();
  });
});

describe('Reset graphics settings and retry', () => {
  it('clears what the guard left out, sets Auto and relaunches', async () => {
    useSettingsStore.setState({ appSettings: { processingBackend: 'dx12' } as AppSettings });
    const restart = vi.fn(async () => {});
    await resetGraphicsAndRetry(restart);
    expect(invoke).toHaveBeenCalledWith(Invokes.GpuBackendUseAutomatic);
    expect(invoke).toHaveBeenCalledWith(Invokes.SaveSettings, {
      settings: expect.objectContaining({ processingBackend: 'auto' })
    });
    expect(useSettingsStore.getState().appSettings?.processingBackend).toBe('auto');
    expect(restart).toHaveBeenCalledOnce();
  });

  it('stays open and says so when the reset cannot be saved', async () => {
    useSettingsStore.setState({ appSettings: { processingBackend: 'auto' } as AppSettings });
    vi.mocked(invoke).mockImplementation(async (command) => {
      if (command === Invokes.GpuBackendUseAutomatic) throw new Error('read-only folder');
      return undefined;
    });
    const restart = vi.fn(async () => {});
    await resetGraphicsAndRetry(restart);
    expect(restart).not.toHaveBeenCalled();
    const failure = useActivityStore.getState().entries.find((e) => e.id === 'gpu-reset-failed');
    expect(failure?.title).toContain('read-only folder');
  });
});
