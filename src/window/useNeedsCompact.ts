import { useEffect, useState } from 'react';

/** Margin kept on each side of a floating toolbar (CSS px). */
export const TOOLBAR_MARGIN = 8;

/**
 * Compact when the toolbar's full content (measured on an invisible copy,
 * which never shrinks) is wider than the room the container gives it. Depends
 * only on the two widths now, never on how the window got there, so widening
 * the window always brings the full toolbar back.
 */
export function needsCompact(fullWidth: number, containerWidth: number): boolean {
  return fullWidth > 0 && containerWidth > 0 && fullWidth > containerWidth - 2 * TOOLBAR_MARGIN;
}

export function useNeedsCompact(container: HTMLElement | null, full: HTMLElement | null): boolean {
  const [compact, setCompact] = useState(false);
  useEffect(() => {
    if (!container || !full) return;
    const check = () => setCompact(needsCompact(full.offsetWidth, container.clientWidth));
    check();
    if (typeof ResizeObserver === 'undefined') return;
    const observer = new ResizeObserver(check);
    observer.observe(container);
    observer.observe(full);
    return () => observer.disconnect();
  }, [container, full]);
  return compact;
}
