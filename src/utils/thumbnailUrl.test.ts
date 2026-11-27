// SPDX-License-Identifier: AGPL-3.0-or-later
import { afterEach, describe, expect, it, vi } from 'vitest';

vi.mock('@tauri-apps/api/core', () => ({
  convertFileSrc: (path: string, protocol = 'asset') => `${protocol}://localhost/${encodeURIComponent(path)}`
}));
vi.mock('@tauri-apps/plugin-os', () => ({ platform: () => 'macos' }));

import { forMonitorTransform, setMonitorTransform, setThumbnailPlatformForTests, thumbnailFileUrl } from './thumbnailUrl';

describe('thumbnailFileUrl', () => {
  afterEach(() => setThumbnailPlatformForTests(null));

  it('uses the asset protocol where the system matches images to the display', () => {
    setThumbnailPlatformForTests(false);
    expect(thumbnailFileUrl('C:\\cache\\a_small.jpg')).toBe(`asset://localhost/${encodeURIComponent('C:/cache/a_small.jpg')}`);
  });

  it('goes through the converting scheme on Linux and Windows and names the monitor transform', () => {
    setThumbnailPlatformForTests(true);
    const first = thumbnailFileUrl('/cache/a_small.jpg');
    expect(first).toBe(`rawtonic-thumb://localhost/${encodeURIComponent('/cache/a_small.jpg')}?mp=0`);
    const moved = forMonitorTransform(first, 3);
    expect(new URL(moved).searchParams.get('mp')).toBe('3');
    expect(moved).not.toBe(first);
    setMonitorTransform(3);
    expect(thumbnailFileUrl('/cache/a_small.jpg')).toBe(moved);
    // Inline data and other URLs stay as they are.
    expect(forMonitorTransform('data:image/jpeg;base64,AA', 4)).toBe('data:image/jpeg;base64,AA');
    expect(forMonitorTransform('asset://localhost/x.jpg', 4)).toBe('asset://localhost/x.jpg');
  });
});
