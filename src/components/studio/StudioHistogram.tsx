import { memo, type CSSProperties } from "react";
import { useTranslation } from "react-i18next";
import { useEditorStore } from "../../store/useEditorStore";
import { COLOR_SPACES, COLOR_SPACE_NAMES } from "../../utils/exportRecipes";
import { clippingChannelColour, histogramClipping } from "../../utils/clippingWarnings";
import {
  formatAperture,
  formatFocalLength,
  formatIso,
  formatShutter,
  parseExposureTime,
} from "../../utils/captureInfo";
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

/** One fixed-size control above the plot that switches that
 * side's warning (left: shadows; right: highlights). `lit` is the
 * histogram's own verdict: this side clips, whatever the overlay shows. */
function ClippingCorner({
  side,
  on,
  lit,
  colour,
  label,
  title,
  onToggle,
}: {
  side: ClippingSide;
  on: boolean;
  lit: boolean;
  /** The clipping channels' colour of a lit triangle; null: neutral. */
  colour: string | null;
  label: string;
  title: string;
  onToggle(): void;
}) {
  return (
    <button
      type="button"
      className={`studio-clip-corner studio-clip-${side}${on ? " is-on" : ""}${lit ? " is-lit" : ""}`}
      aria-label={label}
      aria-pressed={on}
      title={title}
      data-clipping={lit || undefined}
      style={lit && colour ? ({ "--clip-channel": colour } as CSSProperties) : undefined}
      onClick={(event) => {
        event.stopPropagation();
        onToggle();
      }}
      onKeyDown={(event) => {
        // Space otherwise reaches the global zoom shortcut. Keep native
        // button activation for Space/Enter and leave J available globally.
        if (event.key === " " || event.key === "Enter") event.stopPropagation();
      }}
    >
      <svg viewBox="0 0 12 12" aria-hidden="true">
        <polygon className="studio-clip-corner-mark" points="6,1 11.5,11 0.5,11" />
      </svg>
    </button>
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
    <section
      className="studio-histogram"
      aria-label={t("studio.histogram.aria")}
    >
      <div className="studio-clip-corners">
        <ClippingCorner
          side="shadows"
          on={clipShadows}
          lit={clipped.shadows}
          colour={clippingChannelColour(histogram, "shadows")}
          label={
            clipped.shadows
              ? t("studio.histogram.shadowClippingLit")
              : t("studio.histogram.shadowClipping")
          }
          title={t("studio.histogram.shadowClippingTitle", {
            shortcut: "J",
          })}
          onToggle={() =>
            setEditor({
              clipShadows: !useEditorStore.getState().clipShadows,
            })
          }
        />
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
        <ClippingCorner
          side="highlights"
          on={clipHighlights}
          lit={clipped.highlights}
          colour={clippingChannelColour(histogram, "highlights")}
          label={
            clipped.highlights
              ? t("studio.histogram.highlightClippingLit")
              : t("studio.histogram.highlightClipping")
          }
          title={t("studio.histogram.highlightClippingTitle", {
            shortcut: "J",
          })}
          onToggle={() =>
            setEditor({
              clipHighlights: !useEditorStore.getState().clipHighlights,
            })
          }
        />
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
      </div>
      <div className="studio-exif-line">
        {image ? (
          <>
            <span>
              {formatIso(image.exif?.ISO || image.exif?.PhotographicSensitivity) ??
                (image.isRaw ? "RAW" : "RGB")}
            </span>
            <span>{formatAperture(image.exif?.FNumber) ?? "—"}</span>
            <span>{formatShutter(parseExposureTime(image.exif?.ExposureTime)) ?? "—"}</span>
            <span>
              {formatFocalLength(image.exif?.FocalLength) ??
                `${image.width} × ${image.height}`}
            </span>
          </>
        ) : (
          <span>{t("studio.histogram.selectPhoto")}</span>
        )}
      </div>
    </section>
  );
});
