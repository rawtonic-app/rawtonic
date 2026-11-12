import { useEffect, useState } from 'react';

type Widths = { clientWidth: number; scrollWidth: number };

/**
 * Whether the top bar runs compact (action names hidden, tighter spacing):
 * from the moment its content no longer fits, until the bar is again as wide
 * as the content needed then. The window controls are the bar's last items,
 * so an overflowing bar would clip them first; compacting keeps them on
 * screen on a small logical screen at a large interface size.
 */
export function nextCompact(state: { compact: boolean; needed: number }, bar: Widths) {
  if (!state.compact) return bar.scrollWidth > bar.clientWidth + 1 ? { compact: true, needed: bar.scrollWidth } : state;
  return bar.clientWidth >= state.needed ? { compact: false, needed: 0 } : state;
}

export function useCompactBar(bar: HTMLElement | null) {
  const [compact, setCompact] = useState(false);
  useEffect(() => {
    if (!bar || typeof ResizeObserver === 'undefined') return;
    let state = { compact: false, needed: 0 };
    const check = () => {
      const next = nextCompact(state, bar);
      if (next === state) return;
      state = next;
      setCompact(next.compact);
    };
    const observer = new ResizeObserver(check);
    observer.observe(bar);
    for (const child of Array.from(bar.children)) observer.observe(child);
    check();
    return () => observer.disconnect();
  }, [bar]);
  return compact;
}
