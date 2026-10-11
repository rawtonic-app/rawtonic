import { cleanup, renderHook } from '@testing-library/react';
import { afterEach, expect, it, vi } from 'vitest';
import { useEscapeToClose } from './useEscapeToClose';

const escape = () => {
  const event = new KeyboardEvent('keydown', { key: 'Escape', bubbles: true, cancelable: true });
  window.dispatchEvent(event);
  return event;
};
afterEach(cleanup);

it('closes an open dialog on Escape and consumes the key', () => {
  const onClose = vi.fn();
  renderHook(() => useEscapeToClose(true, onClose));
  expect(escape().defaultPrevented).toBe(true);
  expect(onClose).toHaveBeenCalledOnce();
});

it('ignores Escape while closed or when another handler consumed it', () => {
  const onClose = vi.fn();
  const { rerender } = renderHook(({ open }) => useEscapeToClose(open, onClose), { initialProps: { open: false } });
  escape();
  rerender({ open: true });
  const consume = (event: KeyboardEvent) => event.preventDefault();
  window.addEventListener('keydown', consume, { capture: true });
  escape();
  window.removeEventListener('keydown', consume, { capture: true });
  expect(onClose).not.toHaveBeenCalled();
});
