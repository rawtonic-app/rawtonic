import '../i18n';
import { afterEach, beforeEach, expect, it, vi } from 'vitest';
import { act, cleanup, render, screen, within } from '@testing-library/react';
import StudioWorkspace from '../components/studio/StudioWorkspace';
import { useEditorStore } from '../store/useEditorStore';
import { useLibraryStore } from '../store/useLibraryStore';
import { useUIStore } from '../store/useUIStore';
import { useSettingsStore } from '../store/useSettingsStore';
import { Panel, type AppSettings, type ImageFile, type SelectedImage } from '../components/ui/AppProperties';
import { ContextMenuProvider } from '../context/ContextMenuContext';
import { expectNoNewViolations, KNOWN, type Allowed } from '../utils/axeAudit.testSupport';

// Only the commands these views read answer; everything else resolves to
// nothing, as an idle backend would (a catch-all answer crashed Develop's
// white balance tool and the audit scanned its error fallback).
vi.mock('@tauri-apps/api/core', () => ({
  // The RAW's as-shot white balance, so the Kelvin/tint controls render too.
  invoke: vi.fn((command: string) =>
    Promise.resolve(command === 'white_balance_info' ? { temperature: 5200, tint: 4 } : undefined)
  ),
  isTauri: () => false,
  convertFileSrc: (path: string) => path
}));
vi.mock('@tauri-apps/plugin-dialog', () => ({ open: vi.fn(), save: vi.fn() }));
// The Develop canvas is native wgpu under the webview; nothing to audit in jsdom.
vi.mock('../components/panel/Editor', () => ({ default: () => null }));

const file = (path: string): ImageFile => ({
  path,
  is_edited: false,
  modified: 0,
  rating: 0,
  tags: null,
  exif: null,
  is_virtual_copy: false,
  is_cloud_placeholder: false,
  is_raw: true,
  group_id: null
});
const files = ['/s/a.NEF', '/s/b.NEF', '/s/c.NEF'].map(file);

beforeEach(() => {
  HTMLDialogElement.prototype.showModal = function () {
    this.setAttribute('open', '');
  };
  HTMLDialogElement.prototype.close = function () {
    this.removeAttribute('open');
  };
  localStorage.clear();
  vi.stubGlobal('PointerEvent', MouseEvent);
  vi.stubGlobal(
    'ResizeObserver',
    class {
      observe() {}
      disconnect() {}
    }
  );
  useSettingsStore.setState({ appSettings: { keybinds: {} } as unknown as AppSettings, osPlatform: 'macos' });
  useLibraryStore.setState({ multiSelectedPaths: ['/s/b.NEF'], imageRatings: {}, imageFlags: {} });
  useEditorStore.setState({
    selectedImage: { path: '/s/b.NEF', width: 6000, height: 4000, isRaw: true, isReady: true } as SelectedImage
  });
});
afterEach(() => {
  cleanup();
  vi.unstubAllGlobals();
});

const noop = () => {};
const workspace = () =>
  render(
    <StudioWorkspace
      files={files}
      flagCounts={{ all: files.length, picked: 0, rejected: 0, unflagged: files.length }}
      transformWrapperRef={{ current: null }}
      onSelect={noop}
      onFilmSelect={noop}
      onOpen={noop}
      onNavigate={noop}
      onBack={noop}
      onRate={noop}
      onContextMenu={noop}
      onEditorContextMenu={noop}
      onCopy={noop}
      onPaste={noop}
      onReset={noop}
      onZoom={noop}
      requestThumbnails={noop}
      settings={null}
      onSettings={noop}
    />,
    { wrapper: ContextMenuProvider }
  );

/** Every part rendered: no error boundary stood in for a crashed one. */
const expectRendered = () => expect(screen.queryAllByRole('alert').map((a) => a.textContent)).toEqual([]);

const LIBRARY_ALLOWED: Allowed[] = [KNOWN.toolHeader, KNOWN.gridTile];

it('Library workspace (grid) has no new accessibility violations', async () => {
  useUIStore.setState({ activeView: 'library' });
  workspace();
  await act(async () => {});
  expectRendered();
  await expectNoNewViolations('library grid', LIBRARY_ALLOWED, { page: true });
});

it('Develop workspace shell (bars, filmstrip, panels) has no new accessibility violations', async () => {
  useUIStore.setState({ activeView: 'editor', studioToolLayout: 'split-tools' });
  workspace();
  await act(async () => {});
  expectRendered();
  // The real Develop tools, not an error fallback.
  expect(screen.getByRole('navigation', { name: 'Adjustment tools' })).toBeTruthy();
  // The RAW white balance (Kelvin), not the relative fallback for files without one.
  document.querySelectorAll('details').forEach((group) => {
    group.open = true;
  });
  await screen.findByLabelText('White balance mode');
  await expectNoNewViolations('develop shell', LIBRARY_ALLOWED, { page: true });
});

it('Export dialog, opened from the workspace, has no new accessibility violations', async () => {
  useUIStore.setState({ activeView: 'library', activePanel: Panel.Adjustments });
  workspace();
  await act(async () => useUIStore.getState().setPanel(Panel.Export));
  await act(async () => {});
  const dialog = screen.getByRole('dialog', { name: 'Export photos' });
  expect(within(dialog).getByRole('button', { name: 'Close tools' })).toBeTruthy();
  dialog.querySelectorAll('details').forEach((group) => {
    group.open = true;
  });
  expectRendered();
  await expectNoNewViolations('export', LIBRARY_ALLOWED, { page: true });
});
