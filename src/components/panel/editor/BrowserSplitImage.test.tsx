import { act, cleanup, render } from '@testing-library/react';
import { afterEach, expect, it, vi } from 'vitest';
import BrowserSplitImage from './BrowserSplitImage';
import { useEditorStore } from '../../../store/useEditorStore';
import { useSettingsStore } from '../../../store/useSettingsStore';
import { INITIAL_ADJUSTMENTS } from '../../../utils/adjustments';
const preview = vi.hoisted(() => vi.fn((..._args: unknown[]) => ({ frame: { url: 'blob:before' } })));
vi.mock('../../../hooks/useBrowserViewPreview', () => ({ useBrowserViewPreview: preview }));
afterEach(cleanup);
/** The Before's own request (the backdrop's has no region argument). */
const beforeCall = () => preview.mock.calls.filter((args) => args.length === 6).at(-1);
it.each(['left-right', 'top-bottom'] as const)(
  'clips a developed before frame along the %s viewer axis while panned and zoomed',
  (splitOrientation) => {
    useSettingsStore.setState({ osPlatform: 'linux' });
    useEditorStore.setState({
      splitView: true,
      splitOrientation,
      splitPosition: 0.5,
      showOriginal: false,
      finalPreview: { url: 'blob:after', lossless: false, fullZoom: true }
    });
    const { container } = render(
      <svg>
        <BrowserSplitImage
          path="/a.dng"
          adjustments={INITIAL_ADJUSTMENTS}
          size={{
            scale: 1,
            width: 600,
            height: 400,
            containerWidth: 800,
            containerHeight: 600,
            offsetX: 100,
            offsetY: 100
          }}
          transform={{ scale: 2, positionX: -100, positionY: -50 }}
          enabled
        />
      </svg>
    );
    const rect = container.querySelector('clipPath rect')!;
    expect(rect.getAttribute('width')).toBe(splitOrientation === 'top-bottom' ? '600' : '150');
    expect(rect.getAttribute('height')).toBe(splitOrientation === 'top-bottom' ? '75' : '400');
    expect(container.querySelector('image')?.getAttribute('href')).toBe('blob:before');
  }
);

it.each([true, false])('asks for the before encoded as the after (lossless %s)', (lossless) => {
  useSettingsStore.setState({ osPlatform: 'linux' });
  useEditorStore.setState({
    splitView: true,
    showOriginal: false,
    splitOrientation: 'left-right',
    finalPreview: { url: 'blob:after', lossless, fullZoom: false }
  });
  preview.mockClear();
  render(
    <svg>
      <BrowserSplitImage
        path="/a.dng"
        adjustments={INITIAL_ADJUSTMENTS}
        size={{
          scale: 0.25,
          width: 600,
          height: 400,
          containerWidth: 800,
          containerHeight: 600,
          offsetX: 0,
          offsetY: 0
        }}
        transform={{ scale: 1, positionX: 0, positionY: 0 }}
        enabled
      />
    </svg>
  );
  expect(beforeCall()?.[4]).toBe(lossless);
});

it('waits at the full zoom target until the After for that target is JPEG', () => {
  useSettingsStore.setState({ osPlatform: 'linux' });
  useEditorStore.setState({
    splitView: true,
    showOriginal: false,
    splitOrientation: 'left-right',
    finalPreview: { url: 'blob:fit', lossless: true, fullZoom: false }
  });
  const split = (
    <svg>
      <BrowserSplitImage
        path="/a.dng"
        adjustments={INITIAL_ADJUSTMENTS}
        size={{
          scale: 0.25,
          width: 600,
          height: 400,
          containerWidth: 800,
          containerHeight: 600,
          offsetX: 0,
          offsetY: 0
        }}
        transform={{ scale: 1, positionX: 0, positionY: 0 }}
        zoomTarget={4}
        enabled
      />
    </svg>
  );
  render(split);
  expect(beforeCall()?.[0]).toBeNull();
  act(() =>
    useEditorStore.setState({
      finalPreview: { url: 'blob:full', lossless: false, fullZoom: true }
    })
  );
  expect(beforeCall()?.[0]).toBe('/a.dng');
  expect(beforeCall()?.[4]).toBe(false);
});

it("shows the after's region at full zoom: the before is asked for and placed as that region, lossless", () => {
  useSettingsStore.setState({ osPlatform: 'linux' });
  useEditorStore.setState({
    splitView: true,
    showOriginal: false,
    splitOrientation: 'left-right',
    finalPreview: { url: 'blob:fit', lossless: true, fullZoom: false },
    finalRegion: { url: 'blob:region', path: '/a.dng', normX: 0.25, normY: 0.5, normW: 0.5, normH: 0.25 }
  });
  preview.mockClear();
  preview.mockImplementation((...args: unknown[]) => ({
    frame: { url: 'blob:before', region: args[5] }
  }));
  const { container } = render(
    <svg>
      <BrowserSplitImage
        path="/a.dng"
        adjustments={INITIAL_ADJUSTMENTS}
        size={{
          scale: 0.25,
          width: 600,
          height: 400,
          containerWidth: 800,
          containerHeight: 600,
          offsetX: 0,
          offsetY: 0
        }}
        transform={{ scale: 4, positionX: 0, positionY: 0 }}
        enabled
      />
    </svg>
  );
  const call = beforeCall()!;
  expect(call[0]).toBe('/a.dng');
  expect(call[4]).toBe(true);
  expect(call[5]).toEqual([0.25, 0.5, 0.5, 0.25]);
  // Under it the whole Before at the fit's size (lossless): a pan uncovers
  // the Before, not the After.
  const backdropCall = preview.mock.calls.filter((args) => args.length === 5 && args[0] === '/a.dng').at(-1)!;
  expect(backdropCall[2]).toBe(640);
  expect(backdropCall[4]).toBe(true);
  const backdrop = container.querySelector('[data-testid="browser-before-backdrop"]')!;
  expect(backdrop.getAttribute('width')).toBe('100%');
  const image = container.querySelector('image:not([data-testid])')!;
  expect(image.getAttribute('x')).toBe('25%');
  expect(image.getAttribute('y')).toBe('50%');
  expect(image.getAttribute('width')).toBe('50%');
  expect(image.getAttribute('height')).toBe('25%');
  preview.mockImplementation(() => ({ frame: { url: 'blob:before' } }) as never);
});

it('keeps the whole Before under a region Before still shown after zooming back out', () => {
  useSettingsStore.setState({ osPlatform: 'linux' });
  useEditorStore.setState({
    splitView: true,
    showOriginal: false,
    splitOrientation: 'left-right',
    finalPreview: { url: 'blob:fit', lossless: true, fullZoom: false },
    finalRegion: null
  });
  preview.mockClear();
  // The Before's hook still holds the 100 % region while the fit Before renders.
  preview.mockImplementation((...args: unknown[]) =>
    args.length === 6
      ? { frame: { url: 'blob:region-before', region: [0.25, 0.5, 0.5, 0.25] } }
      : ({ frame: args[0] ? { url: 'blob:whole-before', region: null } : null } as never)
  );
  const { container } = render(
    <svg>
      <BrowserSplitImage
        path="/a.dng"
        adjustments={INITIAL_ADJUSTMENTS}
        size={{
          scale: 0.25,
          width: 600,
          height: 400,
          containerWidth: 800,
          containerHeight: 600,
          offsetX: 0,
          offsetY: 0
        }}
        transform={{ scale: 1, positionX: 0, positionY: 0 }}
        enabled
      />
    </svg>
  );
  const backdropCall = preview.mock.calls.filter((args) => args.length === 5).at(-1)!;
  expect(backdropCall[0]).toBe('/a.dng');
  expect(container.querySelector('[data-testid="browser-before-backdrop"]')?.getAttribute('href')).toBe(
    'blob:whole-before'
  );
  preview.mockImplementation(() => ({ frame: { url: 'blob:before' } }) as never);
});
