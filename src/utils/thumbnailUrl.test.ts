// SPDX-License-Identifier: AGPL-3.0-or-later
import { afterEach, describe, expect, it, vi } from 'vitest';

vi.mock('@tauri-apps/api/core', () => ({
  convertFileSrc: (path: string, protocol = 'asset') => `${protocol}://localhost/${encodeURIComponent(path)}`
}));
const os = vi.hoisted(() => ({ name: 'macos' }));
vi.mock('@tauri-apps/plugin-os', () => ({ platform: () => os.name }));

import {
  forMonitorTransform,
  setMonitorTransform,
  setThumbnailPlatformForTests,
  thumbnailFileUrl
} from './thumbnailUrl';

describe('thumbnailFileUrl', () => {
  afterEach(() => {
    setThumbnailPlatformForTests(null);
    os.name = 'macos';
  });

  it('goes through the scheme on Linux and Windows, the asset protocol on macOS', () => {
    // Windows: WebView2 applies only the system's profile; the scheme
    // serves the file as it is unless a chosen profile is shown (MCM3).
    for (const [name, scheme] of [
      ['linux', true],
      ['windows', true],
      ['macos', false]
    ] as const) {
      setThumbnailPlatformForTests(null);
      os.name = name;
      const url = thumbnailFileUrl('C:\\cache\\a_small.jpg');
      expect(url.startsWith(scheme ? 'rawtonic-thumb://' : 'asset://'), `${name}: ${url}`).toBe(true);
    }
  });

  it('uses the asset protocol where the system matches images to the display', () => {
    setThumbnailPlatformForTests(false);
    expect(thumbnailFileUrl('C:\\cache\\a_small.jpg')).toBe(
      `asset://localhost/${encodeURIComponent('C:/cache/a_small.jpg')}`
    );
  });

  it('goes through the converting scheme on Linux and names the monitor transform', () => {
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
