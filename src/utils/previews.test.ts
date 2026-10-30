import { afterEach, expect, it, vi } from 'vitest';
import { invoke } from '@tauri-apps/api/core';
import type { TFunction } from 'i18next';
import { notify as toast } from '../activity/notify';
import { Invokes } from '../components/ui/AppProperties';
import { formatBytes, previewTargetResolution, previewsMenu, viewerDimHint } from './previews';

vi.mock('@tauri-apps/api/core', () => ({ invoke: vi.fn() }));
vi.mock('../activity/notify', () => ({ notify: { error: vi.fn(), info: vi.fn() } }));

/** Returns the key and its options, so the test sees what was asked for. */
const t = ((key: string, options?: Record<string, unknown>) =>
  options ? `${key} ${JSON.stringify(options)}` : key) as unknown as TFunction;

afterEach(() => vi.clearAllMocks());

const flush = () => new Promise((resolve) => setTimeout(resolve, 0));

it('offers Standard and 1:1 builds and discards for the selection', async () => {
  vi.mocked(invoke).mockResolvedValue(2);
  const paths = ['/s/Capture/A.RAF', '/s/Capture/B.NEF?vc=1a2b3c'];
  const menu = previewsMenu(t, paths);
  expect(menu.label).toBe('previews.menu.title');
  const [standard, full, discardFull, discardAll] = menu.submenu!;
  expect(standard.label).toContain('previews.menu.buildStandard');
  expect(standard.label).toContain('"n":2');

  standard.onClick!();
  full.onClick!();
  discardFull.onClick!();
  discardAll.onClick!();
  await flush();
  const calls = vi.mocked(invoke).mock.calls;
  expect(calls[0]).toEqual([Invokes.BuildPreviews, { paths, full: false, viewerDim: viewerDimHint() }]);
  expect(calls[1]).toEqual([Invokes.BuildPreviews, { paths, full: true, viewerDim: viewerDimHint() }]);
  expect(calls[2]).toEqual([Invokes.DiscardPreviews, { paths, fullOnly: true }]);
  expect(calls[3]).toEqual([Invokes.DiscardPreviews, { paths, fullOnly: false }]);
  expect(toast.info).toHaveBeenCalledWith('previews.toasts.discarded {"files":2}');
});

it('reports a failed request instead of failing silently', async () => {
  vi.mocked(invoke).mockRejectedValue('No previews folder');
  previewsMenu(t, ['/x.RAF']).submenu![0].onClick!();
  await flush();
  expect(toast.error).toHaveBeenCalledWith('previews.toasts.failed {"err":"No previews folder"}');
});

it('formats disk use in decimal units', () => {
  expect(formatBytes(0)).toBe('0 B');
  expect(formatBytes(10_743_467)).toBe('10.7 MB');
  expect(formatBytes(253_000_000)).toBe('253 MB');
  expect(formatBytes(4_200_000_000)).toBe('4.2 GB');
});

it('renders a grain preview at the display device pixels, other previews with headroom', () => {
  const fit = {
    display: { width: 1034.893, height: 691 },
    dpr: 2,
    fullDpi: false,
    zoomMultiplier: 1,
    original: { width: 5796, height: 3870 },
  };
  // Without grain: 1035 × 1.25 in 256 px steps (1536), or with full DPI 2070 × 1.25 (2816).
  expect(previewTargetResolution({ ...fit, grain: false })).toBe(1536);
  expect(previewTargetResolution({ ...fit, fullDpi: true, grain: false })).toBe(2816);
  // With grain: one rendered pixel per screen pixel, whatever the DPI setting.
  expect(previewTargetResolution({ ...fit, grain: true })).toBe(2070);
  expect(previewTargetResolution({ ...fit, fullDpi: true, grain: true })).toBe(2070);
  // Never above the photograph's own size (100 % = the export).
  expect(previewTargetResolution({ ...fit, display: { width: 4000, height: 2670 }, grain: true })).toBe(5796);
  expect(previewTargetResolution({ ...fit, display: { width: 4000, height: 2670 }, grain: false })).toBe(5796);
});
