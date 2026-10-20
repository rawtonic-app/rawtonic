import { afterEach, expect, it, vi } from 'vitest';
import { swallowDragClick } from './swallowClick';

afterEach(() => {
  vi.runOnlyPendingTimers();
  vi.useRealTimers();
});

it('swallows only the click immediately following a drag', () => {
  vi.useFakeTimers();
  const clicked = vi.fn();
  const target = document.createElement('button');
  document.body.append(target);
  target.addEventListener('click', clicked);
  swallowDragClick();
  const first = new MouseEvent('click', { bubbles: true, cancelable: true });
  target.dispatchEvent(first);
  expect(first.defaultPrevented).toBe(true);
  expect(clicked).not.toHaveBeenCalled();
  target.click();
  expect(clicked).toHaveBeenCalledOnce();
  target.remove();
});

it('can preserve the default click action', () => {
  vi.useFakeTimers();
  swallowDragClick(false);
  const click = new MouseEvent('click', { bubbles: true, cancelable: true });
  window.dispatchEvent(click);
  expect(click.defaultPrevented).toBe(false);
});

it('expires at the next task when no click arrives', () => {
  vi.useFakeTimers();
  swallowDragClick();
  vi.runOnlyPendingTimers();
  const later = new MouseEvent('click', { cancelable: true });
  window.dispatchEvent(later);
  expect(later.defaultPrevented).toBe(false);
});
