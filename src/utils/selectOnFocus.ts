import type { FocusEvent, MouseEvent } from 'react';

// Inputs pressed while they did not have focus: the mouse-up of that click
// must not place a caret.
const clickFocusing = new WeakSet<EventTarget>();

/**
 * Props for a value field whose value is selected when it gets focus, by
 * click as well as by Tab (Lightroom's value fields), so typing replaces the
 * value without ⌘A first. Selecting on focus alone is not enough: WebKit
 * collapses the selection to a caret on the mouse-up of the focusing click
 * (notes: reference/wp7-use-test-c1-lr.md finding 1). A later click into the
 * focused field still places the caret.
 */
export const selectOnFocus = {
  onMouseDown(e: MouseEvent<HTMLInputElement>) {
    if (document.activeElement !== e.currentTarget) clickFocusing.add(e.currentTarget);
  },
  onFocus(e: FocusEvent<HTMLInputElement>) {
    e.currentTarget.select();
  },
  onMouseUp(e: MouseEvent<HTMLInputElement>) {
    if (clickFocusing.delete(e.currentTarget)) e.preventDefault();
  },
};
