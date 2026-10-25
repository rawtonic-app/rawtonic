import { useEffect, useLayoutEffect, useState, type RefObject } from "react";

/** The English toolbar with counts fits at 1440 CSS px in browser QA at
 * 100% UI scale (including macOS titlebar space). Enlarged controls get the
 * same logical room; translated/active controls are also measured below. */
const LIBRARY_HEADER_MINIMUM = 1440;
export const libraryHeaderFits = (width: number, scale: number, minimum = LIBRARY_HEADER_MINIMUM) => width / scale >= minimum;

/** Translations, large counts and active-filter actions can need more room
 * than the base budget. Measure the actual controls before allowing overlap. */
export function useLibraryHeaderFits(
  scale: number,
  toolbar: RefObject<HTMLDivElement | null>,
  contentKey: string,
  eligible: boolean,
) {
  const [width, setWidth] = useState(() => window.innerWidth);
  const [measured, setMeasured] = useState({ key: "", minimum: LIBRARY_HEADER_MINIMUM });
  const minimum = measured.key === contentKey ? measured.minimum : LIBRARY_HEADER_MINIMUM;
  const fits = eligible && libraryHeaderFits(width, scale, minimum);
  useEffect(() => {
    const resize = () => setWidth(window.innerWidth);
    window.addEventListener("resize", resize);
    return () => window.removeEventListener("resize", resize);
  }, []);
  useLayoutEffect(() => {
    const element = toolbar.current;
    if (!fits || !element) return;
    const measure = () => {
      const edge = element.getBoundingClientRect().right;
      const right = Math.max(edge, ...[...element.querySelectorAll("button,select,input")]
        .map((control) => control.getBoundingClientRect().right));
      if (right > edge + 1) setMeasured({ key: contentKey, minimum: (window.innerWidth + right - edge + 8) / scale });
    };
    measure();
    const observer = new ResizeObserver(measure);
    observer.observe(element);
    return () => observer.disconnect();
  }, [fits, toolbar, contentKey, scale]);
  return fits;
}
