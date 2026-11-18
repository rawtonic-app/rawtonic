import { cleanup, render } from '@testing-library/react';
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
    useEditorStore.setState({ splitView: true, splitOrientation, splitPosition: 0.5, showOriginal: false });
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
  useEditorStore.setState({ splitView: true, showOriginal: false, finalPreviewLossless: lossless });
  preview.mockClear();
  render(
    <svg>
      <BrowserSplitImage
        path="/a.dng"
        adjustments={INITIAL_ADJUSTMENTS}
        size={{ scale: 1, width: 600, height: 400, containerWidth: 800, containerHeight: 600, offsetX: 0, offsetY: 0 }}
        transform={{ scale: 1, positionX: 0, positionY: 0 }}
        enabled
      />
    </svg>
  );
  expect(preview.mock.calls.at(-1)?.[4]).toBe(lossless);
});
