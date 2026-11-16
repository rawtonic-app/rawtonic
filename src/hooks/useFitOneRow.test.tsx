import { cleanup, render } from '@testing-library/react';
import { afterEach, describe, expect, it } from 'vitest';
import { useFitOneRow } from './useFitOneRow';

/** A bar whose content is `widths[level]` wide in a 500 px bar. */
function Bar({ widths, attach = true }: { widths: number[]; attach?: boolean }) {
  const ref = useFitOneRow(widths.length - 1);
  return (
    <div
      data-testid="bar"
      ref={(el) => {
        if (el && !Object.getOwnPropertyDescriptor(el, 'scrollWidth')) {
          Object.defineProperty(el, 'clientWidth', { get: () => 500 });
          Object.defineProperty(el, 'scrollWidth', { get: () => widths[Number(el.dataset.collapse ?? 0)] });
        }
        ref(attach ? el : null);
      }}
    />
  );
}

afterEach(cleanup);

describe('useFitOneRow', () => {
  it('takes the first level whose content fits the bar', () => {
    const { getByTestId } = render(<Bar widths={[700, 620, 480, 400]} />);
    expect(getByTestId('bar').dataset.collapse).toBe('2');
    expect(getByTestId('bar').dataset.wrap).toBeUndefined();
  });

  it('keeps every word while the bar is wide enough', () => {
    const { getByTestId } = render(<Bar widths={[500, 450]} />);
    expect(getByTestId('bar').dataset.collapse).toBe('0');
  });

  it('wraps rather than cutting a control off when no level fits', () => {
    const { getByTestId } = render(<Bar widths={[900, 800, 700]} />);
    expect(getByTestId('bar').dataset.collapse).toBe('2');
    expect(getByTestId('bar').dataset.wrap).toBe('true');
  });
});

it('leaves no level on a node it lets go of (React reuses it for another bar)', () => {
  const { getByTestId, rerender } = render(<Bar widths={[700, 600, 900]} />);
  const bar = getByTestId('bar');
  expect(bar.dataset.collapse).toBe('2');
  expect(bar.dataset.wrap).toBe('true');
  // Same node, ref released (Develop's bar becoming the loupe's).
  rerender(<Bar widths={[700, 600, 900]} attach={false} />);
  expect(getByTestId('bar')).toBe(bar);
  expect(bar.dataset.collapse).toBeUndefined();
  expect(bar.dataset.wrap).toBeUndefined();
});
