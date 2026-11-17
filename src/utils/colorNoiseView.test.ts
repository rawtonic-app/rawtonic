import { describe, expect, it, vi } from 'vitest';
import { listen } from '@tauri-apps/api/event';
import { onColorNoiseViewReady } from './colorNoiseView';

vi.mock('@tauri-apps/api/event', () => ({ listen: vi.fn() }));

describe('colour noise view', () => {
  it("asks for the photo's frame again when its correction is in", async () => {
    let fire = (_path: string) => {};
    const stop = vi.fn();
    vi.mocked(listen).mockImplementation(async (event, handler) => {
      expect(event).toBe('color-noise-view-ready');
      fire = (path) => handler({ event, id: 0, payload: { path } } as never);
      return stop;
    });
    const render = vi.fn();
    const unlisten = onColorNoiseViewReady(render);
    await Promise.resolve();
    fire('/photos/a.ARW');
    expect(render).toHaveBeenCalledWith('/photos/a.ARW');
    unlisten();
    await new Promise((resolve) => setTimeout(resolve));
    expect(stop).toHaveBeenCalled();
  });
});
