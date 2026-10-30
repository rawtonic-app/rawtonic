import { memo } from "react";
import { useTranslation } from "react-i18next";
import { useEditorStore } from "../../store/useEditorStore";
import { COLOR_SPACES, COLOR_SPACE_NAMES } from "../../utils/exportRecipes";
import { histogramClipping } from "../../utils/clippingWarnings";
import type { OutputColorSpace } from "../ui/ExportImportProperties";

export function histogramBins(channel: unknown): number[] {
  const bins = Array.isArray(channel)
    ? channel
    : (channel as { data?: unknown })?.data;
  return Array.isArray(bins)
    ? bins.map((value) => (Number.isFinite(value) ? Math.max(0, value) : 0))
    : [];
}

type ClippingSide = "shadows" | "highlights";

/** One histogram corner: a small triangle in the plot that switches that
 * side's warning (left: shadows, blue; right: highlights, red). `lit` is the
 * histogram's own verdict: this side clips, whatever the overlay shows. */
function ClippingCorner({
  side,
  on,
  lit,
  label,
  title,
  onToggle,
}: {
  side: ClippingSide;
  on: boolean;
  lit: boolean;
  label: string;
  title: string;
  onToggle(): void;
}) {
  const size = 20;
  const x = side === "shadows" ? 2 : 256 - size - 2;
  return (
    <g
      className={`studio-clip-${side}${on ? " is-on" : ""}${lit ? " is-lit" : ""}`}
      role="button"
      tabIndex={0}
      aria-label={label}
      aria-pressed={on}
      data-clipping={lit || undefined}
      onClick={(event) => {
        event.stopPropagation();
        onToggle();
      }}
      onKeyDown={(event) => {
        if (event.key !== "Enter" && event.key !== " ") return;
        event.preventDefault();
        event.stopPropagation();
        onToggle();
      }}
    >
      <title>{title}</title>
      <rect
        className="studio-clip-corner-hit"
        x={x}
        y={0}
        width={size}
        height={size}
      />
      {/* The triangle sits in the plot's corner and points inwards. */}
      <polygon
        className="studio-clip-corner-mark"
        points={
          side === "shadows"
            ? `${x + 3},3 ${x + 3},17 ${x + 17},17`
            : `${x + size - 3},3 ${x + size - 3},17 ${x + size - 17},17`
        }
      />
    </g>
  );
}

export default memo(function StudioHistogram() {
  const { t } = useTranslation();
  const histogram = useEditorStore((s) => s.histogram);
  const image = useEditorStore((s) => s.selectedImage);
  const clipHighlights = useEditorStore((s) => s.clipHighlights);
  const clipShadows = useEditorStore((s) => s.clipShadows);
  const proof = useEditorStore((s) => s.softProof);
  const proofSpace = useEditorStore((s) => s.softProofSpace);
  const proofWarning = useEditorStore((s) => s.softProofWarning);
  const setEditor = useEditorStore((s) => s.setEditor);
  const clipped = histogramClipping(histogram);
  const channels = ["red", "green", "blue"] as const;
  const max = Math.max(
    1,
    ...channels.flatMap((channel) => histogramBins(histogram?.[channel])),
  );
  return (
    <section className="studio-histogram" aria-label={t("studio.histogram.aria")}>
      <div className="studio-section-caption">
        <span>{t("studio.histogram.title")}</span>
        <span className="studio-histogram-toggles">
          <button
            type="button"
            className="studio-clipping-toggle"
            aria-pressed={proof}
            title={t("studio.histogram.proofTitle", { shortcut: "Shift+S" })}
            onClick={() => setEditor({ softProof: !proof })}
          >
            {t("studio.histogram.proof")}
          </button>
        </span>
      </div>
      {proof && (
        <div className="studio-proof-line">
          <select
            aria-label={t("studio.histogram.proofSpace")}
            value={proofSpace}
            onChange={(e) =>
              setEditor({ softProofSpace: e.target.value as OutputColorSpace })
            }
          >
            {COLOR_SPACES.map((space) => (
              <option key={space} value={space}>
                {COLOR_SPACE_NAMES[space]}
              </option>
            ))}
          </select>
          <button
            type="button"
            className="studio-clipping-toggle"
            aria-pressed={proofWarning}
            title={t("studio.histogram.gamutTitle")}
            onClick={() => setEditor({ softProofWarning: !proofWarning })}
          >
            {t("studio.histogram.gamut")}
          </button>
        </div>
      )}
      <div className="studio-histogram-plot">
        <svg
          viewBox="0 0 256 84"
          preserveAspectRatio="none"
          role="img"
          aria-label={
            histogram
              ? t("studio.histogram.graphAria")
              : t("studio.histogram.graphEmpty")
          }
        >
          {[64, 128, 192].map((x) => (
            <line
              key={x}
              x1={x}
              x2={x}
              y1="0"
              y2="84"
              stroke="currentColor"
              opacity=".15"
            />
          ))}
          {channels.map((channel) => {
            const values = histogramBins(histogram?.[channel]);
            if (!values.length) return null;
            return (
              <path
                key={channel}
                className={`histogram-${channel}`}
                d={`M0 84 ${values.map((v, i) => `L${(i * 256) / Math.max(1, values.length - 1)} ${84 - Math.sqrt(Math.max(0, v) / max) * 80}`).join(" ")} L256 84 Z`}
              />
            );
          })}
        </svg>
        <svg
          className="studio-clip-corners"
          viewBox="0 0 256 84"
          role="group"
          aria-label={t("studio.histogram.clippingAria")}
        >
          <ClippingCorner
            side="shadows"
            on={clipShadows}
            lit={clipped.shadows}
            label={
              clipped.shadows
                ? t("studio.histogram.shadowClippingLit")
                : t("studio.histogram.shadowClipping")
            }
            title={t("studio.histogram.shadowClippingTitle", { shortcut: "J" })}
            onToggle={() => setEditor({ clipShadows: !useEditorStore.getState().clipShadows })}
          />
          <ClippingCorner
            side="highlights"
            on={clipHighlights}
            lit={clipped.highlights}
            label={
              clipped.highlights
                ? t("studio.histogram.highlightClippingLit")
                : t("studio.histogram.highlightClipping")
            }
            title={t("studio.histogram.highlightClippingTitle", { shortcut: "J" })}
            onToggle={() => setEditor({ clipHighlights: !useEditorStore.getState().clipHighlights })}
          />
        </svg>
      </div>
      <div className="studio-exif-line">
        {image ? (
          <>
            <span>
              {image.exif?.ISO || image.exif?.PhotographicSensitivity
                ? `ISO ${image.exif.ISO || image.exif.PhotographicSensitivity}`
                : image.isRaw
                  ? "RAW"
                  : "RGB"}
            </span>
            <span>{image.exif?.FNumber || "—"}</span>
            <span>{image.exif?.ExposureTime || "—"}</span>
            <span>
              {image.exif?.FocalLength || `${image.width} × ${image.height}`}
            </span>
          </>
        ) : (
          <span>{t("studio.histogram.selectPhoto")}</span>
        )}
      </div>
    </section>
  );
});
