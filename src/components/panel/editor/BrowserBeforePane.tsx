import { useSettingsStore } from '../../../store/useSettingsStore';
import type { RenderSize } from '../../../hooks/useImageRenderSize';
import type { Adjustments } from '../../../utils/adjustments';
import { BeforeFrameStatus, useBrowserBeforeFrame } from './BrowserSplitImage';
import { drawsOneToOne } from '../../../utils/pixelSnap';

/**
 * Side by side on Linux (browser previews): the before pane's photograph,
 * placed exactly as the after pane places its own (same fit, zoom and pan;
 * both panes have the same size). The native display draws this pane itself.
 */
export default function BrowserBeforePane({
  path,
  adjustments,
  size,
  transform,
  enabled,
  decoded
}: {
  path: string;
  adjustments: Adjustments;
  size: RenderSize;
  transform: { scale: number; positionX: number; positionY: number };
  enabled: boolean;
  decoded?: boolean;
}) {
  const linux = useSettingsStore((s) => s.osPlatform === 'linux');
  const active = linux && enabled && size.width > 0 && size.height > 0;
  const { frame, error } = useBrowserBeforeFrame({ path, adjustments, size, transform, active, decoded });
  if (!active) return null;
  return (
    <div
      className="absolute inset-0 origin-top-left pointer-events-none"
      data-testid="browser-before-pane"
      aria-busy={!frame && !error}
      style={{ transform: `translate(${transform.positionX}px, ${transform.positionY}px) scale(${transform.scale})` }}
    >
      {frame ? (
        // Drawn as the after pane draws its frame (ImageCanvas): an <image>
        // in an <svg> box at the fit's place. WebKit places an <img> and an
        // <svg> at a fractional offset differently, which 100 % magnified
        // into a 2 device px shift between the panes (LNX1).
        <svg
          className="absolute select-none"
          style={{
            left: size.offsetX,
            top: size.offsetY,
            width: size.width,
            height: size.height,
            overflow: 'visible'
          }}
        >
          <image
            href={frame.url}
            x="0"
            y="0"
            width="100%"
            height="100%"
            // As the after pane: a full-size frame at 100 % unfiltered.
            style={{
              imageRendering: drawsOneToOne(frame.pixelWidth, size, transform.scale, window.devicePixelRatio || 1)
                ? 'pixelated'
                : 'auto'
            }}
          />
        </svg>
      ) : (
        <div className="absolute" style={{ left: size.offsetX + 8, top: size.offsetY + 32 }}>
          <BeforeFrameStatus error={error} />
        </div>
      )}
    </div>
  );
}
