import { useLayoutEffect, useState } from 'react';

/**
 * Keeps a tool bar in one row, as Lightroom's and Capture One's bars do
 * (UX4-07, UX4-08, UXR4M-24, UXR4L-15/-16): `data-collapse` on the bar goes
 * from 0 up to `levels` until its content fits its own width (not the
 * window's: panels and the filmstrip dock change it), and the bar's CSS
 * drops words, then narrows fields, then moves rarely used controls into a
 * ⋯ menu at each level. Measured, so a longer translation collapses earlier
 * and nothing depends on fixed breakpoints. Only when the last level still
 * does not fit (the smallest window) does `data-wrap` let the bar wrap,
 * so no control is ever cut off. Returns the ref callback for the bar
 * (bars mount and unmount with their view).
 */
export function useFitOneRow(levels: number): (bar: HTMLElement | null) => void {
  const [bar, setBar] = useState<HTMLElement | null>(null);
  useLayoutEffect(() => {
    if (!bar) return;
    let frame = 0;
    const fit = () => {
      frame = 0;
      delete bar.dataset.wrap;
      for (let level = 0; level <= levels; level++) {
        bar.dataset.collapse = String(level);
        // A one-pixel tolerance for sub-pixel widths.
        if (bar.scrollWidth <= bar.clientWidth + 1) return;
      }
      bar.dataset.wrap = 'true';
    };
    const schedule = () => {
      if (!frame) frame = requestAnimationFrame(fit);
    };
    fit();
    // The bar's width (window, panels, docked filmstrip) and its content
    // (language, counts, controls that come and go) both change the fit.
    const resized = typeof ResizeObserver === 'undefined' ? null : new ResizeObserver(schedule);
    resized?.observe(bar);
    const changed = new MutationObserver(schedule);
    changed.observe(bar, { childList: true, subtree: true, characterData: true });
    return () => {
      if (frame) cancelAnimationFrame(frame);
      resized?.disconnect();
      changed.disconnect();
    };
  }, [bar, levels]);
  return setBar;
}
