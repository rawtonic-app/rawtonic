import { render } from '@testing-library/react';
import { describe, expect, it } from 'vitest';
import { useFitOneRow } from './useFitOneRow';

/** A bar whose content is `widths[level]` wide in a 500 px bar. */
function Bar({ widths }: { widths: number[] }) {
  const ref = useFitOneRow(widths.length - 1);
  return (
    <div
      data-testid="bar"
      ref={(el) => {
        if (el && !Object.getOwnPropertyDescriptor(el, 'scrollWidth')) {
          Object.defineProperty(el, 'clientWidth', { get: () => 500 });
          Object.defineProperty(el, 'scrollWidth', { get: () => widths[Number(el.dataset.collapse ?? 0)] });
        }
        ref(el);
      }}
    />
  );
}

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
