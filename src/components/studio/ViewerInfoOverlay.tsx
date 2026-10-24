import { useMemo } from "react";
import { useTranslation } from "react-i18next";
import { Flag, Star, X } from "lucide-react";
import { useLibraryStore } from "../../store/useLibraryStore";
import { captureInfoParts } from "../../utils/captureInfo";
import { keywordOf, useShownPhoto, useTextMetadataCache } from "./StudioMetadata";

/**
 * The photograph's facts over the viewer, read only (Lightroom's info
 * overlay, I): name, title, capture, camera and lens, rating, flag and
 * keywords. Shown in Develop and the full screen, where the Library's
 * Metadata and Keywords tools are not; editing them stays in the Library.
 */
export default function ViewerInfoOverlay() {
  const { t } = useTranslation();
  const { path, shown, exif } = useShownPhoto();
  const rating = useLibraryStore((s) => (path ? (s.imageRatings[path] ?? 0) : 0));
  const flag = useLibraryStore((s) => (path ? (s.imageFlags[path] ?? 0) : 0));
  const tags = useLibraryStore((s) => s.imageList.find((image) => image.path === path)?.tags);
  // The title as the Metadata tool last read it (no read of its own here).
  const title = useTextMetadataCache((s) => (path ? s.byPath[path.split("?vc=")[0]]?.title : undefined));
  const keywords = useMemo(
    () =>
      (tags ?? [])
        .map(keywordOf)
        .filter((keyword): keyword is string => !!keyword)
        .sort((a, b) => a.localeCompare(b)),
    [tags],
  );
  if (!path) return null;
  const name = path.split("?vc=")[0].split(/[\\/]/).pop();
  const camera = [exif.Make, exif.Model]
    .filter(Boolean)
    .join(" ")
    .replace(/^(\S+) \1 /i, "$1 ");
  const capture = captureInfoParts(exif).join(" · ");
  const size = shown?.width && shown.height ? `${shown.width} × ${shown.height}` : "";
  return (
    <aside className="studio-info-overlay" aria-label={t("studio.workspace.info.label")}>
      <strong className="studio-info-name">
        {name}
        {size && <span className="studio-muted">{` · ${size}`}</span>}
      </strong>
      {title && <span>{title}</span>}
      {exif.DateTimeOriginal && <span>{exif.DateTimeOriginal}</span>}
      {(camera || exif.LensModel) && <span>{[camera, exif.LensModel].filter(Boolean).join(" · ")}</span>}
      {capture && <span>{capture}</span>}
      {(rating > 0 || flag !== 0) && (
        <span className="studio-info-marks">
          {rating > 0 && (
            <span role="img" aria-label={t("studio.library.browser.stars", { count: rating })}>
              {Array.from({ length: rating }, (_, i) => (
                <Star key={i} size={11} fill="currentColor" />
              ))}
            </span>
          )}
          {flag === 1 && <Flag size={11} fill="currentColor" aria-label={t("studio.library.browser.picked")} />}
          {flag === -1 && <X size={12} aria-label={t("studio.library.browser.rejected")} />}
        </span>
      )}
      <span className="studio-info-keywords">
        {keywords.length
          ? t("studio.workspace.info.keywords", { keywords: keywords.join(", ") })
          : t("studio.workspace.info.noKeywords")}
      </span>
    </aside>
  );
}
