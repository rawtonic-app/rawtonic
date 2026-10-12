import { useEffect } from "react";
import { Loader2, TriangleAlert } from "lucide-react";
import { useBrowserViewPreview } from "../../hooks/useBrowserViewPreview";
import { useMultiViewStore } from "../../store/useMultiViewStore";
import { fitScaleOf, type SharedZoom } from "../../utils/multiView";

/** Browser equivalent of a secondary native cell. The GPU develops its
 * stored recipe; pan/zoom changes only CSS while the sharper frame loads. */
export default function BrowserViewCell({
  path,
  width,
  height,
  revision,
  zoom,
}: {
  path: string;
  width: number;
  height: number;
  revision: string;
  zoom: SharedZoom | null;
}) {
  const sizes = useMultiViewStore((s) => s.cellSizes[path]);
  const dpr = window.devicePixelRatio || 1;
  const edge =
    sizes && zoom
      ? Math.max(sizes.width, sizes.height) * zoom.scale
      : Math.max(width, height) * dpr;
  const resolution = Math.min(4096, Math.max(64, Math.ceil(edge / 64) * 64));
  const { frame, error } = useBrowserViewPreview(
    path,
    null,
    resolution,
    revision,
  );
  useEffect(() => {
    if (frame)
      useMultiViewStore
        .getState()
        .setCellSizes([{ path, width: frame.width, height: frame.height }]);
  }, [frame, path]);
  const size = frame ?? sizes;
  const fit = size ? fitScaleOf({ width, height }, size, dpr) : 1;
  const scale = size && zoom ? zoom.scale / fit : 1;
  const photoW = size ? (size.width * fit) / dpr : width;
  const photoH = size ? (size.height * fit) / dpr : height;
  return (
    <div
      className="studio-mv-browser"
      style={{ height }}
      aria-busy={!frame && !error}
    >
      {frame ? (
        <img
          src={frame.url}
          alt=""
          draggable={false}
          style={{
            width: photoW * scale,
            height: photoH * scale,
            left: width / 2 - (zoom?.center[0] ?? 0.5) * photoW * scale,
            top: height / 2 - (zoom?.center[1] ?? 0.5) * photoH * scale,
          }}
        />
      ) : error ? (
        <TriangleAlert size={18} role="img" aria-label={error} />
      ) : (
        <Loader2 size={18} className="animate-spin" />
      )}
    </div>
  );
}
