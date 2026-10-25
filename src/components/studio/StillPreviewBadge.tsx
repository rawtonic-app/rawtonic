import { Image } from "lucide-react";
import { useTranslation } from "react-i18next";
import { useEditorStore } from "../../store/useEditorStore";

/** Marks the open photograph as shown from its thumbnail (a still preview)
 * until its first developed frame replaces it in the same place. */
export default function StillPreviewBadge({ path }: { path?: string }) {
  const { t } = useTranslation();
  const still = useEditorStore((s) => Boolean(path) && s.stillPreview === path);
  if (!still) return null;
  return (
    <span className="studio-offline-badge studio-still-badge" role="status" title={t("previews.still.title")}>
      <Image size={12} />
      {t("previews.still.label")}
    </span>
  );
}
