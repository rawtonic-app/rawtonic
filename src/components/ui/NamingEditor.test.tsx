import '../../i18n';
import { useState } from 'react';
import { cleanup, fireEvent, render, screen, waitFor, within } from '@testing-library/react';
import { afterEach, beforeEach, expect, it, vi } from 'vitest';
import NamingEditor from './NamingEditor';
import type { NamingKind, NamingMode } from '../../utils/namingTemplates';

let current = '';
function Editor({
  initial = '{orig_base}',
  mode = 'export',
  kind = 'file'
}: {
  initial?: string;
  mode?: NamingMode;
  kind?: NamingKind;
}) {
  const [value, set] = useState(initial);
  const [sequenceStart, setSequenceStart] = useState(1);
  current = value;
  return (
    <NamingEditor
      mode={mode}
      kind={kind}
      label="Filename template"
      value={value}
      onChange={set}
      original="photo.RAF"
      job="Acme"
      sequenceStart={sequenceStart}
      onSequenceStartChange={kind === 'file' ? setSequenceStart : undefined}
    />
  );
}
beforeEach(() => localStorage.clear());
afterEach(cleanup);
const field = () => screen.getByRole('group', { name: 'Naming blocks' });
const chipNames = () =>
  within(field())
    .queryAllByRole('button')
    .filter((b) => b.classList.contains('naming-chip-label'))
    .map((b) => b.textContent);
const texts = () => within(field()).getAllByRole('textbox') as HTMLInputElement[];

it('shows tokens as chips with editable free text between them', () => {
  render(<Editor initial="Shoot_{date}-{seq:3}.x" />);
  expect(chipNames()).toEqual(['Date', 'Counter']);
  expect(texts().map((t) => t.value)).toEqual(['Shoot_', '-', '.x']);
  fireEvent.change(texts()[1], { target: { value: ' · ' } });
  expect(current).toBe('Shoot_{date} · {seq:3}.x');
  expect(screen.getByText('Shoot_2026-10-11 · 001.x.jpg')).toBeTruthy();
});

const openTokens = () => fireEvent.click(screen.getByRole('button', { name: 'Add a naming token' }));

it.each([
  { top: 100, bottom: 220, above: false },
  { top: 400, bottom: 520, above: true }
])('places the token list outside a wrapped field at $top–$bottom', ({ top, bottom, above }) => {
  vi.stubGlobal('innerHeight', 700);
  try {
    render(<Editor initial="{orig_base}_{date}_{seq}_{lens}" />);
    const toggle = screen.getByRole('button', { name: 'Add a naming token' });
    vi.spyOn(toggle.parentElement!, 'getBoundingClientRect').mockReturnValue(
      DOMRect.fromRect({ y: top, height: bottom - top })
    );
    openTokens();
    const popup = screen.getByRole('dialog', { name: 'Add a naming token' });
    expect(popup.style.top).toBe(above ? '' : 'calc(100% + 4px)');
    expect(popup.style.bottom).toBe(above ? 'calc(100% + 4px)' : '');
  } finally {
    vi.unstubAllGlobals();
  }
});

it('keeps the token search usable when neither side has room', () => {
  vi.stubGlobal('innerHeight', 100);
  try {
    render(<Editor />);
    const toggle = screen.getByRole('button', { name: 'Add a naming token' });
    vi.spyOn(toggle.parentElement!, 'getBoundingClientRect').mockReturnValue(DOMRect.fromRect({ y: 0, height: 100 }));
    openTokens();
    const popup = screen.getByRole('dialog', { name: 'Add a naming token' });
    expect(parseFloat(popup.style.maxHeight)).toBeGreaterThanOrEqual(80);
  } finally {
    vi.unstubAllGlobals();
  }
});

it('adds a token by double-click or Enter at the text cursor, not by a single click', () => {
  render(<Editor initial="ab" />);
  const text = texts()[0];
  fireEvent.focus(text);
  text.setSelectionRange(1, 1);
  fireEvent.select(text);
  openTokens();
  const year = screen.getByRole('button', { name: 'Year' });
  fireEvent.click(year);
  expect(current).toBe('ab');
  fireEvent.doubleClick(year);
  expect(current).toBe('a{yyyy}b');
  // The list stays open and the next token follows the one just added.
  fireEvent.keyDown(screen.getByRole('button', { name: 'Lens' }), { key: 'Enter' });
  expect(current).toBe('a{yyyy}{lens}b');
});

it('finds tokens by name in the token list; Enter adds the first match, Escape closes it', () => {
  render(<Editor initial="" />);
  openTokens();
  const search = screen.getByRole('searchbox', { name: 'Search tokens' });
  fireEvent.change(search, { target: { value: 'iso' } });
  const list = screen.getByRole('dialog', { name: 'Add a naming token' });
  expect(
    within(list)
      .getAllByRole('button')
      .map((b) => b.getAttribute('aria-label'))
  ).toEqual(['ISO']);
  fireEvent.keyDown(search, { key: 'Enter' });
  expect(current).toBe('{iso}');
  fireEvent.change(search, { target: { value: 'zzz' } });
  expect(within(list).getByText('No matching tokens')).toBeTruthy();
  fireEvent.keyDown(search, { key: 'Escape' });
  expect(screen.queryByRole('dialog', { name: 'Add a naming token' })).toBeNull();
  expect(document.activeElement).toBe(screen.getByRole('button', { name: 'Add a naming token' }));
});

it('moves through the token list with the arrow keys', () => {
  render(<Editor initial="" mode="ingest" />);
  openTokens();
  const search = screen.getByRole('searchbox', { name: 'Search tokens' });
  fireEvent.keyDown(search, { key: 'ArrowDown' });
  const first = document.activeElement as HTMLElement;
  expect(first.classList.contains('naming-token-item')).toBe(true);
  fireEvent.keyDown(first, { key: 'ArrowDown' });
  expect(document.activeElement).not.toBe(first);
  fireEvent.keyDown(document.activeElement as HTMLElement, { key: 'ArrowUp' });
  expect(document.activeElement).toBe(first);
  fireEvent.keyDown(first, { key: 'ArrowUp' });
  expect(document.activeElement).toBe(search);
});

it('removes and moves chips from the keyboard', () => {
  render(<Editor initial="{yyyy}_{orig_base}" />);
  const [year] = within(field())
    .getAllByRole('button')
    .filter((b) => b.classList.contains('naming-chip-label'));
  fireEvent.keyDown(year, { key: 'ArrowRight', altKey: true });
  expect(current).toBe('{orig_base}_{yyyy}');
  // Backspace at the start of the text after a chip removes the chip.
  const after = texts()[2];
  after.setSelectionRange(0, 0);
  fireEvent.keyDown(after, { key: 'Backspace' });
  expect(current).toBe('{orig_base}_');
  fireEvent.click(screen.getByRole('button', { name: 'Remove Photo name' }));
  expect(current).toBe('_');
});

it('chooses counter digits and date formats on their chips', () => {
  render(<Editor initial="{date}_{seq}" />);
  fireEvent.change(within(field()).getByLabelText('Date format'), { target: { value: 'dd.mm.yyyy' } });
  expect(current).toBe('{date:dd.mm.yyyy}_{seq}');
  fireEvent.change(within(field()).getByLabelText('Counter digits'), { target: { value: '4' } });
  expect(current).toBe('{date:dd.mm.yyyy}_{seq:4}');
  expect(screen.getByText('11.10.2026_0001.jpg')).toBeTruthy();
  fireEvent.change(screen.getByLabelText('Counter starts at'), { target: { value: '42' } });
  expect(screen.getByText('11.10.2026_0042.jpg')).toBeTruthy();
});

it("shows the backend's name for the selected photo when it has one", () => {
  render(
    <NamingEditor
      mode="ingest"
      label="Filename template"
      value="{camera}_{seq:4}"
      onChange={() => {}}
      preview={{ name: 'Nikon D750_0001.NEF', dateSource: 'original', sidecar: false }}
    />
  );
  expect(screen.getByText('Nikon D750_0001.NEF')).toBeTruthy();
  expect(screen.getByText('Date from the capture time (EXIF)')).toBeTruthy();
});

it('saves, renames and deletes presets, shared by import, rename and export', () => {
  const view = render(<Editor />);
  fireEvent.change(screen.getByLabelText('Filename template'), { target: { value: 'Web_{orig_base}' } });
  fireEvent.click(screen.getByRole('button', { name: 'Save as preset…' }));
  fireEvent.change(screen.getByLabelText('Preset name'), { target: { value: 'Website' } });
  fireEvent.click(screen.getByRole('button', { name: 'Save preset' }));
  view.unmount();
  // The rename dialog offers it too.
  const rename = render(<Editor mode="rename" />);
  fireEvent.change(screen.getByLabelText('Naming preset'), { target: { value: 'saved:0' } });
  expect(current).toBe('Web_{orig_base}');
  fireEvent.click(screen.getByRole('button', { name: 'Save as preset…' }));
  fireEvent.change(screen.getByLabelText('Preset name'), { target: { value: 'Web delivery' } });
  fireEvent.click(screen.getByRole('button', { name: 'Rename preset' }));
  expect(screen.getByRole('option', { name: 'Web delivery' })).toBeTruthy();
  fireEvent.click(screen.getByRole('button', { name: 'Delete preset' }));
  expect(screen.queryByRole('option', { name: 'Web delivery' })).toBeNull();
  rename.unmount();
});

it('hides a preset whose tokens a mode cannot fill', () => {
  localStorage.setItem(
    'rawtonic.naming.presets',
    JSON.stringify([{ name: 'Per format', template: '{orig_base}_{format}' }])
  );
  render(<Editor mode="ingest" />);
  expect(screen.queryByRole('option', { name: 'Per format' })).toBeNull();
});

it('shows invalid tokens inline and prevents form submission', () => {
  render(<Editor />);
  const input = screen.getByLabelText('Filename template') as HTMLInputElement;
  fireEvent.change(input, { target: { value: '{unknown}' } });
  expect(input.checkValidity()).toBe(false);
  expect(screen.getByRole('alert').textContent).toContain('available tokens');
  fireEvent.change(input, { target: { value: '{orig_base}' } });
  expect(input.checkValidity()).toBe(true);
});

it('edits a folder template with folder levels and no presets', () => {
  render(<Editor kind="folder" initial="{job}/{date}/{format}" />);
  expect(screen.queryByLabelText('Naming preset')).toBeNull();
  expect(screen.getByText('Acme/2026-10-11/JPEG')).toBeTruthy();
  openTokens();
  fireEvent.doubleClick(screen.getByRole('button', { name: 'Folder level (/)' }));
  expect(current).toBe('{job}/{date}/{format}/');
});

it('keeps the separator before the counter when custom text replaces the project name', () => {
  render(<Editor initial="_{seq:4}" mode="ingest" />);
  fireEvent.change(texts()[0], { target: { value: 'W' } });
  expect(current).toBe('W_{seq:4}');
  fireEvent.change(texts()[0], { target: { value: 'Wedding_' } });
  expect(current).toBe('Wedding_{seq:4}');
  // the user can still remove it
  fireEvent.change(texts()[0], { target: { value: 'Wedding' } });
  expect(current).toBe('Wedding{seq:4}');
});

it('types into an empty field after a click on it, for file names and subfolders', async () => {
  for (const kind of ['file', 'folder'] as const) {
    const view = render(<Editor initial="" kind={kind} />);
    fireEvent.mouseDown(field());
    await waitFor(() => expect(document.activeElement).toBe(texts()[0]));
    fireEvent.change(texts()[0], { target: { value: 'Client' } });
    expect(current).toBe('Client');
    view.unmount();
  }
});

it('types after a field holding only a token, where a click lands at its end', async () => {
  render(<Editor initial="{orig_base}" />);
  fireEvent.mouseDown(field());
  const last = texts()[texts().length - 1];
  await waitFor(() => expect(document.activeElement).toBe(last));
  fireEvent.change(last, { target: { value: '_web' } });
  expect(current).toBe('{orig_base}_web');
});

it('switches a chosen template to Custom format when its content is edited', () => {
  render(<Editor initial="" />);
  const preset = screen.getByLabelText('Naming preset') as HTMLSelectElement;
  fireEvent.change(preset, { target: { value: 'builtin:0' } });
  expect(current).toBe('{orig_base}');
  expect(preset.value).toBe('builtin:0');
  fireEvent.change(texts()[1], { target: { value: '_edit' } });
  expect(current).toBe('{orig_base}_edit');
  expect(preset.value).toBe('');
  expect(preset.selectedOptions[0].textContent).toBe('Custom format');
});

it('drops a token dragged from the token list into the field where it is let go', () => {
  // dnd-kit measures elements: each free text slot of the field gets its own
  // 100 px column, everything else sits far below.
  const rect = Element.prototype.getBoundingClientRect;
  Element.prototype.getBoundingClientRect = function (this: Element) {
    const slot = this.getAttribute('data-drop-index');
    const left = slot === null ? 0 : Number(slot) * 100;
    const top = slot === null ? 500 : 0;
    return { left, top, right: left + 90, bottom: top + 30, width: 90, height: 30, x: left, y: top, toJSON() {} };
  };
  try {
    render(<Editor initial="a{yyyy}b" />);
    openTokens();
    const iso = screen.getByRole('button', { name: 'ISO' });
    fireEvent.pointerDown(iso, { button: 0, isPrimary: true, clientX: 10, clientY: 510 });
    fireEvent.pointerMove(document, { clientX: 10, clientY: 400 });
    fireEvent.pointerMove(document, { clientX: 150, clientY: 10 });
    fireEvent.pointerUp(document, { clientX: 150, clientY: 10 });
    // Slot 1 is the text after the year: the token lands at its end.
    expect(current).toBe('a{yyyy}b{iso}');
  } finally {
    Element.prototype.getBoundingClientRect = rect;
  }
});

it('keeps the format to edit when Custom format is chosen, with the cursor at its end', async () => {
  render(<Editor initial="{orig_base}" />);
  const preset = screen.getByLabelText('Naming preset') as HTMLSelectElement;
  expect(preset.value).toBe('builtin:0');
  fireEvent.change(preset, { target: { value: '' } });
  expect(preset.value).toBe('');
  expect(current).toBe('{orig_base}');
  await waitFor(() => expect(document.activeElement).toBe(texts()[1]));
  fireEvent.change(preset, { target: { value: 'builtin:1' } });
  expect(current).toBe('{date:yyyymmdd}_{orig_base}');
  expect(preset.value).toBe('builtin:1');
});

it('closes the token list with Escape without letting the import dialog see it', () => {
  render(<Editor initial="" />);
  openTokens();
  const popover = screen.getByRole('dialog', { name: 'Add a naming token' });
  expect(popover.hasAttribute('data-escape-local')).toBe(true);
});

it('closes the save-preset panel with Escape from its buttons too, keeping the dialog', () => {
  const outer = vi.fn();
  render(
    <div onKeyDown={(e) => e.key === 'Escape' && outer()}>
      <Editor />
    </div>
  );
  const toggle = screen.getByRole('button', { name: 'Save as preset…' });
  fireEvent.click(toggle);
  fireEvent.change(screen.getByLabelText('Preset name'), { target: { value: 'Web' } });
  const save = screen.getByRole('button', { name: 'Save preset' });
  expect(save.closest('[data-escape-local]')).toBeTruthy();
  fireEvent.keyDown(save, { key: 'Escape' });
  expect(screen.queryByLabelText('Preset name')).toBeNull();
  expect(outer).not.toHaveBeenCalled();
  expect(document.activeElement).toBe(toggle);
});
