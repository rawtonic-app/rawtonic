import { act, cleanup, render } from '@testing-library/react';
import { afterEach, expect, it, vi } from 'vitest';
import BrowserSplitImage from './BrowserSplitImage';
import { useEditorStore } from '../../../store/useEditorStore';
import { useSettingsStore } from '../../../store/useSettingsStore';
import { INITIAL_ADJUSTMENTS } from '../../../utils/adjustments';
const preview = vi.hoisted(() => vi.fn((..._args: unknown[]) => ({ frame: { url: 'blob:before' } })));
vi.mock('../../../hooks/useBrowserViewPreview', () => ({ useBrowserViewPreview: preview }));
afterEach(cleanup);
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
  expect(preview.mock.calls.at(-1)?.[4]).toBe(lossless);
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
  expect(preview.mock.calls.at(-1)?.[0]).toBeNull();
  act(() =>
    useEditorStore.setState({
      finalPreview: { url: 'blob:full', lossless: false, fullZoom: true }
    })
  );
  expect(preview.mock.calls.at(-1)?.[0]).toBe('/a.dng');
  expect(preview.mock.calls.at(-1)?.[4]).toBe(false);
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
  const call = preview.mock.calls.at(-1)!;
  expect(call[0]).toBe('/a.dng');
  expect(call[4]).toBe(true);
  expect(call[5]).toEqual([0.25, 0.5, 0.5, 0.25]);
  const image = container.querySelector('image')!;
  expect(image.getAttribute('x')).toBe('25%');
  expect(image.getAttribute('y')).toBe('50%');
  expect(image.getAttribute('width')).toBe('50%');
  expect(image.getAttribute('height')).toBe('25%');
  preview.mockImplementation(() => ({ frame: { url: 'blob:before' } }) as never);
});
