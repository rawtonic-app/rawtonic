import '../../i18n';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { act, cleanup, fireEvent, render, screen, within } from '@testing-library/react';
import StudioFilmstripDock from './StudioFilmstripDock';
import { useLibraryStore } from '../../store/useLibraryStore';
import { useUIStore } from '../../store/useUIStore';
import { DOCK_SIZE, readFilmstripDock } from '../../utils/filmstripDock';
import type { ImageFile } from '../ui/AppProperties';

vi.mock('@tauri-apps/api/core', () => ({ invoke: vi.fn(), convertFileSrc: (path: string) => `asset://${path}` }));

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
const many = (n: number) => Array.from({ length: n }, (_, i) => file(`/s/IMG_${String(i).padStart(4, '0')}.NEF`));
const dockState = () => readFilmstripDock(useUIStore.getState().uiVisibility.filmstripDock);

function mount(
  files: ImageFile[],
  side: 'left' | 'right' = 'left',
  requestThumbnails = vi.fn(),
  onSelect = vi.fn(),
  current = 1
) {
  const view = render(
    <div className="studio-body">
      <StudioFilmstripDock
        side={side}
        files={files}
        currentPath={files[current]?.path}
        onSelect={onSelect}
        onOpen={vi.fn()}
        onRate={vi.fn()}
        onContextMenu={vi.fn()}
        requestThumbnails={requestThumbnails}
      />
    </div>
  );
  const browser = view.container.querySelector<HTMLElement>('.studio-browser')!;
  Object.defineProperty(browser, 'clientWidth', { value: 224, configurable: true });
  Object.defineProperty(browser, 'clientHeight', { value: 600, configurable: true });
  return { ...view, browser, requestThumbnails, onSelect };
}

beforeEach(() => {
  vi.stubGlobal(
    'ResizeObserver',
    class {
      observe() {}
      disconnect() {}
      unobserve() {}
    }
  );
  useUIStore.setState((s) => ({ uiVisibility: { ...s.uiVisibility, filmstripDock: undefined, modules: undefined } }));
  useLibraryStore.setState({ filterCriteria: { colors: [], rating: 0, rawStatus: 0 as never } });
});
afterEach(() => {
  cleanup();
  vi.unstubAllGlobals();
});

describe('docked filmstrip', () => {
  it('shows the count in its header and the photographs as a vertical grid', () => {
    const { container } = mount(many(3));
    expect(screen.getByText('3 photos')).toBeTruthy();
    expect(container.querySelector('.studio-browser.is-dock')).not.toBeNull();
    expect(screen.getAllByRole('option')).toHaveLength(3);
  });

  it('virtualises: 1000 photographs mount a window and request only those thumbnails', () => {
    const { requestThumbnails } = mount(many(1000));
    expect(screen.getAllByRole('option').length).toBeLessThan(100);
    const asked = [...new Set(requestThumbnails.mock.calls.flatMap((call) => call[0] as string[]))];
    expect(asked.length).toBeLessThan(100);
    expect(asked).toContain('/s/IMG_0000.NEF');
    expect(asked).not.toContain('/s/IMG_0999.NEF');
  });

  it('selects like the bottom strip: a click chooses that photograph', () => {
    const { onSelect } = mount(many(3));
    fireEvent.click(screen.getAllByRole('option')[2].querySelector('button')!);
    expect(onSelect).toHaveBeenCalledWith('/s/IMG_0002.NEF', expect.anything());
  });

  it('filters by rating, flag and colour through the library filter, and clears it', () => {
    mount(many(3));
    fireEvent.click(screen.getByRole('button', { name: '3★+' }));
    expect(useLibraryStore.getState().filterCriteria.rating).toBe(3);
    fireEvent.click(screen.getByRole('button', { name: '3★+' }));
    expect(useLibraryStore.getState().filterCriteria.rating).toBe(0);
    fireEvent.click(screen.getByRole('button', { name: 'Picked' }));
    expect(useLibraryStore.getState().filterCriteria.flag).toBe('picked');
    fireEvent.click(screen.getByRole('button', { name: 'Red label' }));
    expect(useLibraryStore.getState().filterCriteria.colors).toEqual(['red']);
    fireEvent.click(screen.getByRole('button', { name: 'Clear filters' }));
    expect(useLibraryStore.getState().filterCriteria).toMatchObject({ flag: 'all', rating: 0, colors: [] });
  });

  it('shows file names only when asked, and remembers the choice', () => {
    const { container } = mount(many(2));
    expect(container.querySelector('.studio-photo-name')).toBeNull();
    fireEvent.click(screen.getByRole('button', { name: 'File names' }));
    expect(container.querySelectorAll('.studio-photo-name')).toHaveLength(2);
    expect(dockState().names).toBe(true);
  });

  it('sets the thumbnail size with Ctrl/⌘ + wheel and leaves a plain wheel to scrolling', () => {
    const { browser } = mount(many(40));
    const before = dockState().size;
    const plain = new WheelEvent('wheel', { deltaY: -100, cancelable: true, bubbles: true });
    browser.dispatchEvent(plain);
    expect(dockState().size).toBe(before);
    expect(plain.defaultPrevented).toBe(false);
    const zoom = new WheelEvent('wheel', { deltaY: -100, ctrlKey: true, cancelable: true, bubbles: true });
    act(() => void browser.dispatchEvent(zoom));
    expect(zoom.defaultPrevented).toBe(true);
    expect(dockState().size).toBe(before + DOCK_SIZE.step);
    act(
      () =>
        void browser.dispatchEvent(
          new WheelEvent('wheel', { deltaY: 100, metaKey: true, cancelable: true, bubbles: true })
        )
    );
    expect(dockState().size).toBe(before);
  });

  it('moves to the other side or back under the photograph from its header', () => {
    mount(many(2));
    const header = screen.getByRole('complementary').querySelector('header')!;
    fireEvent.click(within(header as HTMLElement).getByRole('button', { name: 'Right of the photo' }));
    expect(dockState().side).toBe('right');
    fireEvent.click(within(header as HTMLElement).getByRole('button', { name: 'Under the photo' }));
    expect(dockState().side).toBe('bottom');
  });

  it('shows the filmstrip again when a side is chosen while it was hidden', () => {
    useUIStore.getState().togglePanels('filmstrip');
    useUIStore.setState({ activeView: 'editor' });
    useUIStore.getState().setFilmstripDock({ side: 'left' });
    expect(useUIStore.getState().uiVisibility.modules?.develop?.filmstrip).toBe(true);
  });

  it('docks by dragging the header to a window edge and previews the place', () => {
    const { container } = mount(many(2), 'left');
    const body = container.querySelector<HTMLElement>('.studio-body')!;
    body.getBoundingClientRect = () =>
      ({
        left: 0,
        top: 0,
        width: 1000,
        height: 500,
        right: 1000,
        bottom: 500,
        x: 0,
        y: 0,
        toJSON: () => ({})
      }) as DOMRect;
    const header = container.querySelector<HTMLElement>('.studio-dock-header')!;
    header.setPointerCapture = vi.fn();
    header.releasePointerCapture = vi.fn();
    header.hasPointerCapture = () => true;
    fireEvent.pointerDown(header, { button: 0, clientX: 50, clientY: 10, pointerId: 1 });
    fireEvent.pointerMove(header, { clientX: 900, clientY: 200, pointerId: 1 });
    expect(container.querySelector('.studio-dock-drop.is-right')).not.toBeNull();
    fireEvent.pointerUp(header, { clientX: 900, clientY: 200, pointerId: 1 });
    expect(container.querySelector('.studio-dock-drop')).toBeNull();
    expect(dockState().side).toBe('right');
  });

  it('a click on the header without travel does not move it', () => {
    const { container } = mount(many(2), 'left');
    const header = container.querySelector<HTMLElement>('.studio-dock-header')!;
    header.setPointerCapture = vi.fn();
    header.hasPointerCapture = () => false;
    fireEvent.pointerDown(header, { button: 0, clientX: 50, clientY: 10, pointerId: 1 });
    fireEvent.pointerUp(header, { clientX: 50, clientY: 10, pointerId: 1 });
    expect(dockState().side).toBe('bottom');
  });

  it('keeps its scroll place when hidden and shown again', () => {
    const size = vi.spyOn(HTMLElement.prototype, 'clientWidth', 'get').mockReturnValue(224);
    const height = vi.spyOn(HTMLElement.prototype, 'clientHeight', 'get').mockReturnValue(600);
    // jsdom does not scroll: keep the assigned place per element.
    const places = new WeakMap<Element, number>();
    Object.defineProperty(HTMLElement.prototype, 'scrollTop', {
      configurable: true,
      get(this: Element) {
        return places.get(this) ?? 0;
      },
      set(this: Element, value: number) {
        places.set(this, value);
      }
    });
    try {
      const first = mount(many(400), 'left', vi.fn(), vi.fn(), 80);
      first.browser.scrollTop = 3000;
      fireEvent.scroll(first.browser);
      first.unmount();
      const again = mount(many(400), 'left', vi.fn(), vi.fn(), 80);
      expect(again.browser.scrollTop).toBeGreaterThan(2000);
    } finally {
      delete (HTMLElement.prototype as { scrollTop?: number }).scrollTop;
      size.mockRestore();
      height.mockRestore();
    }
  });

  it('keeps its scroll place while hidden by the full screen and shown again', () => {
    let observe: ResizeObserverCallback = () => {};
    vi.stubGlobal(
      'ResizeObserver',
      class {
        constructor(callback: ResizeObserverCallback) {
          observe = callback;
        }
        observe() {}
        disconnect() {}
        unobserve() {}
      }
    );
    const places = new WeakMap<Element, number>();
    Object.defineProperty(HTMLElement.prototype, 'scrollTop', {
      configurable: true,
      get(this: Element) {
        return places.get(this) ?? 0;
      },
      set(this: Element, value: number) {
        places.set(this, value);
      }
    });
    let visibleWidth = 224;
    const width = vi.spyOn(HTMLElement.prototype, 'clientWidth', 'get').mockImplementation(() => visibleWidth);
    const height = vi.spyOn(HTMLElement.prototype, 'clientHeight', 'get').mockReturnValue(600);
    const size = (w: number, h: number) =>
      act(() => observe([{ contentRect: { width: w, height: h } } as ResizeObserverEntry], {} as ResizeObserver));
    try {
      const view = mount(many(400), 'left', vi.fn(), vi.fn(), 80);
      Object.defineProperty(view.browser, 'clientWidth', { get: () => visibleWidth, configurable: true });
      view.browser.scrollTop = 3000;
      fireEvent.scroll(view.browser);
      // display: none: no size, the browser drops the place and says so.
      visibleWidth = 0;
      size(0, 0);
      places.set(view.browser, 0);
      fireEvent.scroll(view.browser);
      visibleWidth = 224;
      size(224, 600);
      expect(view.browser.scrollTop).toBeGreaterThan(2000);
    } finally {
      delete (HTMLElement.prototype as { scrollTop?: number }).scrollTop;
      width.mockRestore();
      height.mockRestore();
    }
  });

  it('docks from the right column to the left by dragging the header', () => {
    const { container } = mount(many(2), 'right');
    const body = container.querySelector<HTMLElement>('.studio-body')!;
    body.getBoundingClientRect = () =>
      ({
        left: 0,
        top: 0,
        width: 1000,
        height: 500,
        right: 1000,
        bottom: 500,
        x: 0,
        y: 0,
        toJSON: () => ({})
      }) as DOMRect;
    const header = container.querySelector<HTMLElement>('.studio-dock-header')!;
    header.setPointerCapture = vi.fn();
    header.releasePointerCapture = vi.fn();
    header.hasPointerCapture = () => true;
    fireEvent.pointerDown(header, { button: 0, clientX: 950, clientY: 10, pointerId: 1 });
    fireEvent.pointerMove(header, { clientX: 100, clientY: 200, pointerId: 1 });
    fireEvent.pointerUp(header, { clientX: 100, clientY: 200, pointerId: 1 });
    expect(dockState().side).toBe('left');
  });

  it('reveals a late current photograph once measured, as the dock opens', () => {
    const width = vi.spyOn(HTMLElement.prototype, 'clientWidth', 'get').mockReturnValue(224);
    const height = vi.spyOn(HTMLElement.prototype, 'clientHeight', 'get').mockReturnValue(600);
    const places = new WeakMap<Element, number>();
    Object.defineProperty(HTMLElement.prototype, 'scrollTop', {
      configurable: true,
      get(this: Element) {
        return places.get(this) ?? 0;
      },
      set(this: Element, value: number) {
        places.set(this, value);
      }
    });
    try {
      const { browser, container } = mount(many(400), 'left', vi.fn(), vi.fn(), 300);
      // Row 150 of two columns starts at 150 * 92 px; it is in view and mounted.
      expect(browser.scrollTop).toBeGreaterThan(150 * 92 - 600);
      expect(container.querySelector('[aria-label="IMG_0300.NEF"]')).not.toBeNull();
    } finally {
      delete (HTMLElement.prototype as { scrollTop?: number }).scrollTop;
      width.mockRestore();
      height.mockRestore();
    }
  });
});
