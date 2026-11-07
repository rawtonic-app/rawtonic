import { describe, expect, it, vi } from 'vitest';
import { listen } from '@tauri-apps/api/event';
import { onSharpenViewReady, sharpenViewFor, sharpenViewWhile } from './sharpenView';

vi.mock('@tauri-apps/api/event', () => ({ listen: vi.fn() }));

describe('sharpening view', () => {
  it('shows the mask for Masking and the photo in grey for Amount, Radius and Detail', () => {
    expect(sharpenViewFor('sharpenMasking')).toBe('mask');
    for (const key of ['sharpness', 'sharpenRadius', 'sharpenDetail']) expect(sharpenViewFor(key)).toBe('grey');
    for (const key of ['sharpnessThreshold', 'sharpnessFalloff', 'exposure']) expect(sharpenViewFor(key)).toBeNull();
  });
  it('shows only while the slider is dragged with Option held', () => {
    expect(sharpenViewWhile('sharpenMasking', true, true)).toBe('mask');
    expect(sharpenViewWhile('sharpenMasking', true, false)).toBeNull();
    expect(sharpenViewWhile('sharpenMasking', false, true)).toBeNull();
    expect(sharpenViewWhile('exposure', true, true)).toBeNull();
  });
  it('renders the view again when its variant is ready, only while it is shown', async () => {
    let fire = () => {};
    const stop = vi.fn();
    vi.mocked(listen).mockImplementation(async (event, handler) => {
      expect(event).toBe('sharpen-view-ready');
      fire = () => handler({ event, id: 0, payload: null } as never);
      return stop;
    });
    let shown = true;
    const render = vi.fn();
    const unlisten = onSharpenViewReady(() => shown, render);
    await Promise.resolve();
    fire();
    shown = false;
    fire();
    expect(render).toHaveBeenCalledTimes(1);
    unlisten();
    await new Promise((resolve) => setTimeout(resolve));
    expect(stop).toHaveBeenCalled();
  });
});
