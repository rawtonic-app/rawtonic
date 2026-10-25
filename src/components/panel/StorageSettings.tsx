// SPDX-License-Identifier: AGPL-3.0-or-later
import { useCallback, useEffect, useRef, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { useTranslation } from "react-i18next";
import { type AppSettings, Invokes } from "../ui/AppProperties";
import { useLibraryStore } from "../../store/useLibraryStore";
import { formatFileSize } from "../../utils/formatFileSize";
import Button from "../ui/Button";
import ConfirmModal from "../modals/ConfirmModal";

type Category =
  | "thumbnails"
  | "previews"
  | "decoded"
  | "aiResults"
  | "models"
  | "compiledModels"
  | "logs"
  | "scratch"
  | "session"
  | "exif"
  | "systemModels";
interface StorageEntry {
  category: Category;
  bytes: number | null;
  files: number;
  path: string | null;
  incomplete: boolean;
  clearable: boolean;
  pendingRemoval?: boolean;
  error: boolean;
}
interface Props {
  appSettings: AppSettings;
  onSettingsChange(settings: AppSettings): Promise<void>;
  onLibraryRefresh(): void;
}

export default function StorageSettings({
  appSettings,
  onSettingsChange,
  onLibraryRefresh,
}: Props) {
  const { t } = useTranslation();
  const library = useLibraryStore((s) => s.sessionDirectory);
  const [entries, setEntries] = useState<StorageEntry[]>([]);
  const [busy, setBusy] = useState(false);
  const [scanning, setScanning] = useState(false);
  const [error, setError] = useState(false);
  const [modelsConfirm, setModelsConfirm] = useState(false);
  const [modelsPending, setModelsPending] = useState(false);
  const [cleared, setCleared] = useState(false);
  const request = useRef<string | null>(null);
  const mounted = useRef(false);

  const cancel = useCallback(() => {
    if (!request.current) return;
    void invoke(Invokes.StorageCancelScan, {
      requestId: request.current,
    }).catch(() => {});
    request.current = null;
  }, []);
  const refresh = useCallback(async () => {
    cancel();
    const requestId = crypto.randomUUID();
    request.current = requestId;
    setScanning(true);
    try {
      const result = await invoke<StorageEntry[]>(Invokes.StorageUsage, {
        requestId,
      });
      if (mounted.current && request.current === requestId) setEntries(result);
    } catch {
      if (mounted.current && request.current === requestId) setError(true);
    } finally {
      if (mounted.current && request.current === requestId) {
        request.current = null;
        setScanning(false);
      }
    }
  }, [cancel]);
  useEffect(() => {
    mounted.current = true;
    void refresh();
    return () => {
      mounted.current = false;
      cancel();
    };
  }, [refresh, cancel, library]);

  const clear = async (category: Category) => {
    setBusy(true);
    setError(false);
    setCleared(false);
    try {
      await invoke(Invokes.StorageClear, {
        category,
        confirmed: category === "models",
        ...(category === "session" ? { sessionDirectory: library } : {}),
      });
      if (!mounted.current) return;
      if (category === "models") setModelsPending(true);
      else {
        setCleared(true);
        onLibraryRefresh();
        await refresh();
      }
    } catch {
      if (mounted.current) setError(true);
    } finally {
      if (mounted.current) setBusy(false);
    }
  };
  const changeLimit = async (
    key:
      | "thumbnailCacheLimitGb"
      | "aiCacheLimitGb"
      | "standardPreviewsLimitGb"
      | "imageCacheSize",
    value: number,
  ) => {
    setBusy(true);
    setError(false);
    try {
      await onSettingsChange({ ...appSettings, [key]: value });
      if (key === "standardPreviewsLimitGb")
        await invoke(Invokes.ApplyPreviewLimits, { folder: library });
      else await invoke(Invokes.StorageApplyLimits);
      if (mounted.current) await refresh();
    } catch {
      if (mounted.current) setError(true);
    } finally {
      if (mounted.current) setBusy(false);
    }
  };
  const limit = (category: Category) => {
    const spec = {
      thumbnails: {
        key: "thumbnailCacheLimitGb",
        label: t("storage.thumbnailLimit"),
        value: appSettings.thumbnailCacheLimitGb ?? 2,
      },
      aiResults: {
        key: "aiCacheLimitGb",
        label: t("storage.aiLimit"),
        value: appSettings.aiCacheLimitGb ?? 5,
      },
      previews: {
        key: "standardPreviewsLimitGb",
        label: t("previews.settings.standardLimit"),
        value: appSettings.standardPreviewsLimitGb ?? 10,
      },
      decoded: {
        key: "imageCacheSize",
        label: t("settings.processing.imageCache"),
        value: appSettings.imageCacheSize ?? 5,
      },
    } as const;
    if (!(category in spec)) return null;
    const setting = spec[category as keyof typeof spec];
    const options =
      category === "decoded" ? [0, 1, 2, 3, 5, 10] : [0, 1, 2, 5, 10, 20, 50];
    if (!options.includes(setting.value)) options.push(setting.value);
    return (
      <label className="flex items-center gap-2 text-sm">
        <span>{setting.label}</span>
        <select
          aria-label={setting.label}
          disabled={busy}
          value={setting.value}
          className="rounded border border-border-color bg-bg-primary p-2"
          onChange={(event) =>
            void changeLimit(setting.key, Number(event.target.value))
          }
        >
          {options
            .sort((a, b) => a - b)
            .map((value) => (
              <option key={value} value={value}>
                {category === "decoded"
                  ? t("storage.images", { count: value })
                  : value === 0
                    ? t("previews.settings.noLimit")
                    : `${value} GB`}
              </option>
            ))}
        </select>
      </label>
    );
  };

  return (
    <section className="pref-section">
      <h2 className="pref-section-title">{t("storage.title")}</h2>
      <p className="pref-description">{t("storage.description")}</p>
      <p className="pref-description">{t("storage.limitsDescription")}</p>
      <div className="my-4 flex gap-2">
        <Button
          disabled={busy || scanning}
          onClick={() => {
            setError(false);
            void refresh();
          }}
        >
          {t("storage.refresh")}
        </Button>
        {scanning && (
          <Button
            onClick={() => {
              cancel();
              setScanning(false);
            }}
          >
            {t("storage.cancel")}
          </Button>
        )}
        {scanning && <span role="status">{t("storage.scanning")}</span>}
      </div>
      {error && <p role="alert">{t("storage.failed")}</p>}
      {cleared && <p role="status">{t("storage.cleared")}</p>}
      {(modelsPending || entries.some((entry) => entry.pendingRemoval)) && (
        <p role="status">{t("storage.modelsPending")}</p>
      )}
      {entries.map((entry) => (
        <div className="pref-row" key={entry.category}>
          <div className="pref-text">
            <h3 className="pref-label">
              {t(`storage.categories.${entry.category}.title`)}
            </h3>
            <p className="pref-description">
              {t(`storage.categories.${entry.category}.description`)}
            </p>
            <p>
              {entry.bytes === null
                ? t("storage.unavailable")
                : entry.incomplete
                  ? t("storage.atLeast", { size: formatFileSize(entry.bytes) })
                  : formatFileSize(entry.bytes)}
            </p>
            {entry.path && (
              <p className="pref-description break-all font-mono">
                {entry.path}
              </p>
            )}
          </div>
          <div className="pref-control flex flex-col items-end gap-2">
            {limit(entry.category)}
            {entry.clearable && (
              <Button
                disabled={
                  busy ||
                  entry.bytes === 0 ||
                  (entry.category === "models" &&
                    (modelsPending || entry.pendingRemoval))
                }
                onClick={() =>
                  entry.category === "models"
                    ? setModelsConfirm(true)
                    : void clear(entry.category)
                }
              >
                {entry.category === "models"
                  ? t("storage.removeModels")
                  : t("storage.clear")}
              </Button>
            )}
          </div>
        </div>
      ))}
      <ConfirmModal
        isOpen={modelsConfirm}
        title={t("storage.removeModels")}
        message={t("storage.modelsConfirm")}
        confirmText={t("storage.confirmRemove")}
        confirmVariant="destructive"
        onClose={() => setModelsConfirm(false)}
        onConfirm={() => void clear("models")}
      />
    </section>
  );
}
