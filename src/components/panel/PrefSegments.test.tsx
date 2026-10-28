import { afterEach, describe, expect, it, vi } from 'vitest';
import { cleanup, fireEvent, render, screen } from '@testing-library/react';
import { useState } from 'react';
import PrefSegments from './PrefSegments';

afterEach(cleanup);

function Host({ changed }: { changed(value: string): void }) {
  const [value, setValue] = useState<'mouse' | 'trackpad'>('mouse');
  return (
    <PrefSegments
      label="Input device"
      options={[
        { value: 'mouse', label: 'Mouse' },
        { value: 'trackpad', label: 'Touchpad' },
      ]}
      value={value}
      onChange={(next) => {
        changed(next);
        setValue(next);
      }}
    />
  );
}

describe('PrefSegments', () => {
  it('is one Tab stop and the arrow keys move and choose, as a radio group', () => {
    const changed = vi.fn();
    render(<Host changed={changed} />);
    const mouse = screen.getByRole('radio', { name: 'Mouse' });
    const touchpad = screen.getByRole('radio', { name: 'Touchpad' });
    expect([mouse.tabIndex, touchpad.tabIndex]).toEqual([0, -1]);
    mouse.focus();
    fireEvent.keyDown(mouse, { key: 'ArrowRight' });
    expect(changed).toHaveBeenLastCalledWith('trackpad');
    expect(touchpad.getAttribute('aria-checked')).toBe('true');
    expect(document.activeElement).toBe(touchpad);
    expect([mouse.tabIndex, touchpad.tabIndex]).toEqual([-1, 0]);
    fireEvent.keyDown(touchpad, { key: 'ArrowRight' });
    expect(changed).toHaveBeenLastCalledWith('mouse');
    expect(document.activeElement).toBe(mouse);
  });
});
