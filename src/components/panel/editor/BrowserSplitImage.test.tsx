import { cleanup, render } from '@testing-library/react';
import { afterEach, expect, it, vi } from 'vitest';
import BrowserSplitImage from './BrowserSplitImage';
import { useEditorStore } from '../../../store/useEditorStore';
import { useSettingsStore } from '../../../store/useSettingsStore';
import { INITIAL_ADJUSTMENTS } from '../../../utils/adjustments';
vi.mock('../../../hooks/useBrowserViewPreview', () => ({ useBrowserViewPreview: () => ({ frame: { url: 'blob:before' } }) }));
afterEach(cleanup);
it.each(['left-right', 'top-bottom'] as const)('clips a developed before frame along the %s viewer axis while panned and zoomed', (splitOrientation) => {
  useSettingsStore.setState({ osPlatform: 'linux' });
  useEditorStore.setState({ splitView: true, splitOrientation, splitPosition: 0.5, showOriginal: false });
  const { container } = render(<svg><BrowserSplitImage path="/a.dng" adjustments={INITIAL_ADJUSTMENTS}
    size={{ scale: 1, width: 600, height: 400, containerWidth: 800, containerHeight: 600, offsetX: 100, offsetY: 100 }}
    transform={{ scale: 2, positionX: -100, positionY: -50 }} enabled /></svg>);
  const rect = container.querySelector('clipPath rect')!;
  expect(rect.getAttribute('width')).toBe(splitOrientation === 'top-bottom' ? '600' : '150');
  expect(rect.getAttribute('height')).toBe(splitOrientation === 'top-bottom' ? '75' : '400');
  expect(container.querySelector('image')?.getAttribute('href')).toBe('blob:before');
});
