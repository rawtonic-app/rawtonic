import { useEffect, useRef, type MouseEvent, type PointerEvent } from 'react';
import { endToolBefore, startToolBefore } from './useEditorActions';

/** How long a plain press on a tool's reset is held before it shows the
 * photograph without the tool instead of resetting it on release. */
export const TOOL_BEFORE_HOLD_MS = 350;

/**
 * Per-tool before on a tool's reset button (Capture One's ⌥-held reset):
 * Alt/Option + press shows the photograph without the tool at once, a plain
 * press held for TOOL_BEFORE_HOLD_MS too; the release brings the edit back.
 * Either way that press does not reset. A short plain click resets as before.
 * Nothing is written: no recipe change, no history step.
 */
export function useToolBeforeHold(tool: string, onReset: (() => void) | undefined) {
  const timer = useRef<ReturnType<typeof setTimeout> | null>(null);
  const showing = useRef(false);
  // The press showed the before: its click must not reset.
  const swallowClick = useRef(false);
  const resetRef = useRef(onReset);
  resetRef.current = onReset;

  const clearTimer = () => {
    if (timer.current !== null) clearTimeout(timer.current);
    timer.current = null;
  };
  const begin = () => {
    clearTimer();
    swallowClick.current = true;
    const reset = resetRef.current;
    if (reset && startToolBefore(tool, reset)) showing.current = true;
  };
  const end = () => {
    clearTimer();
    if (!showing.current) return;
    showing.current = false;
    endToolBefore();
  };

  // A focus loss (another window, a dialog) ends it, as releasing would.
  useEffect(() => {
    const onBlur = () => end();
    window.addEventListener('blur', onBlur);
    return () => {
      window.removeEventListener('blur', onBlur);
      end();
    };
    // eslint-disable-next-line react-hooks/exhaustive-deps -- end reads refs only
  }, []);

  return {
    onPointerDown: (event: PointerEvent<HTMLElement>) => {
      if (event.button !== 0 || !resetRef.current) return;
      swallowClick.current = false;
      // The release outside the button still ends it.
      try {
        event.currentTarget.setPointerCapture?.(event.pointerId);
      } catch {
        /* No active pointer (a synthetic press): pointerup still ends it. */
      }
      if (event.altKey) begin();
      else timer.current = setTimeout(begin, TOOL_BEFORE_HOLD_MS);
    },
    onPointerUp: end,
    onPointerCancel: end,
    onLostPointerCapture: end,
    /** The button's click: resets unless the press showed the before. */
    onClick: (event: MouseEvent<HTMLElement>) => {
      event.preventDefault();
      // Not the header's own click (⌥-click there opens this tool alone).
      event.stopPropagation();
      if (swallowClick.current) {
        swallowClick.current = false;
        return;
      }
      resetRef.current?.();
    },
  };
}
