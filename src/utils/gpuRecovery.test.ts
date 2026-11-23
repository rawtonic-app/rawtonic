import { describe, expect, it, vi } from 'vitest';

const listeners = vi.hoisted(() => new Map<string, (event: { payload: unknown }) => void>());
vi.mock('@tauri-apps/api/event', () => ({
  listen: vi.fn((event: string, handler: (event: { payload: unknown }) => void) => {
    listeners.set(event, handler);
    return Promise.resolve(() => listeners.delete(event));
  })
}));

import { GPU_DEVICE_RECOVERED, onGpuRecovered } from './gpuRecovery';

describe('onGpuRecovered', () => {
  it('hears each recovery until stopped', async () => {
    const heard = vi.fn();
    const stop = onGpuRecovered(heard);
    await Promise.resolve();
    listeners.get(GPU_DEVICE_RECOVERED)?.({ payload: { lost: 1, reason: 'lost', tookMs: 800 } });
    expect(heard).toHaveBeenCalledWith({ lost: 1, reason: 'lost', tookMs: 800 });
    stop();
    await Promise.resolve();
    await Promise.resolve();
    expect(listeners.has(GPU_DEVICE_RECOVERED)).toBe(false);
  });
});