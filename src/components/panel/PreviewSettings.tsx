import { useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { useTranslation } from "react-i18next";
import Button from "../ui/Button";
import Dropdown, { type OptionItem } from "../ui/Dropdown";
import { type AppSettings, Invokes } from "../ui/AppProperties";
import { useLibraryStore } from "../../store/useLibraryStore";

interface Props {
  appSettings: AppSettings | null;
  onSettingsChange(settings: AppSettings): Promise<void> | void;
  onOpenStorage(): void;
}

/** Preview construction and expiry; disk use, limits and clearing have one
 * home in Storage. */
export default function PreviewSettings({
  appSettings,
  onSettingsChange,
  onOpenStorage,
}: Props) {
  const { t } = useTranslation();
  const folder = useLibraryStore(
    (state) => state.sessionDirectory ?? state.currentFolderPath,
  );
  const [error, setError] = useState(false);
  const sizeOptions: OptionItem<number>[] = [
    { label: t("previews.settings.auto"), value: 0 },
    ...[2048, 2560, 3072, 3840].map((size) => ({
      label: `${size} px`,
      value: size,
    })),
  ];
  const discardOptions: OptionItem<number>[] = [
    { label: t("previews.settings.afterDay"), value: 1 },
    { label: t("previews.settings.afterWeek"), value: 7 },
    { label: t("previews.settings.after30Days"), value: 30 },
    { label: t("previews.settings.never"), value: 0 },
  ];
  const change = async (patch: Partial<AppSettings>) => {
    if (!appSettings) return;
    try {
      await onSettingsChange({ ...appSettings, ...patch });
      if ("discardFullPreviewsDays" in patch)
        await invoke(Invokes.ApplyPreviewLimits, { folder });
      setError(false);
    } catch {
      setError(true);
    }
  };
  return (
    <>
      <div className="pref-row">
        <div className="pref-text">
          <div className="pref-label">
            {t("previews.settings.standardSize")}
          </div>
          <p className="pref-description">
            {t("previews.settings.standardSizeDesc")}
          </p>
        </div>
        <div className="pref-control">
          <Dropdown
            onChange={(value) => void change({ standardPreviewSize: value })}
            options={sizeOptions}
            value={appSettings?.standardPreviewSize ?? 0}
            triggerClassName="bg-bg-primary"
          />
        </div>
      </div>
      <div className="pref-row">
        <div className="pref-text">
          <div className="pref-label">{t("previews.settings.discardFull")}</div>
          <p className="pref-description">
            {t("previews.settings.discardFullDesc")}
          </p>
        </div>
        <div className="pref-control">
          <Dropdown
            onChange={(value) =>
              void change({ discardFullPreviewsDays: value })
            }
            options={discardOptions}
            value={appSettings?.discardFullPreviewsDays ?? 30}
            triggerClassName="bg-bg-primary"
          />
        </div>
      </div>
      <Button onClick={onOpenStorage}>{t("storage.manage")}</Button>
      {error && <p role="alert">{t("storage.failed")}</p>}
    </>
  );
}
