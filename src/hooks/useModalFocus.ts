import { useEffect, type RefObject } from "react";

const FOCUSABLE = [
  "a[href]",
  "button:not([disabled])",
  "input:not([disabled]):not([type=hidden])",
  "select:not([disabled])",
  "textarea:not([disabled])",
  "summary",
  "[contenteditable=true]",
  "[tabindex]",
].join(",");

/** Tab stops inside `root`, in document order. */
export function tabStops(root: HTMLElement): HTMLElement[] {
  return Array.from(root.querySelectorAll<HTMLElement>(FOCUSABLE)).filter(
    (el) =>
      el.tabIndex >= 0 &&
      !el.closest("[inert]") &&
      // jsdom has no layout; real engines skip what is not rendered.
      (typeof el.checkVisibility !== "function" ||
        el.checkVisibility({ visibilityProperty: true })),
  );
}

// Layers that stay usable above a modal: notifications (the update notice
// too, which a manual check in Preferences brings up) and dialogs opened
// from inside it (a confirmation portalled to <body>).
const KEEP_LIVE = '.app-notices, .Toastify, [aria-modal="true"], dialog[open]';

/**
 * Makes `ref` a real modal while `open` (WAI-ARIA dialog pattern): the
 * focus moves into it, Tab and Shift+Tab cycle inside it, everything outside
 * it becomes `inert` and, on close, the focus returns to the control that
 * had it before. Escape stays with the caller (the shortcut handler closes
 * Preferences).
 */
export function useModalFocus(
  ref: RefObject<HTMLElement | null>,
  open: boolean,
) {
  useEffect(() => {
    const root = ref.current;
    if (!open || !root) return;
    const opener =
      document.activeElement instanceof HTMLElement ? document.activeElement : null;

    // Everything but the dialog's ancestors becomes inert, so neither the
    // pointer nor Tab nor assistive technology reaches the workspace behind.
    const madeInert: Element[] = [];
    for (let node: Element = root; node.parentElement; node = node.parentElement) {
      for (const sibling of Array.from(node.parentElement.children)) {
        if (
          sibling === node ||
          sibling.hasAttribute("inert") ||
          sibling.matches(`script, style, ${KEEP_LIVE}`)
        )
          continue;
        sibling.setAttribute("inert", "");
        madeInert.push(sibling);
      }
      if (node.parentElement === document.body) break;
    }

    if (!root.contains(document.activeElement)) {
      const first =
        root.querySelector<HTMLElement>("[autofocus], [data-autofocus]") ??
        tabStops(root)[0];
      (first ?? root).focus({ preventScroll: true });
    }

    const onKeyDown = (event: KeyboardEvent) => {
      if (event.key !== "Tab" || event.defaultPrevented) return;
      const stops = tabStops(root);
      const active = document.activeElement;
      if (!stops.length) {
        event.preventDefault();
        root.focus({ preventScroll: true });
        return;
      }
      const first = stops[0];
      const last = stops[stops.length - 1];
      const inside = active instanceof Node && root.contains(active);
      if (event.shiftKey && (!inside || active === first || active === root)) {
        event.preventDefault();
        last.focus();
      } else if (!event.shiftKey && (!inside || active === last)) {
        event.preventDefault();
        first.focus();
      }
    };
    root.addEventListener("keydown", onKeyDown);

    return () => {
      root.removeEventListener("keydown", onKeyDown);
      for (const el of madeInert) el.removeAttribute("inert");
      if (opener?.isConnected && !opener.closest("[inert]")) {
        opener.focus({ preventScroll: true });
      }
    };
  }, [ref, open]);
}
