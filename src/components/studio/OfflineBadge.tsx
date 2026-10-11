import { CloudOff } from "lucide-react";
import { useTranslation } from "react-i18next";
import type { OfflinePreview } from "../ui/AppProperties";

/** Says that the open photograph's RAW is offline and which preview it is
 * shown and edited on (Lightroom marks smart-preview editing the same way). */
/** " · 2 offline" after the photo count while some listed photographs are
 * offline (missing file or disconnected drive); nothing otherwise. */
export function OfflineCount({ files }: { files: Array<{ is_offline?: boolean }> }) {
  const { t } = useTranslation();
  const count = files.reduce((sum, file) => sum + (file.is_offline ? 1 : 0), 0);
  if (!count) return null;
  return (
    <span className="studio-offline-count" title={t("offline.sessionCountTitle")}>
      {` · ${t("offline.sessionCount", { count })}`}
    </span>
  );
}

export default function OfflineBadge({ offline }: { offline?: OfflinePreview | null }) {
  const { t } = useTranslation();
  if (!offline) return null;
  const full = offline === "full";
  return (
    <span
      className="studio-offline-badge"
      role="status"
      title={full ? t("previews.offline.fullTitle") : t("previews.offline.standardTitle")}
    >
      <CloudOff size={12} />
      {full ? t("previews.offline.full") : t("previews.offline.standard")}
    </span>
  );
}
