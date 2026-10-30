import { useId } from "react";
import { useShallow } from "zustand/react/shallow";
import { Loader2, TriangleAlert } from "lucide-react";
import { useEditorStore } from "../../../store/useEditorStore";
import { useSettingsStore } from "../../../store/useSettingsStore";
import { useSnapshotStore } from "../../../store/useSnapshotStore";
import { beforeAdjustments } from "../../../utils/beforeSnapshot";
import { showClippingValue } from "../../../utils/clippingWarnings";
import { useBrowserViewPreview } from "../../../hooks/useBrowserViewPreview";
import { browserSplitWidth } from "../../../utils/beforeAfter";
import type { RenderSize } from "../../../hooks/useImageRenderSize";
import type { Adjustments } from "../../../utils/adjustments";

/** Before pixels in the same transformed SVG as the browser's edited frame.
 * The clip is in viewer coordinates, so moving the divider never renders. */
export default function BrowserSplitImage({
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
  const linux = useSettingsStore((s) => s.osPlatform === "linux");
  const split = useEditorStore((s) => s.splitView && !s.showOriginal);
  const vertical = useEditorStore((s) => s.splitOrientation === 'top-bottom');
  const position = useEditorStore((s) => s.splitPosition);
  const view = useEditorStore(
    useShallow((s) => ({
      showClipping: showClippingValue(s.clipHighlights, s.clipShadows),
      softProof: s.softProof ? s.softProofSpace : null,
      softProofWarning: s.softProof && s.softProofWarning,
      colorRangeShow: s.colorRangeShow,
    })),
  );
  const id = useId();
  const active = linux && enabled && split;
  // A snapshot compared with is the before; its list read again re-reads it.
  useEditorStore((s) => s.beforeSnapshot);
  useSnapshotStore((s) => s.snapshots);
  const recipe = JSON.stringify({
    ...beforeAdjustments(adjustments),
    ...view,
  });
  const resolution = Math.min(
    4096,
    Math.max(
      64,
      Math.ceil(
        (Math.max(size.width, size.height) *
          transform.scale *
          (window.devicePixelRatio || 1)) /
          64,
      ) * 64,
    ),
  );
  const { frame, error } = useBrowserViewPreview(
    active ? path : null,
    recipe,
    resolution,
    String(decoded),
  );
  if (!active) return null;
  const extent = vertical
    ? browserSplitWidth(position, { containerWidth: size.containerHeight, offsetX: size.offsetY, width: size.height }, { scale: transform.scale, positionX: transform.positionY })
    : browserSplitWidth(position, size, transform);
  return (
    <g aria-busy={!frame && !error} data-testid="browser-split-image">
      <defs>
        <clipPath id={id}>
          <rect x={0} y={0} width={vertical ? size.width : extent} height={vertical ? extent : size.height} />
        </clipPath>
      </defs>
      {frame ? (
        <image
          href={frame.url}
          x={0}
          y={0}
          width="100%"
          height="100%"
          clipPath={`url(#${id})`}
        />
      ) : (
        <foreignObject x={8} y={32} width={24} height={24}>
          {error ? (
            <TriangleAlert size={18} role="img" aria-label={error} />
          ) : (
            <Loader2 size={18} className="animate-spin" />
          )}
        </foreignObject>
      )}
    </g>
  );
}
