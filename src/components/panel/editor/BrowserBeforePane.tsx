import { useSettingsStore } from '../../../store/useSettingsStore';
import type { RenderSize } from '../../../hooks/useImageRenderSize';
import type { Adjustments } from '../../../utils/adjustments';
import { BeforeFrameStatus, useBrowserBeforeFrame } from './BrowserSplitImage';

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
  decoded,
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
        <img
          alt=""
          src={frame.url}
          draggable={false}
          className="absolute select-none"
          style={{ left: size.offsetX, top: size.offsetY, width: size.width, height: size.height }}
        />
      ) : (
        <div className="absolute" style={{ left: size.offsetX + 8, top: size.offsetY + 32 }}>
          <BeforeFrameStatus error={error} />
        </div>
      )}
    </div>
  );
}
