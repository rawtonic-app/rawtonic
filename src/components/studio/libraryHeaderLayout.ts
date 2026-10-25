import { useEffect, useLayoutEffect, useState, type RefObject } from "react";

/** Budget in unscaled CSS pixels: enlarged controls get the same room. */
export const libraryHeaderFits = (width: number, scale: number, minimum = 1440) => width / scale >= minimum;

/** Translations, large counts and active-filter actions can need more room
 * than the base budget. Measure the actual controls before allowing overlap. */
export function useLibraryHeaderFits(
  scale: number,
  toolbar: RefObject<HTMLDivElement | null>,
  contentKey: string,
  eligible: boolean,
) {
  const [width, setWidth] = useState(() => window.innerWidth);
  const [measured, setMeasured] = useState({ key: "", minimum: 1440 });
  const minimum = measured.key === contentKey ? measured.minimum : 1440;
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
