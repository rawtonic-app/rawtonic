import { useLayoutEffect, type RefObject } from "react";

/** Give a non-modal tool popover keyboard focus, then return it on dismissal.
 * An outside press may focus another control; leave that focus where it went. */
export function usePopoverFocus(ref: RefObject<HTMLDivElement | null>) {
  useLayoutEffect(() => {
    const panel = ref.current;
    if (!panel) return;
    const trigger = document.activeElement;
    // Skip buttons so Enter cannot activate Reset on opening. If there
    // are no fields, focus the panel itself.
    const first = panel.querySelector<HTMLElement>(
      'input:not(:disabled):not([type="hidden"]), select:not(:disabled), textarea:not(:disabled), [role="slider"][tabindex="0"]',
    );
    (first ?? panel).focus({ preventScroll: true });
    return () => {
      if (
        trigger instanceof HTMLElement && trigger.isConnected &&
        (panel.contains(document.activeElement) || document.activeElement === document.body)
      ) trigger.focus({ preventScroll: true });
    };
  }, [ref]);
}
