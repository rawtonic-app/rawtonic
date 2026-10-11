import { memo } from "react";
import { useTranslation } from "react-i18next";
import { useEditorStore } from "../../store/useEditorStore";
import { COLOR_SPACES, COLOR_SPACE_NAMES } from "../../utils/exportRecipes";
import type { OutputColorSpace } from "../ui/ExportImportProperties";

export function histogramBins(channel: unknown): number[] {
  const bins = Array.isArray(channel)
    ? channel
    : (channel as { data?: unknown })?.data;
  return Array.isArray(bins)
    ? bins.map((value) => (Number.isFinite(value) ? Math.max(0, value) : 0))
    : [];
}

export default memo(function StudioHistogram() {
  const { t } = useTranslation();
  const histogram = useEditorStore((s) => s.histogram);
  const image = useEditorStore((s) => s.selectedImage);
  const clipping = useEditorStore((s) => s.clippingWarnings);
  const proof = useEditorStore((s) => s.softProof);
  const proofSpace = useEditorStore((s) => s.softProofSpace);
  const proofWarning = useEditorStore((s) => s.softProofWarning);
  const setEditor = useEditorStore((s) => s.setEditor);
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
            aria-pressed={clipping}
            title={t("studio.histogram.clippingTitle", { shortcut: "J" })}
            onClick={() => setEditor({ clippingWarnings: !clipping })}
          >
            {t("studio.histogram.clipping")}
          </button>
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
