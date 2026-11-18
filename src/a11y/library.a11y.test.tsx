import '../i18n';
import { afterEach, beforeEach, it, vi } from 'vitest';
import { act, cleanup, render } from '@testing-library/react';
import StudioWorkspace from '../components/studio/StudioWorkspace';
import { useEditorStore } from '../store/useEditorStore';
import { useLibraryStore } from '../store/useLibraryStore';
import { useUIStore } from '../store/useUIStore';
import { useSettingsStore } from '../store/useSettingsStore';
import type { AppSettings, ImageFile, SelectedImage } from '../components/ui/AppProperties';
import { ContextMenuProvider } from '../context/ContextMenuContext';
import { expectNoNewViolations, KNOWN, type Allowed } from '../utils/axeAudit.testSupport';

vi.mock('@tauri-apps/api/core', () => ({
  invoke: vi.fn().mockResolvedValue({ files: [] }),
  isTauri: () => false,
  convertFileSrc: (path: string) => path
}));
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
  HTMLDialogElement.prototype.close = vi.fn();
  HTMLDialogElement.prototype.showModal = vi.fn();
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

const LIBRARY_ALLOWED: Allowed[] = [KNOWN.toolHeader, KNOWN.gridTile];

it('Library workspace (grid) has no new accessibility violations', async () => {
  useUIStore.setState({ activeView: 'library' });
  workspace();
  await act(async () => {});
  await expectNoNewViolations('library grid', LIBRARY_ALLOWED, { page: true });
});

it('Develop workspace shell (bars, filmstrip, panels) has no new accessibility violations', async () => {
  useUIStore.setState({ activeView: 'editor', studioToolLayout: 'split-tools' });
  workspace();
  await act(async () => {});
  await expectNoNewViolations('develop shell', LIBRARY_ALLOWED, { page: true });
});
