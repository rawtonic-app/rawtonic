import { Invokes } from '../ui/AppProperties';
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
  /** The pack (folder) an imported LUT came with; null for a single file. */
  group?: string | null;
}

interface LutImport {
  luts: LutEntry[];
  imported: number;
  skipped: { file: string; reason: string }[];
}

const EXTENSIONS = ["cube", "3dl", "png", "jpg", "jpeg", "tiff"];

/** The imported LUTs by pack, single files first; groups by name. */
function importedGroups(entries: LutEntry[]): [string | null, LutEntry[]][] {
  const groups = new Map<string | null, LutEntry[]>();
  for (const entry of entries) {
    if (entry.isBuiltIn) continue;
    const key = entry.group ?? null;
    groups.set(key, [...(groups.get(key) ?? []), entry]);
  }
  return [...groups].sort(([a], [b]) =>
    a === null ? -1 : b === null ? 1 : a.localeCompare(b, undefined, { sensitivity: "base" }),
  );
}

/**
 * A 3D LUT with an amount (Capture One's LUT tool; Lightroom reaches LUTs
 * only through creative profiles, whose Amount runs to 200 %): the built-in
 * looks, LUTs imported by the user (.cube, .3dl or a HALD image; several
 * files or a folder become a pack), or none. Above 100 % the LUT's change
 * is extrapolated (engine `lut::apply_amount`).
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
    invoke<LutEntry[]>(Invokes.ListLuts)
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

  const importLuts = async (folder: boolean) => {
    try {
      const selected = folder
        ? await open({ directory: true, multiple: true })
        : await open({
            multiple: true,
            filters: [{ name: "LUT", extensions: [...EXTENSIONS, ...EXTENSIONS.map((e) => e.toUpperCase())] }],
          });
      const sourcePaths = Array.isArray(selected) ? selected : selected ? [selected] : [];
      if (sourcePaths.length === 0) return;
      const result = await invoke<LutImport>(Invokes.ImportLuts, { sourcePaths });
      setEntries(result?.luts ?? []);
      const skipped = result?.skipped ?? [];
      if (skipped.length > 0) {
        const files = skipped.map((s) => `${s.file}: ${s.reason}`).join("\n");
        toast.warn(t("studio.lut.toasts.skipped", { count: skipped.length, imported: result.imported, files }));
      } else if (result && result.imported === 0) {
        toast.info(t("studio.lut.toasts.noneFound"));
      }
    } catch (error) {
      console.error("Importing LUTs failed:", error);
      toast.error(t("studio.lut.toasts.importFailed"));
    }
  };

  // An imported LUT is Rawtonic's own copy; removing it never touches the
  // file it was imported from.
  const removeLut = async (path: string) => {
    try {
      const list = await invoke<LutEntry[]>(Invokes.RemoveLut, { path });
      setEntries(list ?? []);
      clear();
    } catch (error) {
      console.error("Removing the LUT failed:", error);
      toast.error(t("studio.lut.toasts.removeFailed"));
    }
  };
  const known = entries.some((e) => e.path === lutPath);
  const imported = entries.find((e) => e.path === lutPath && !e.isBuiltIn);
  const builtIn = entries.filter((e) => e.isBuiltIn);
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
            {builtIn.length > 0 && (
              <optgroup label={t("studio.lut.builtIn")}>
                {builtIn.map((e) => (
                  <option key={e.path} value={e.path}>
                    {e.name}
                  </option>
                ))}
              </optgroup>
            )}
            {importedGroups(entries).map(([group, list]) => (
              <optgroup key={group ?? ""} label={group ?? t("studio.lut.imported")}>
                {list.map((e) => (
                  <option key={e.path} value={e.path}>
                    {e.name}
                  </option>
                ))}
              </optgroup>
            ))}
          </select>
        </label>
        <button title={t("studio.lut.importTitle")} disabled={disabled} onClick={() => void importLuts(false)}>
          {t("studio.lut.import")}
        </button>
        <button title={t("studio.lut.importFolderTitle")} disabled={disabled} onClick={() => void importLuts(true)}>
          {t("studio.lut.importFolder")}
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
          max={200}
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
