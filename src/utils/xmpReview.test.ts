import { beforeEach, describe, expect, it, vi } from 'vitest';
import { invoke } from '@tauri-apps/api/core';
import { useLibraryStore } from '../store/useLibraryStore';
import { XMP_VIEW_ID, openXmpView, xmpCounts, type XmpReviewEntry } from './xmpReview';

vi.mock('@tauri-apps/api/core', () => ({ invoke: vi.fn() }));

const entry = (path: string, state: XmpReviewEntry['state'], offline = false, copies: string[] = []): XmpReviewEntry => ({
  path,
  state,
  offline,
  volume: null,
  copies,
});

beforeEach(() => {
  vi.clearAllMocks();
  useLibraryStore.setState({ sessionDirectory: '/jobs/Acme' });
});

describe('XMP review view', () => {
  it('lists what is still to review each time it is shown again (after a rename or its undo too)', async () => {
    const select = vi.fn();
    vi.mocked(invoke).mockResolvedValueOnce([entry('/jobs/Acme/Capture/a.nef', 'conflict', false, ['v1'])]);
    await openXmpView('/jobs/Acme', 'XMP to review', select);
    expect(select).toHaveBeenLastCalledWith(XMP_VIEW_ID, 'XMP to review', ['/jobs/Acme/Capture/a.nef', '/jobs/Acme/Capture/a.nef?vc=v1'], false);
    // Renamed meanwhile: the refresh lists its new path.
    vi.mocked(invoke).mockResolvedValueOnce([entry('/jobs/Acme/Capture/b.nef', 'conflict')]);
    await openXmpView('/jobs/Acme', 'XMP to review', select, true);
    expect(invoke).toHaveBeenLastCalledWith('session_xmp_review', { directory: '/jobs/Acme' });
    expect(select).toHaveBeenLastCalledWith(XMP_VIEW_ID, 'XMP to review', ['/jobs/Acme/Capture/b.nef'], true);
  });

  it('does not show another library’s answer', async () => {
    const select = vi.fn();
    vi.mocked(invoke).mockImplementationOnce(async () => {
      useLibraryStore.setState({ sessionDirectory: '/jobs/Other' });
      return [entry('/jobs/Acme/Capture/a.nef', 'pending')];
    });
    await openXmpView('/jobs/Acme', 'XMP to review', select);
    expect(select).not.toHaveBeenCalled();
  });

  it('counts pending, offline and conflicts', () => {
    expect(
      xmpCounts([entry('a', 'pending', true), entry('b', 'pending'), entry('c', 'conflict', true)]),
    ).toEqual({ pending: 2, offline: 1, conflicts: 1 });
  });
});
