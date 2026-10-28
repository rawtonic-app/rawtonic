import '../../i18n';
import { afterEach, expect, it, vi } from 'vitest';
import { cleanup, render, screen } from '@testing-library/react';
import SettingItem from './SettingItem';
import Dropdown from '../ui/Dropdown';
import Switch from '../ui/Switch';
import Slider from '../ui/Slider';

afterEach(cleanup);

const description = (el: HTMLElement) =>
  (el.getAttribute('aria-describedby') ?? '')
    .split(' ')
    .map((id) => document.getElementById(id)?.textContent)
    .join(' ');

it('names a dropdown by its setting, not by its value, and describes it', () => {
  render(
    <SettingItem label="Interface size" description="How large the tool panels are drawn.">
      <Dropdown options={[{ label: '100%', value: 1 }]} value={1} onChange={vi.fn()} />
    </SettingItem>,
  );
  const combobox = screen.getByRole('combobox', { name: 'Interface size' });
  expect(description(combobox)).toBe('How large the tool panels are drawn.');
});

it('keeps a switch its own label and describes it with the setting', () => {
  render(
    <SettingItem label="XMP sync" description="Writes edits next to the photographs.">
      <Switch label="Enable XMP sync" checked={false} onChange={vi.fn()} />
    </SettingItem>,
  );
  const toggle = screen.getByRole('checkbox', { name: 'Enable XMP sync' });
  expect(description(toggle)).toBe('XMP sync Writes edits next to the photographs.');
});

it('names a slider by its setting, not by its generic label, and describes it', () => {
  render(
    <SettingItem label="Maximum AI tags" description="How many tags are added per photograph.">
      <Slider label="Amount" value={5} min={1} max={20} step={1} onChange={vi.fn()} />
    </SettingItem>,
  );
  const slider = screen.getByRole('slider', { name: 'Maximum AI tags' });
  expect(description(slider)).toBe('How many tags are added per photograph.');
});

it('names a slider outside a preference row by its own label', () => {
  render(<Slider label="Amount" value={5} min={1} max={20} step={1} onChange={vi.fn()} />);
  expect(screen.getByRole('slider', { name: 'Amount' })).toBeTruthy();
});

it('leaves a dropdown outside a preference row as before', () => {
  render(<Dropdown options={[{ label: 'Dark', value: 'dark' }]} value="dark" onChange={vi.fn()} />);
  expect(screen.getByRole('combobox').hasAttribute('aria-labelledby')).toBe(false);
});
