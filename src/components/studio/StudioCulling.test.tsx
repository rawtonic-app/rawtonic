import '../../i18n';
import { afterEach, beforeEach, expect, it, vi } from 'vitest';
import { cleanup, fireEvent, render, screen, waitFor } from '@testing-library/react';
import { invoke } from '@tauri-apps/api/core';
import { PhotoCullBadge, StudioCulling } from './StudioCulling';
import { useCullStore, type CullPhoto } from '../../store/useCullStore';
import { useLibraryStore } from '../../store/useLibraryStore';
import { useSettingsStore } from '../../store/useSettingsStore';
import { useSessionBar } from '../session/sessionBar';
import { useAutoStackDialog } from '../../utils/stacks';
import { canUndo, catalogAction, undo, useCatalogUndoStore, type CullingChange } from '../../utils/appUndo';
import { suggestionRules } from '../../utils/librarySearch';
import { Invokes, RawStatus, type AppSettings, type ImageFile } from '../ui/AppProperties';

vi.mock('@tauri-apps/api/core', () => ({ invoke: vi.fn() }));
vi.mock('../../activity/notify', () => ({
  notify: { error: vi.fn(), info: vi.fn(), success: vi.fn(), warn: vi.fn(), dismiss: vi.fn(), update: vi.fn() }
}));
const mockInvoke = vi.mocked(invoke);

const photo = (suggested: CullPhoto['suggested'], dismissed: CullPhoto['dismissed'] = []): CullPhoto => ({
  suggested,
  dismissed,
  scored: true
});
const list = (paths: string[]) => paths.map((path) => ({ path }) as ImageFile);

beforeEach(() => {
  localStorage.clear();
  mockInvoke.mockReset();
  mockInvoke.mockImplementation(async (command) => {
    if (command === Invokes.ContentSearchStatus)
      return { installed: true, downloadBytes: 0, running: false, indexed: 4, failed: 0, total: 4 };
    if (command === Invokes.CullDismiss) return ['/job/a.arw'];
    return null;
  });
  useCatalogUndoStore.setState(useCatalogUndoStore.getInitialState());
  useSettingsStore.setState({
    appSettings: { contentSearch: true, eyeCheck: true } as AppSettings,
    handleSettingsChange: vi.fn()
  });
  useLibraryStore.setState({
    sessionDirectory: '/job',
    imageList: list(['/job/a.arw', '/job/b.arw', '/job/c.arw', '/job/d.arw']),
    imageFlags: {},
    multiSelectedPaths: [],
    filterCriteria: { flag: 'all', rating: 0, colors: [], rawStatus: RawStatus.All },
    contentStatus: { installed: true, downloadBytes: 0, running: false, indexed: 4, failed: 0, total: 4 }
  });
  useCullStore.setState({
    photos: {
      '/job/a.arw': photo(['misfire']),
      '/job/b.arw': photo(['misfire', 'document']),
      '/job/c.arw': photo(['eyesClosed']),
      '/job/d.arw': photo([], ['misfire'])
    },
    generation: 0
  });
});
afterEach(() => {
  cleanup();
  useSessionBar.setState({ mark: null });
  useAutoStackDialog.getState().close();
});

it('counts each suggestion and rejects only the checked ones, never closed eyes by default', async () => {
  const mark = vi.fn(async () => {});
  useSessionBar.setState({ mark });
  render(<StudioCulling />);
  expect(screen.getByRole('button', { name: 'Show only Misfires (2)' })).toBeTruthy();
  expect(screen.getByRole('button', { name: 'Show only Documents (1)' })).toBeTruthy();
  expect(screen.getByRole('button', { name: 'Show only Eyes closed (1)' })).toBeTruthy();
  expect((screen.getByRole('checkbox', { name: /Eyes closed/ }) as HTMLInputElement).checked).toBe(false);
  fireEvent.click(screen.getByRole('button', { name: 'Reject 2 suggested photos' }));
  expect(mark).toHaveBeenCalledWith(-1, ['/job/a.arw', '/job/b.arw']);
  // Closed eyes checked: c too; remembered.
  fireEvent.click(screen.getByRole('checkbox', { name: /Eyes closed/ }));
  fireEvent.click(screen.getByRole('button', { name: 'Reject 3 suggested photos' }));
  expect(mark).toHaveBeenLastCalledWith(-1, ['/job/a.arw', '/job/b.arw', '/job/c.arw']);
  expect(JSON.parse(localStorage.getItem('rawtonic.culling.checked')!)).toContain('eyesClosed');
});

it('leaves photos already rejected out, and nothing to reject disables the button', () => {
  useSessionBar.setState({ mark: vi.fn() });
  useLibraryStore.setState({ imageFlags: { '/job/a.arw': -1, '/job/b.arw': -1 } });
  render(<StudioCulling />);
  expect((screen.getByRole('button', { name: 'Reject 0 suggested photos' }) as HTMLButtonElement).disabled).toBe(true);
});

it('rejecting the suggestions is one undo step that undo takes back', async () => {
  const changes: CullingChange[] = ['/job/a.arw', '/job/b.arw'].map((path) => ({
    path,
    before: { kind: 'flag', value: 0 },
    after: { kind: 'flag', value: -1 }
  }));
  // The session's mark, as P/X use it: one catalog step per call.
  const mark = vi.fn((flag: number, paths?: string[]) =>
    catalogAction('flag', async () => {
      await invoke(Invokes.SessionSetFlag, { directory: '/job', paths, flag });
      return changes;
    })
  );
  useSessionBar.setState({ mark });
  mockInvoke.mockImplementation(async (command) => {
    if (command === Invokes.RestoreCulling) return changes.map((c) => ({ ...c, before: c.after, after: c.before }));
    return null;
  });
  render(<StudioCulling />);
  fireEvent.click(screen.getByRole('button', { name: 'Reject 2 suggested photos' }));
  await waitFor(() => expect(canUndo()).toBe(true));
  expect(useCatalogUndoStore.getState().past).toHaveLength(1);
  expect(mockInvoke).toHaveBeenCalledWith(Invokes.SessionSetFlag, {
    directory: '/job',
    paths: ['/job/a.arw', '/job/b.arw'],
    flag: -1
  });
  await undo();
  expect(mockInvoke).toHaveBeenCalledWith(Invokes.RestoreCulling, expect.anything());
  expect(canUndo()).toBe(false);
});

it('filters by a suggestion and turns the filter off again', () => {
  render(<StudioCulling />);
  fireEvent.click(screen.getByRole('button', { name: 'Show only Documents (1)' }));
  expect(useLibraryStore.getState().filterCriteria.suggestion).toBe('document');
  expect(screen.getByRole('button', { name: 'Show only Documents (1)' }).getAttribute('aria-pressed')).toBe('true');
  fireEvent.click(screen.getByRole('button', { name: 'Show only Documents (1)' }));
  expect(useLibraryStore.getState().filterCriteria.suggestion).toBe('all');
  expect(suggestionRules('document')).toEqual([{ field: 'suggestion', op: 'is', value: 'document' }]);
  expect(suggestionRules('all')).toEqual([]);
});

it('dismisses the checked suggestions of the selection and shows dismissed ones again', async () => {
  useLibraryStore.setState({ multiSelectedPaths: ['/job/a.arw', '/job/d.arw'] });
  render(<StudioCulling />);
  fireEvent.click(screen.getByRole('button', { name: 'Not a problem (1 selected)' }));
  await waitFor(() =>
    expect(mockInvoke).toHaveBeenCalledWith(Invokes.CullDismiss, {
      directory: '/job',
      paths: ['/job/a.arw'],
      criterion: 'misfire',
      dismissed: true
    })
  );
  expect(useCullStore.getState().generation).toBe(1);
  fireEvent.click(screen.getByRole('button', { name: 'Show suggestions again (1 selected)' }));
  await waitFor(() =>
    expect(mockInvoke).toHaveBeenCalledWith(Invokes.CullDismiss, {
      directory: '/job',
      paths: ['/job/d.arw'],
      criterion: 'misfire',
      dismissed: false
    })
  );
});

it('offers the search model when it is missing, the eye check when it is off, and Auto-Stack', async () => {
  useSettingsStore.setState({ appSettings: { contentSearch: false, eyeCheck: false } as AppSettings });
  useLibraryStore.setState({
    contentStatus: { installed: false, downloadBytes: 1_535_588_149, running: false, indexed: 0, failed: 0, total: 4 }
  });
  mockInvoke.mockImplementation(async (command) =>
    command === Invokes.ContentSearchStatus
      ? { installed: false, downloadBytes: 1_535_588_149, running: false, indexed: 0, failed: 0, total: 4 }
      : null
  );
  render(<StudioCulling />);
  expect(screen.getByRole('button', { name: 'Find misfires and documents (download 1.5 GB)' })).toBeTruthy();
  expect(screen.getByRole('button', { name: 'Check eyes…' })).toBeTruthy();
  // Nothing is downloaded until clicked.
  expect(mockInvoke).not.toHaveBeenCalledWith(Invokes.ContentSearchDownload, expect.anything());
  fireEvent.click(screen.getByRole('button', { name: 'Auto-Stack…' }));
  expect(useAutoStackDialog.getState().paths).toEqual(['/job/a.arw', '/job/b.arw', '/job/c.arw', '/job/d.arw']);
});

it('says how far the reading is', () => {
  useCullStore.setState({
    photos: { '/job/a.arw': { suggested: [], dismissed: [], scored: false }, '/job/b.arw': photo([]) }
  });
  render(<StudioCulling />);
  expect(screen.getByRole('status').textContent).toBe('Read 1 of 2 photos');
});

it('tags a tile with its misfire or document suggestion, not closed eyes', () => {
  const { container } = render(
    <>
      <PhotoCullBadge path="/job/b.arw" />
      <PhotoCullBadge path="/job/c.arw" />
    </>
  );
  expect(screen.getByRole('img', { name: 'Suggested: Misfires · Documents' })).toBeTruthy();
  expect(container.querySelectorAll('.studio-cull-badge')).toHaveLength(1);
});
