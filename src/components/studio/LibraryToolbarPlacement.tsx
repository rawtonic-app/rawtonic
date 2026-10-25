import { useLayoutEffect, useRef, useState, type ReactNode } from "react";
import { createPortal } from "react-dom";

/** Keep one React tree while moving the toolbar between its two DOM slots.
 * appendChild blurs focused descendants on WebKit, so retain keyboard focus
 * and the search selection around the move as well. */
export default function LibraryToolbarPlacement({ header, compact, children }: {
  header: HTMLElement | null;
  compact: boolean;
  children: ReactNode;
}) {
  const inline = useRef<HTMLDivElement>(null);
  const [host] = useState(() => {
    const element = document.createElement("div");
    element.style.display = "contents";
    return element;
  });
  useLayoutEffect(() => {
    const target = compact ? header : inline.current;
    if (!target || host.parentElement === target) return;
    const focused = document.activeElement;
    const restore = focused instanceof HTMLElement && host.contains(focused);
    const input = focused instanceof HTMLInputElement ? focused : null;
    const selection = input && input.selectionStart !== null
      ? [input.selectionStart, input.selectionEnd, input.selectionDirection ?? "none"] as const : null;
    target.appendChild(host);
    if (restore) {
      focused.focus({ preventScroll: true });
      if (input && selection) input.setSelectionRange(...selection);
    }
  }, [compact, header, host]);
  useLayoutEffect(() => () => host.remove(), [host]);
  return <div ref={inline} style={{ display: "contents" }}>{createPortal(children, host)}</div>;
}
