import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { clearDraftPatch, registerDraftPatchHost, retireDraftPatches, showDraftPatch } from './browserDraftPatch';

// jsdom decodes no images: each decode resolves when the test says so.
const decodes = new Map<string, () => void>();
function FakeImage() {
  const image = document.createElement('img');
  image.decode = () => new Promise<void>((resolve) => decodes.set(image.src, resolve));
  return image;
}
const patch = (url: string) => ({ url, normX: 0, normY: 0, normW: 1, normH: 1 });
const shown = (host: HTMLElement) => (host.firstElementChild as HTMLImageElement | null)?.src ?? null;
async function finish(url: string) {
  decodes.get(url)!();
  await Promise.resolve();
  await Promise.resolve();
}

describe('browser draft patches', () => {
  let host: HTMLDivElement;
  beforeEach(() => {
    vi.stubGlobal('Image', FakeImage);
    host = document.createElement('div');
    registerDraftPatchHost(host);
  });
  afterEach(() => {
    registerDraftPatchHost(null);
    vi.unstubAllGlobals();
    decodes.clear();
  });

  it('shows the newest decoded draft and ignores older ones finishing late', async () => {
    showDraftPatch(patch('http://a/1'), 101);
    showDraftPatch(patch('http://a/2'), 102);
    await finish('http://a/2');
    expect(shown(host)).toBe('http://a/2');
    await finish('http://a/1');
    expect(shown(host)).toBe('http://a/2');
  });

  it('never lets a draft still decoding cover a final frame', async () => {
    showDraftPatch(patch('http://a/3'), 103);
    await finish('http://a/3');
    showDraftPatch(patch('http://a/4'), 104);
    retireDraftPatches();
    await finish('http://a/4');
    // The shown draft stays until the final frame is on screen.
    expect(shown(host)).toBe('http://a/3');
    clearDraftPatch();
    expect(shown(host)).toBeNull();
  });

  it('declines without a viewer', () => {
    registerDraftPatchHost(null);
    expect(showDraftPatch(patch('http://a/5'), 105)).toBe(false);
  });
});
