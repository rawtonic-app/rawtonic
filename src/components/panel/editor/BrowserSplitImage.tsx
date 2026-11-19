import { useId, useMemo } from 'react';
import { useShallow } from 'zustand/react/shallow';
import { Loader2, TriangleAlert } from 'lucide-react';
import { useEditorStore } from '../../../store/useEditorStore';
import { drawsOneToOne, isFullPreviewZoom } from '../../../utils/pixelSnap';
import { useSettingsStore } from '../../../store/useSettingsStore';
import { useSnapshotStore } from '../../../store/useSnapshotStore';
import { beforeAdjustments } from '../../../utils/beforeSnapshot';
import { showClippingValue } from '../../../utils/clippingWarnings';
import { useBrowserViewPreview, type ViewRegion } from '../../../hooks/useBrowserViewPreview';
import { browserSplitWidth } from '../../../utils/beforeAfter';
import type { RenderSize } from '../../../hooks/useImageRenderSize';
import type { Adjustments } from '../../../utils/adjustments';

/** The browser's before frame (Linux): the unedited recipe, or the snapshot
 * compared with, at the resolution the viewer shows it at; `null` path while
 * `active` is false. */
export function useBrowserBeforeFrame({
  path,
  adjustments,
  size,
  zoomTarget,
  active,
  decoded
}: {
  path: string;
  adjustments: Adjustments;
  size: RenderSize;
  zoomTarget: number;
  active: boolean;
  decoded?: boolean;
}) {
  const view = useEditorStore(
    useShallow((s) => ({
      showClipping: showClippingValue(s.clipHighlights, s.clipShadows),
      softProof: s.softProof ? s.softProofSpace : null,
      softProofWarning: s.softProof && s.softProofWarning,
      colorRangeShow: s.colorRangeShow
    }))
  );
  // A snapshot compared with is the before; its list read again re-reads it.
  useEditorStore((s) => s.beforeSnapshot);
  useSnapshotStore((s) => s.snapshots);
  const after = useEditorStore((s) => s.finalPreview);
  const afterRegion = useEditorStore((s) => (s.finalRegion?.path === path ? s.finalRegion : null));
  const dpr = window.devicePixelRatio || 1;
  const fullZoom = isFullPreviewZoom(zoomTarget, size.scale, dpr);
  // At 100 % and closer the After is the visible region (lossless): the
  // Before shows the same region alike.
  const region = useMemo(
    () =>
      fullZoom && afterRegion
        ? ([afterRegion.normX, afterRegion.normY, afterRegion.normW, afterRegion.normH] as const)
        : null,
    [fullZoom, afterRegion]
  );
  // Wait for the settled After at the new zoom before sending or showing
  // a Before. Its codec can change even when the recipe stays the same.
  const ready = region || (after && after.fullZoom === fullZoom && !(fullZoom && after.lossless));
  const recipe = JSON.stringify({
    ...beforeAdjustments(adjustments),
    ...view
  });
  // As large as shown, up to the photograph's own size at 100 % (the
  // native side keeps it within the GPU's texture limit), so the before is
  // as sharp as the after it is compared with.
  const resolution = Math.max(64, Math.ceil((Math.max(size.width, size.height) * zoomTarget * dpr) / 64) * 64);
  const before = useBrowserViewPreview(
    active && ready ? path : null,
    recipe,
    resolution,
    String(decoded),
    region ? true : (after?.lossless ?? null),
    region
  );
  // Under a region Before, the whole Before at the fit's size: a pan
  // uncovers it (not the After) until the new region arrives, as the After
  // shows its last whole frame.
  const fitResolution = Math.max(64, Math.ceil((Math.max(size.width, size.height) * dpr) / 64) * 64);
  const backdrop = useBrowserViewPreview(active && region ? path : null, recipe, fitResolution, String(decoded), true);
  return { ...before, backdrop: region ? backdrop.frame : null };
}

/** Where a browser frame is drawn in the photograph's box: a region frame
 * at its part, else over the whole box. */
export function regionPlacement(region: ViewRegion | null) {
  if (!region) return { x: '0', y: '0', width: '100%', height: '100%' };
  const [x, y, w, h] = region;
  return {
    x: `${x * 100}%`,
    y: `${y * 100}%`,
    width: `${w * 100}%`,
    height: `${h * 100}%`,
    preserveAspectRatio: 'none'
  };
}

/** The before frame's place holder: a spinner, or the error that stopped it. */
export function BeforeFrameStatus({ error }: { error?: string | null }) {
  return error ? (
    <TriangleAlert size={18} role="img" aria-label={error} />
  ) : (
    <Loader2 size={18} className="animate-spin" />
  );
}

/** Before pixels in the same transformed SVG as the browser's edited frame.
 * The clip is in viewer coordinates, so moving the divider never renders.
 * Side by side, the before has its own pane (BrowserBeforePane). */
export default function BrowserSplitImage({
  path,
  adjustments,
  size,
  transform,
  zoomTarget,
  enabled,
  decoded
}: {
  path: string;
  adjustments: Adjustments;
  size: RenderSize;
  transform: { scale: number; positionX: number; positionY: number };
  zoomTarget?: number;
  enabled: boolean;
  decoded?: boolean;
}) {
  const linux = useSettingsStore((s) => s.osPlatform === 'linux');
  const split = useEditorStore((s) => s.splitView && !s.showOriginal && s.splitOrientation !== 'side-by-side');
  const vertical = useEditorStore((s) => s.splitOrientation === 'top-bottom');
  const position = useEditorStore((s) => s.splitPosition);
  const id = useId();
  const active = linux && enabled && split;
  const { frame, error, backdrop } = useBrowserBeforeFrame({
    path,
    adjustments,
    size,
    zoomTarget: zoomTarget ?? transform.scale,
    active,
    decoded
  });
  if (!active) return null;
  const extent = vertical
    ? browserSplitWidth(
        position,
        { containerWidth: size.containerHeight, offsetX: size.offsetY, width: size.height },
        { scale: transform.scale, positionX: transform.positionY }
      )
    : browserSplitWidth(position, size, transform);
  return (
    <g aria-busy={!frame && !error} data-testid="browser-split-image">
      <defs>
        <clipPath id={id}>
          <rect x={0} y={0} width={vertical ? size.width : extent} height={vertical ? extent : size.height} />
        </clipPath>
      </defs>
      {frame?.region && backdrop && (
        <image
          href={backdrop.url}
          {...regionPlacement(null)}
          clipPath={`url(#${id})`}
          data-testid="browser-before-backdrop"
        />
      )}
      {frame ? (
        <image
          href={frame.url}
          {...regionPlacement(frame.region)}
          clipPath={`url(#${id})`}
          style={{
            imageRendering: drawsOneToOne(frame.pixelWidth, size, transform.scale, window.devicePixelRatio || 1)
              ? 'pixelated'
              : 'auto'
          }}
        />
      ) : (
        <foreignObject x={8} y={32} width={24} height={24}>
          <BeforeFrameStatus error={error} />
        </foreignObject>
      )}
    </g>
  );
}
