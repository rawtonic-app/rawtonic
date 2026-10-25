import { useEffect, useState } from "react";

/** How long the pointer rests before it hides over the full screen. */
export const IDLE_CURSOR_MS = 1500;

/** True while `active` and the pointer has not moved for IDLE_CURSOR_MS
 * (the full screen hides it, as Lightroom's does); any move shows it. */
export function useIdleCursor(active: boolean): boolean {
  const [idle, setIdle] = useState(false);
  useEffect(() => {
    if (!active) {
      setIdle(false);
      return;
    }
    let timer = window.setTimeout(() => setIdle(true), IDLE_CURSOR_MS);
    const onMove = () => {
      setIdle(false);
      window.clearTimeout(timer);
      timer = window.setTimeout(() => setIdle(true), IDLE_CURSOR_MS);
    };
    window.addEventListener("pointermove", onMove);
    window.addEventListener("pointerdown", onMove);
    return () => {
      window.clearTimeout(timer);
      window.removeEventListener("pointermove", onMove);
      window.removeEventListener("pointerdown", onMove);
    };
  }, [active]);
  return idle;
}
