import '../../i18n';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { act, cleanup, fireEvent, render, screen, waitFor } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { useState } from 'react';
import IngestPreviewGrid, { type IngestCandidate } from './IngestPreviewGrid';
import { createLargePreviewCache, zoomOffset, type LargePreview } from './IngestLoupe';
import { useSettingsStore } from '../../store/useSettingsStore';
import type { AppSettings } from '../ui/AppProperties';

vi.mock('@tauri-apps/api/core', () => ({ invoke: vi.fn() }));

const revoked: string[] = [];
beforeEach(() => {
  revoked.length = 0;
  URL.revokeObjectURL = vi.fn((url: string) => void revoked.push(url));
  window.localStorage.clear();
});
afterEach(cleanup);

const files = (count: number): IngestCandidate[] =>
  Array.from({ length: count }, (_, i) => ({
    path: `/Volumes/CARD/DCIM/100MSDCF/DSC0${String(i).padStart(4, '0')}.ARW`,
    size: 25_000_000,
    duplicate: false
  }));

const largePreview = async (path: string): Promise<LargePreview> => ({
  url: `blob:large${path}`,
  info: { iso: 'ISO 400', exposure: '1/250 s', aperture: 'f/2.8', focalLength: '35 mm' }
});

let shownRatings: Record<string, number> = {};
let shownSelection = new Set<string>();

function Harness({
  list,
  loadPreview = largePreview,
  loadFullPreview,
  onCancel
}: {
  list: IngestCandidate[];
  loadPreview?: (path: string) => Promise<LargePreview>;
  loadFullPreview?: (path: string) => Promise<LargePreview>;
  onCancel?: () => void;
}) {
  const [selected, setSelected] = useState(() => new Set(list.map((file) => file.path)));
  const [ratings, setRatings] = useState<Record<string, number>>({});
  shownRatings = ratings;
  shownSelection = selected;
  return (
    <dialog open onCancel={onCancel}>
      <IngestPreviewGrid
        files={list}
        selected={selected}
        ratings={ratings}
        onRatingsChange={setRatings}
        loadThumbnail={async (path) => `blob:thumb${path}`}
        loadPreview={loadPreview}
        loadFullPreview={loadFullPreview}
        onSelectedChange={setSelected}
      />
    </dialog>
  );
}

const shownName = () => document.querySelector('.session-ingest-loupe-title strong')?.textContent;
const loupe = () => screen.queryByRole('region', { name: 'Large view' });

it('fits columns inside the padded preview scroller', () => {
  const width = vi.spyOn(HTMLElement.prototype, 'clientWidth', 'get').mockReturnValue(352);
  try {
    render(<Harness list={files(8)} />);
    expect((document.querySelector('.session-ingest-tiles') as HTMLElement).style.gridTemplateColumns).toBe(
      'repeat(2, 112px)'
    );
  } finally {
    width.mockRestore();
  }
});

describe('Import dialog large view', () => {
  it('opens on double-click and on Enter, shows the large preview and capture info, and closes back to the grid', async () => {
    const user = userEvent.setup();
    render(<Harness list={files(5)} />);
    await user.dblClick(screen.getByRole('checkbox', { name: 'DSC00002.ARW' }));
    expect(loupe()).toBeTruthy();
    expect(shownName()).toBe('DSC00002.ARW');
    expect(screen.getByText('3 / 5')).toBeTruthy();
    await waitFor(() =>
      expect(screen.getByRole('img', { name: 'DSC00002.ARW' }).getAttribute('src')).toBe(
        'blob:large/Volumes/CARD/DCIM/100MSDCF/DSC00002.ARW'
      )
    );
    expect(document.querySelector('.session-ingest-loupe-title span')?.textContent).toContain(
      'ISO 400 · 1/250 s · f/2.8 · 35 mm · 25.0 MB'
    );
    // A double-click toggles a tile twice: its checkbox is as it was.
    expect(screen.getByRole('checkbox', { name: 'DSC00002.ARW' }).getAttribute('aria-checked')).toBe('true');

    await user.keyboard(' ');
    expect(loupe()).toBeNull();
    await waitFor(() => expect(document.activeElement?.getAttribute('aria-label')).toBe('DSC00002.ARW'));

    await user.keyboard('{Enter}');
    expect(shownName()).toBe('DSC00002.ARW');
    // A click on the photo zooms (below); the close button goes back.
    await user.click(screen.getByRole('button', { name: 'Back to the grid (Space)' }));
    expect(loupe()).toBeNull();
  });

  it('steps through the photos with the arrow keys, stopping at the ends', async () => {
    const user = userEvent.setup();
    render(<Harness list={files(3)} />);
    await user.dblClick(screen.getByRole('checkbox', { name: 'DSC00001.ARW' }));
    await user.keyboard('{ArrowRight}');
    expect(shownName()).toBe('DSC00002.ARW');
    await user.keyboard('{ArrowRight}');
    expect(shownName()).toBe('DSC00002.ARW');
    await user.keyboard('{ArrowLeft}{ArrowLeft}');
    expect(shownName()).toBe('DSC00000.ARW');
    await user.keyboard('{ArrowUp}');
    expect(shownName()).toBe('DSC00000.ARW');
    await user.keyboard('{End}');
    expect(shownName()).toBe('DSC00002.ARW');
    await user.click(screen.getByRole('button', { name: 'Previous photo' }));
    expect(shownName()).toBe('DSC00001.ARW');
    // Back in the grid the photo last shown has the focus.
    await user.keyboard('{Escape}');
    await waitFor(() => expect(document.activeElement?.getAttribute('aria-label')).toBe('DSC00001.ARW'));
  });

  it('moves the focus through the grid with the arrow keys and opens the focused photo with Enter', async () => {
    const user = userEvent.setup();
    render(<Harness list={files(8)} />);
    screen.getByRole('checkbox', { name: 'DSC00000.ARW' }).focus();
    await user.keyboard('{ArrowRight}{ArrowRight}');
    expect(document.activeElement?.getAttribute('aria-label')).toBe('DSC00002.ARW');
    await user.keyboard('{Enter}');
    expect(shownName()).toBe('DSC00002.ARW');
    // Enter opened the view; it did not uncheck the tile.
    expect(screen.getByRole('checkbox', { name: 'DSC00002.ARW' }).getAttribute('aria-checked')).toBe('true');
  });

  it('takes Escape for itself while open, so the dialog stays; a second Escape reaches the dialog', async () => {
    const user = userEvent.setup();
    const onCancel = vi.fn();
    render(<Harness list={files(3)} onCancel={onCancel} />);
    await user.dblClick(screen.getByRole('checkbox', { name: 'DSC00000.ARW' }));
    const dialog = document.querySelector('dialog')!;
    const escape = new KeyboardEvent('keydown', { key: 'Escape', bubbles: true, cancelable: true });
    act(() => void document.activeElement!.dispatchEvent(escape));
    expect(escape.defaultPrevented).toBe(true);
    expect(loupe()).toBeNull();
    // The cancel a webview may still send for that key is not the dialog's.
    const cancel = new Event('cancel', { cancelable: true });
    act(() => void dialog.dispatchEvent(cancel));
    expect(cancel.defaultPrevented).toBe(true);
    expect(onCancel).not.toHaveBeenCalled();
    expect(document.querySelector('dialog')?.open).toBe(true);

    await act(() => new Promise((resolve) => setTimeout(resolve, 300)));
    act(() => void dialog.dispatchEvent(new Event('cancel', { cancelable: true })));
    expect(onCancel).toHaveBeenCalledTimes(1);
  });

  it("toggles the shown photo's import checkbox", async () => {
    const user = userEvent.setup();
    render(<Harness list={files(3)} />);
    await user.dblClick(screen.getByRole('checkbox', { name: 'DSC00001.ARW' }));
    const check = screen.getByRole('checkbox', { name: 'Import' });
    expect(check.getAttribute('aria-checked')).toBe('true');
    await user.click(check);
    expect(check.getAttribute('aria-checked')).toBe('false');
    expect(screen.getByRole('status').textContent).toContain('2 of 3 photos selected');
    await user.keyboard('{ArrowRight}');
    expect(screen.getByRole('checkbox', { name: 'Import' }).getAttribute('aria-checked')).toBe('true');
    await user.keyboard('{Escape}');
    expect(screen.getByRole('checkbox', { name: 'DSC00001.ARW' }).getAttribute('aria-checked')).toBe('false');
  });

  it('shows the grid preview until the large one arrives', async () => {
    const user = userEvent.setup();
    let finish: (preview: LargePreview) => void = () => {};
    const slow = () => new Promise<LargePreview>((resolve) => (finish = resolve));
    render(<Harness list={files(1)} loadPreview={slow} />);
    await waitFor(() => expect(document.querySelectorAll('.session-ingest-thumb img')).toHaveLength(1));
    await user.dblClick(screen.getByRole('checkbox', { name: 'DSC00000.ARW' }));
    const image = () => screen.getByRole('img', { name: 'DSC00000.ARW' });
    expect(image().getAttribute('src')).toBe('blob:thumb/Volumes/CARD/DCIM/100MSDCF/DSC00000.ARW');
    expect(image().className).toContain('is-placeholder');
    await act(async () => finish({ url: 'blob:large', info: null }));
    expect(image().getAttribute('src')).toBe('blob:large');
  });

  it('switches the thumbnail size and keeps it', async () => {
    render(<Harness list={files(2)} />);
    const slider = screen.getByRole('slider', { name: 'Thumbnail size' });
    const grid = () => screen.getByRole('group', { name: 'Photos to import' });
    expect(grid().style.getPropertyValue('--ingest-tile')).toBe('112px');
    fireEvent.change(slider, { target: { value: '2' } });
    expect(grid().style.getPropertyValue('--ingest-tile')).toBe('168px');
    cleanup();
    render(<Harness list={files(2)} />);
    expect(grid().style.getPropertyValue('--ingest-tile')).toBe('168px');
  });
});

describe('large preview cache', () => {
  it('loads the shown photo first, keeps its neighbours, drops what is no longer wanted and discards late arrivals', async () => {
    const pending = new Map<string, () => void>();
    const load = vi.fn(
      (path: string) =>
        new Promise<LargePreview>((resolve) => pending.set(path, () => resolve({ url: `blob:${path}`, info: null })))
    );
    const loaded = vi.fn();
    const cache = createLargePreviewCache(load, loaded, 2);
    cache.want(['a', 'b', 'c', 'd']);
    expect(load.mock.calls.map((call) => call[0])).toEqual(['a', 'b']);

    // The view moved on before "c" and "d" started: they are never loaded.
    cache.want(['x', 'a']);
    await act(async () => pending.get('a')!());
    expect(cache.get('a')?.url).toBe('blob:a');
    expect(load.mock.calls.map((call) => call[0])).toEqual(['a', 'b', 'x']);
    // "b" arrives after it stopped being wanted: discarded, its URL revoked.
    await act(async () => pending.get('b')!());
    expect(cache.get('b')).toBeUndefined();
    expect(revoked).toEqual(['blob:b']);
    await act(async () => pending.get('x')!());
    expect(loaded.mock.calls.map((call) => call[0])).toEqual(['a', 'x']);

    cache.want(['x']);
    expect(revoked).toEqual(['blob:b', 'blob:a']);
    cache.clear();
    expect(revoked).toEqual(['blob:b', 'blob:a', 'blob:x']);
    expect(cache.get('x')).toBeUndefined();
  });
});

describe('Reviewing before the import (IMPD)', () => {
  const path = (i: number) => files(5)[i].path;

  it('rates with 0–5, includes with P and leaves out with X, in the grid and the large view', async () => {
    const user = userEvent.setup();
    render(<Harness list={files(5)} />);
    screen.getByRole('checkbox', { name: 'DSC00001.ARW' }).focus();
    await user.keyboard('3');
    expect(shownRatings).toEqual({ [path(1)]: 3 });
    expect(screen.getByLabelText('Rate 3 of 5')).toBeTruthy();
    await user.keyboard('x');
    expect(shownSelection.has(path(1))).toBe(false);
    await user.keyboard('p');
    expect(shownSelection.has(path(1))).toBe(true);
    // Space opens the large view (as in the Library), it does not untick.
    await user.keyboard(' ');
    expect(loupe()).toBeTruthy();
    expect(shownSelection.has(path(1))).toBe(true);
    await user.keyboard('5');
    expect(shownRatings[path(1)]).toBe(5);
    await user.keyboard('x');
    expect(shownSelection.has(path(1))).toBe(false);
    expect(screen.getByRole('checkbox', { name: 'Import' }).getAttribute('aria-checked')).toBe('false');
    await user.keyboard('{ArrowRight}2');
    expect(shownRatings).toEqual({ [path(1)]: 5, [path(2)]: 2 });
    await user.keyboard('0');
    expect(shownRatings).toEqual({ [path(1)]: 5 });
    // The stars in the bar rate too; the same star again clears.
    await user.click(screen.getByRole('button', { name: 'Rate 4 of 5' }));
    expect(shownRatings[path(2)]).toBe(4);
    await user.click(screen.getByRole('button', { name: 'Rate 4 of 5' }));
    expect(shownRatings[path(2)]).toBeUndefined();
  });

  it('shows 100 % only once the full-size pixels arrive, keeps the zoom while stepping, and Z goes back to fit', async () => {
    const user = userEvent.setup();
    const finish: Record<string, (preview: LargePreview) => void> = {};
    const full = vi.fn((p: string) => new Promise<LargePreview>((resolve) => (finish[p] = resolve)));
    render(<Harness list={files(3)} loadFullPreview={full} />);
    await user.dblClick(screen.getByRole('checkbox', { name: 'DSC00000.ARW' }));
    // The shown photograph's full size is made as soon as it is shown, ahead
    // of Z: the shown one first, then the next one, one at a time.
    expect(full.mock.calls.map((c) => c[0])).toEqual([path(0)]);
    await user.keyboard('z');
    expect(full.mock.calls.map((c) => c[0])).toEqual([path(0)]);
    const zoomButton = () => document.querySelector('.session-ingest-loupe-zoom')!;
    expect(zoomButton().textContent).toBe('Loading 100 %…');
    await act(async () => finish[path(0)]({ url: 'blob:full0', info: null }));
    const measure = document.querySelector<HTMLImageElement>('img[hidden]')!;
    Object.defineProperty(measure, 'naturalWidth', { value: 6000 });
    Object.defineProperty(measure, 'naturalHeight', { value: 4000 });
    fireEvent.load(measure);
    expect(zoomButton().textContent).toBe('100 %');
    const fullImage = () => document.querySelector<HTMLImageElement>('img.session-ingest-loupe-full');
    expect(fullImage()?.getAttribute('src')).toBe('blob:full0');
    expect(fullImage()?.style.width).toBe('6000px');
    expect(full.mock.calls.map((c) => c[0])).toEqual([path(0), path(1)]);
    // The next photograph opens at 100 % as soon as its pixels are there.
    await user.keyboard('{ArrowRight}');
    expect(shownName()).toBe('DSC00001.ARW');
    expect(zoomButton().textContent).toBe('Loading 100 %…');
    await act(async () => finish[path(1)]({ url: 'blob:full1', info: null }));
    const measure1 = document.querySelector<HTMLImageElement>('img[hidden]')!;
    Object.defineProperty(measure1, 'naturalWidth', { value: 6000 });
    Object.defineProperty(measure1, 'naturalHeight', { value: 4000 });
    fireEvent.load(measure1);
    expect(fullImage()?.getAttribute('src')).toBe('blob:full1');
    await user.keyboard('z');
    expect(fullImage()).toBeNull();
    expect(zoomButton().textContent).toBe('Fit');
  });

  it('makes the full size ahead, so Z shows 100 % at once, and keeps the previous photograph for stepping back', async () => {
    const user = userEvent.setup();
    const full = vi.fn(async (p: string) => ({ url: `blob:${p}`, info: null, width: 6000, height: 4000 }));
    render(<Harness list={files(3)} loadFullPreview={full} />);
    await user.dblClick(screen.getByRole('checkbox', { name: 'DSC00000.ARW' }));
    // Without any zoom yet: the shown photograph, then the next one.
    await waitFor(() => expect(full.mock.calls.map((c) => c[0])).toEqual([path(0), path(1)]));
    const zoomButton = () => document.querySelector('.session-ingest-loupe-zoom')!;
    const fullImage = () => document.querySelector<HTMLImageElement>('img.session-ingest-loupe-full');
    expect(fullImage()).toBeNull();
    await user.keyboard('z');
    // Already decoded and measured: 100 % at once, no measuring image.
    expect(zoomButton().textContent).toBe('100 %');
    expect(fullImage()?.getAttribute('src')).toBe(`blob:${path(0)}`);
    expect(fullImage()?.style.width).toBe('6000px');
    await user.keyboard('{ArrowRight}');
    expect(fullImage()?.getAttribute('src')).toBe(`blob:${path(1)}`);
    await waitFor(() => expect(full.mock.calls.map((c) => c[0])).toEqual([path(0), path(1), path(2)]));
    // Back to the first: kept, not made again.
    await user.keyboard('{ArrowLeft}');
    expect(fullImage()?.getAttribute('src')).toBe(`blob:${path(0)}`);
    expect(full).toHaveBeenCalledTimes(3);
  });

  it('Z zooms around the pointer over the photo', async () => {
    const user = userEvent.setup();
    const full = vi.fn(async () => ({ url: 'blob:full', info: null }));
    render(<Harness list={files(1)} loadFullPreview={full} />);
    await user.dblClick(screen.getByRole('checkbox', { name: 'DSC00000.ARW' }));
    const fit = await waitFor(() => document.querySelector<HTMLImageElement>('img.session-ingest-loupe-fit')!);
    fit.getBoundingClientRect = () => ({ left: 0, top: 0, width: 600, height: 400 }) as DOMRect;
    Object.defineProperty(fit, 'naturalWidth', { value: 600 });
    Object.defineProperty(fit, 'naturalHeight', { value: 400 });
    const stage = document.querySelector('.session-ingest-loupe-image')!;
    fireEvent.pointerMove(stage, { clientX: 150, clientY: 100 });
    await user.keyboard('z');
    await waitFor(() => expect(document.querySelector('img[hidden]')).toBeTruthy());
    const measure = document.querySelector<HTMLImageElement>('img[hidden]')!;
    Object.defineProperty(measure, 'naturalWidth', { value: 6000 });
    Object.defineProperty(measure, 'naturalHeight', { value: 4000 });
    fireEvent.load(measure);
    // A quarter across and down, at 100 % (dpr 1): that spot sits at the
    // view's centre (jsdom measured the view as 0 × 0, so its centre is 0, 0).
    const shown = document.querySelector<HTMLImageElement>('img.session-ingest-loupe-full')!;
    expect(shown.style.transform).toBe('translate(-1500px, -1000px)');
  });

  it('says so when no full-size view can be made, and never upscales the fitted preview', async () => {
    const user = userEvent.setup();
    render(<Harness list={files(1)} loadFullPreview={async () => ({ url: null, info: null })} />);
    await user.dblClick(screen.getByRole('checkbox', { name: 'DSC00000.ARW' }));
    await user.keyboard('z');
    await waitFor(() => expect(screen.getByText('No full-size view could be made of this file')).toBeTruthy());
    expect(document.querySelector('img.session-ingest-loupe-full')).toBeNull();
    expect(document.querySelector('.session-ingest-loupe-zoom')!.textContent).not.toBe('100 %');
  });

  it("follows the Library's auto-advance: P, X or a rating moves on to the next photo", async () => {
    useSettingsStore.setState({ appSettings: { autoAdvance: true } as AppSettings });
    try {
      const user = userEvent.setup();
      render(<Harness list={files(3)} />);
      await user.dblClick(screen.getByRole('checkbox', { name: 'DSC00000.ARW' }));
      await user.keyboard('x');
      expect(shownName()).toBe('DSC00001.ARW');
      await user.keyboard('4');
      expect(shownName()).toBe('DSC00002.ARW');
      expect(shownRatings).toEqual({ [path(1)]: 4 });
      expect(shownSelection.has(path(0))).toBe(false);
      // At the last photo it stays.
      await user.keyboard('p');
      expect(shownName()).toBe('DSC00002.ARW');
    } finally {
      useSettingsStore.setState({ appSettings: null as unknown as AppSettings });
    }
  });

  it('centres the zoom on the chosen spot and keeps the image covering the view', () => {
    const image = { width: 6000, height: 4000 };
    const view = { width: 1000, height: 800 };
    expect(zoomOffset({ x: 0.5, y: 0.5 }, image, view)).toEqual({ x: -2500, y: -1600 });
    // At the edges the image is not pulled away from the view's border.
    expect(zoomOffset({ x: 0, y: 0 }, image, view)).toEqual({ x: 0, y: 0 });
    expect(zoomOffset({ x: 1, y: 1 }, image, view)).toEqual({ x: -5000, y: -3200 });
    // A small image is centred.
    expect(zoomOffset({ x: 0.9, y: 0.1 }, { width: 400, height: 200 }, view)).toEqual({ x: 300, y: 300 });
  });
});
