import { act, cleanup, render, screen, waitFor } from '@testing-library/react';
import { afterEach, expect, it, vi } from 'vitest';
import { ContextMenuProvider, useContextMenu } from './ContextMenuContext';
import { OPTION_HEADER } from '../components/ui/AppProperties';

afterEach(() => {
  cleanup();
  document.querySelectorAll('dialog').forEach((dialog) => dialog.remove());
  vi.unstubAllGlobals();
});

let show: ReturnType<typeof useContextMenu>['showContextMenu'] = () => {};
function Opener() {
  show = useContextMenu().showContextMenu;
  return null;
}

it('renders the menu inside an open modal dialog so it stays on top and usable', () => {
  const dialog = document.createElement('dialog');
  document.body.append(dialog);
  dialog.setAttribute('open', '');
  render(
    <ContextMenuProvider>
      <Opener />
    </ContextMenuProvider>,
  );
  act(() => show(10, 10, [{ label: 'Add Radial', onClick: () => {} }]));
  expect(dialog.contains(screen.getByRole('menu'))).toBe(true);
});

it('keeps a long menu inside the window so its first entries stay reachable', () => {
  vi.stubGlobal('innerHeight', 600);
  render(
    <ContextMenuProvider>
      <Opener />
    </ContextMenuProvider>,
  );
  const options = Array.from({ length: 30 }, (_, i) => ({ label: `Entry ${i}`, onClick: () => {} }));
  act(() => show(10, 170, options));
  const top = parseFloat((screen.getByRole('menu').parentElement as HTMLElement).style.top);
  expect(top).toBeGreaterThanOrEqual(5);
});

it('renders the menu in the document body without a dialog', () => {
  render(
    <ContextMenuProvider>
      <Opener />
    </ContextMenuProvider>,
  );
  act(() => show(10, 10, [{ label: 'Add Radial', onClick: () => {} }]));
  const menu = screen.getByRole('menu');
  expect(menu.closest('dialog')).toBeNull();
  expect(document.body.contains(menu)).toBe(true);
});

it('closes on Escape without passing the key on to the app shortcuts', async () => {
  render(
    <ContextMenuProvider>
      <Opener />
    </ContextMenuProvider>,
  );
  act(() => show(10, 10, [{ label: 'Add Radial', onClick: () => {} }]));
  let reachedApp = false;
  const appShortcuts = () => (reachedApp = true);
  window.addEventListener('keydown', appShortcuts);
  act(() => {
    document.body.dispatchEvent(new KeyboardEvent('keydown', { key: 'Escape', code: 'Escape', bubbles: true }));
  });
  window.removeEventListener('keydown', appShortcuts);
  expect(reachedApp).toBe(false);
  await waitFor(() => expect(screen.queryByRole('menu')).toBeNull());
});

it('opens a submenu on a click as well as on hover', async () => {
  vi.stubGlobal(
    'ResizeObserver',
    class {
      observe() {}
      disconnect() {}
    },
  );
  let built = false;
  render(
    <ContextMenuProvider>
      <Opener />
    </ContextMenuProvider>,
  );
  act(() =>
    show(10, 10, [
      { label: 'Previews', submenu: [{ label: 'Build Standard Previews', onClick: () => (built = true) }] },
    ]),
  );
  expect(screen.queryByText('Build Standard Previews')).toBeNull();
  act(() => screen.getByText('Previews').click());
  act(() => screen.getByText('Build Standard Previews').click());
  expect(built).toBe(true);
  await waitFor(() => expect(screen.queryByRole('menu')).toBeNull());
});

it('shows group captions that are not selectable entries', () => {
  render(
    <ContextMenuProvider>
      <Opener />
    </ContextMenuProvider>,
  );
  act(() =>
    show(10, 10, [
      { type: OPTION_HEADER, label: 'Draw' },
      { label: 'Brush', onClick: () => {} },
    ]),
  );
  const menu = screen.getByRole('menu');
  expect(screen.getAllByRole('menuitem').map((item) => item.textContent)).toEqual(['Brush']);
  expect(menu.querySelector('.rawtonic-menu-group')?.textContent).toBe('Draw');
});
