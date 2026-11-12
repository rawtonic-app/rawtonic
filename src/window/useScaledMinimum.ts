import { useEffect } from 'react';
import { isTauri } from '@tauri-apps/api/core';
import { LogicalSize } from '@tauri-apps/api/dpi';
import { getCurrentWindow } from '@tauri-apps/api/window';

/** Keep the same usable chrome area at every interface size (OS DPI is separate). */
export function useScaledMinimum(scale: number, platform: string | null) {
  useEffect(() => {
    if (!isTauri() || platform === 'macos' || !platform) return;
    const safeScale = Number.isFinite(scale) ? Math.max(1, scale) : 1;
    void getCurrentWindow()
      .setMinSize(new LogicalSize(1000 * safeScale, 600 * safeScale))
      .catch(console.error);
  }, [scale, platform]);
}
