import { afterEach, expect, it, vi } from 'vitest';
import { cleanup, render, screen } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { useState } from 'react';
import Dropdown, { type OptionItem } from './Dropdown';

afterEach(cleanup);

const OPTIONS: Array<OptionItem<string>> = [
  { label: 'Apple', value: 'apple' },
  { label: 'Banana', value: 'banana', disabled: true },
  { label: 'Cherry', value: 'cherry' },
  { label: 'Date', value: 'date' },
];

function Harness({ initial = null, onChange }: { initial?: string | null; onChange?: (v: string) => void }) {
  const [value, setValue] = useState<string | null>(initial);
  return (
    <Dropdown
      options={OPTIONS}
      value={value}
      onChange={(v) => {
        setValue(v);
        onChange?.(v);
      }}
    />
  );
}

const trigger = () => document.querySelector<HTMLButtonElement>('button[role="combobox"]')!;
const isOpen = () => trigger().getAttribute('aria-expanded') === 'true';
const activeOption = () => {
  const id = trigger().getAttribute('aria-activedescendant');
  return id ? document.getElementById(id) : null;
};

it('opens with ArrowDown on the selected option and announces the listbox', async () => {
  const user = userEvent.setup();
  render(<Harness initial="cherry" />);
  expect(trigger().getAttribute('aria-haspopup')).toBe('listbox');
  expect(trigger().getAttribute('aria-expanded')).toBe('false');
  trigger().focus();
  await user.keyboard('{ArrowDown}');
  expect(trigger().getAttribute('aria-expanded')).toBe('true');
  const listbox = screen.getByRole('listbox');
  expect(trigger().getAttribute('aria-controls')).toBe(listbox.id);
  expect(activeOption()?.textContent).toBe('Cherry');
  expect(activeOption()?.getAttribute('aria-selected')).toBe('true');
  // Options are not tab stops; the focus stays on the trigger.
  for (const option of screen.getAllByRole('option')) expect(option.tabIndex).toBe(-1);
  expect(document.activeElement).toBe(trigger());
});

it('moves the active option with the arrow keys, Home and End, skipping disabled options', async () => {
  const user = userEvent.setup();
  render(<Harness />);
  trigger().focus();
  await user.keyboard('{ArrowDown}');
  expect(activeOption()?.textContent).toBe('Apple');
  await user.keyboard('{ArrowDown}');
  expect(activeOption()?.textContent).toBe('Cherry');
  await user.keyboard('{ArrowDown}{ArrowDown}');
  // The end of the list does not wrap.
  expect(activeOption()?.textContent).toBe('Date');
  await user.keyboard('{Home}');
  expect(activeOption()?.textContent).toBe('Apple');
  await user.keyboard('{End}{ArrowUp}{ArrowUp}');
  expect(activeOption()?.textContent).toBe('Apple');
  expect(screen.getByRole('option', { name: 'Banana' }).getAttribute('aria-disabled')).toBe('true');
});

it('chooses the active option with Enter and with Space, then returns focus to the trigger', async () => {
  const user = userEvent.setup();
  const onChange = vi.fn();
  render(<Harness onChange={onChange} />);
  trigger().focus();
  await user.keyboard('{ArrowDown}{ArrowDown}{Enter}');
  expect(onChange).toHaveBeenLastCalledWith('cherry');
  expect(isOpen()).toBe(false);
  expect(trigger().textContent).toBe('Cherry');
  expect(document.activeElement).toBe(trigger());

  await user.keyboard('{ArrowDown}{ArrowDown}');
  expect(activeOption()?.textContent).toBe('Date');
  await user.keyboard(' ');
  expect(onChange).toHaveBeenLastCalledWith('date');
  expect(isOpen()).toBe(false);
  expect(document.activeElement).toBe(trigger());
});

it('closes with Escape without letting the key reach the dialog around it', async () => {
  const user = userEvent.setup();
  const outer = vi.fn();
  render(
    <div onKeyDown={(e) => e.key === 'Escape' && outer()}>
      <Harness initial="apple" />
    </div>,
  );
  trigger().focus();
  await user.keyboard('{ArrowDown}');
  await user.keyboard('{Escape}');
  expect(isOpen()).toBe(false);
  expect(document.activeElement).toBe(trigger());
  expect(outer).not.toHaveBeenCalled();
  // Closed, Escape belongs to the surrounding dialog again.
  await user.keyboard('{Escape}');
  expect(outer).toHaveBeenCalledTimes(1);
});

it('keeps typeahead: typing filters the list and Enter chooses the first match', async () => {
  const user = userEvent.setup();
  const onChange = vi.fn();
  render(<Harness onChange={onChange} />);
  trigger().focus();
  await user.keyboard('d');
  const search = screen.getByRole('combobox', { name: 'Filter options...' });
  expect(document.activeElement).toBe(search);
  expect((search as HTMLInputElement).value).toBe('d');
  // "Date" is the only label containing "d".
  expect(screen.getAllByRole('option').map((o) => o.textContent)).toEqual(['Date']);
  expect(document.getElementById(search.getAttribute('aria-activedescendant') ?? '')?.textContent).toBe('Date');
  await user.keyboard('{Enter}');
  expect(onChange).toHaveBeenLastCalledWith('date');
  expect(document.activeElement).toBe(trigger());
});

it('chooses with the mouse and ignores disabled options', async () => {
  const user = userEvent.setup();
  const onChange = vi.fn();
  render(<Harness onChange={onChange} />);
  await user.click(trigger());
  await user.click(screen.getByRole('option', { name: 'Banana' }));
  expect(onChange).not.toHaveBeenCalled();
  await user.click(screen.getByRole('option', { name: 'Apple' }));
  expect(onChange).toHaveBeenLastCalledWith('apple');
  expect(isOpen()).toBe(false);
});
