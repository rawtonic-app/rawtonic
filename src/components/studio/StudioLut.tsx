import { useEffect, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { open } from "@tauri-apps/plugin-dialog";
import { toast } from "react-toastify";
import { useTranslation } from "react-i18next";
import { useEditorStore } from "../../store/useEditorStore";
import { useEditorActions } from "../../hooks/useEditorActions";
import AdjustmentControl from "./AdjustmentControl";

interface LutEntry {
  name: string;
  path: string;
  isBuiltIn: boolean;
}

const EXTENSIONS = ["cube", "3dl", "png", "jpg", "jpeg", "tiff"];

/**
 * A 3D LUT with an amount (Capture One's LUT tool; Lightroom reaches LUTs
 * only through creative profiles): the built-in looks, LUTs imported by the
 * user (.cube, .3dl or a HALD image), or none.
 */
export default function StudioLut({
  disabled,
  onInteraction,
}: {
  disabled: boolean;
  onInteraction(active: boolean): void;
}) {
  const { t } = useTranslation();
  const lutPath = useEditorStore((s) => s.adjustments.lutPath ?? null);
  const intensity = useEditorStore((s) => s.adjustments.lutIntensity ?? 100);
  const { handleLutSelect, setAdjustments } = useEditorActions();
  const [entries, setEntries] = useState<LutEntry[]>([]);

  useEffect(() => {
    let active = true;
    invoke<LutEntry[]>("list_luts")
      .then((list) => active && setEntries(list ?? []))
      .catch((error) => console.error("Listing LUTs failed:", error));
    return () => {
      active = false;
    };
  }, []);

  const clear = () =>
    setAdjustments((prev) => ({
      ...prev,
      lutPath: null,
      lutName: null,
      lutData: null,
      lutSize: 0,
      lutIntensity: 100,
      lutIsSceneReferred: false,
    }));

  const importLuts = async () => {
    try {
      const selected = await open({
        multiple: true,
        filters: [{ name: "LUT", extensions: [...EXTENSIONS, ...EXTENSIONS.map((e) => e.toUpperCase())] }],
      });
      const sourcePaths = Array.isArray(selected) ? selected : selected ? [selected] : [];
      if (sourcePaths.length === 0) return;
      const list = await invoke<LutEntry[]>("import_luts", { sourcePaths });
      setEntries(list ?? []);
    } catch (error) {
      console.error("Importing LUTs failed:", error);
      toast.error(t("studio.lut.toasts.importFailed"));
    }
  };

  // An imported LUT is Rawtonic's own copy; removing it never touches the
  // file it was imported from.
  const removeLut = async (path: string) => {
    try {
      const list = await invoke<LutEntry[]>("remove_lut", { path });
      setEntries(list ?? []);
      clear();
    } catch (error) {
      console.error("Removing the LUT failed:", error);
      toast.error(t("studio.lut.toasts.removeFailed"));
    }
  };
  const known = entries.some((e) => e.path === lutPath);
  const imported = entries.find((e) => e.path === lutPath && !e.isBuiltIn);
  return (
    <>
      <div className="studio-inline">
        <label>
          LUT
          <select
            aria-label="LUT"
            value={lutPath ?? ""}
            disabled={disabled}
            onChange={(e) => {
              const entry = entries.find((x) => x.path === e.target.value);
              if (!entry) clear();
              else void handleLutSelect(entry.path, entry.isBuiltIn);
            }}
          >
            <option value="">{t("studio.lut.none")}</option>
            {lutPath && !known && <option value={lutPath}>{lutPath.split(/[\\/]/).pop()}</option>}
            {entries.filter((e) => e.isBuiltIn).length > 0 && (
              <optgroup label={t("studio.lut.builtIn")}>
                {entries
                  .filter((e) => e.isBuiltIn)
                  .map((e) => (
                    <option key={e.path} value={e.path}>
                      {e.name}
                    </option>
                  ))}
              </optgroup>
            )}
            {entries.filter((e) => !e.isBuiltIn).length > 0 && (
              <optgroup label={t("studio.lut.imported")}>
                {entries
                  .filter((e) => !e.isBuiltIn)
                  .map((e) => (
                    <option key={e.path} value={e.path}>
                      {e.name}
                    </option>
                  ))}
              </optgroup>
            )}
          </select>
        </label>
        <button title={t("studio.lut.importTitle")} disabled={disabled} onClick={() => void importLuts()}>
          {t("studio.lut.import")}
        </button>
        {imported && (
          <button title={t("studio.lut.removeTitle")} onClick={() => void removeLut(imported.path)}>
            {t("studio.lut.remove")}
          </button>
        )}
      </div>
      {lutPath && (
        <AdjustmentControl
          label={t("studio.lut.amount")}
          ariaLabel={t("studio.lut.aria.amount")}
          min={0}
          max={100}
          value={intensity}
          defaultValue={100}
          disabled={disabled}
          onInteraction={onInteraction}
          onChange={(lutIntensity) => setAdjustments((prev) => ({ ...prev, lutIntensity }))}
        />
      )}
    </>
  );
}
