import { invoke } from "@tauri-apps/api/core";
import { open } from "@tauri-apps/plugin-dialog";
import { useEffect, useState } from "react";
import { useTranslation } from "react-i18next";
import OperationError from "../ui/OperationError";
import { Invokes } from "../ui/AppProperties";

export interface CameraProfileEntry {
  value: string;
  name: string;
  camera: string;
}

const IMPORT = "__import__";

/**
 * Camera profile choice (Base characteristics): Rawtonic's own colour or a
 * DCP the photographer imported (e.g. from a ColorChecker). Imported files
 * are copied into the session's `profiles/` folder, so the session stays
 * portable. Rawtonic never picks a profile by itself.
 */
export default function StudioCameraProfile({
  value,
  imagePath,
  disabled,
  onChange,
}: {
  value: string | undefined;
  imagePath: string | undefined;
  disabled: boolean;
  onChange(value: string | undefined): void;
}) {
  const { t } = useTranslation();
  const [profiles, setProfiles] = useState<CameraProfileEntry[]>([]);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    let current = true;
    if (!imagePath) {
      setProfiles([]);
      return;
    }
    invoke<CameraProfileEntry[]>(Invokes.ListCameraProfiles, { imagePath })
      .then((list) => current && setProfiles(Array.isArray(list) ? list : []))
      .catch(() => current && setProfiles([]));
    return () => {
      current = false;
    };
  }, [imagePath]);

  const importProfile = async () => {
    if (!imagePath) return;
    try {
      const source = await open({
        multiple: false,
        filters: [{ name: t("studio.profile.dcpFilter"), extensions: ["dcp", "DCP"] }],
      });
      if (typeof source !== "string") return;
      const entry = await invoke<CameraProfileEntry>(
        Invokes.ImportCameraProfile,
        { source, imagePath },
      );
      setError(null);
      setProfiles((list) =>
        list.some((p) => p.value === entry.value) ? list : [...list, entry],
      );
      onChange(entry.value);
    } catch (e) {
      setError(String(e));
    }
  };

  const selected = profiles.find((p) => p.value === value);
  return (
    <>
      <label className="studio-inline">
        <span>{t("studio.profile.label")}</span>
        <select
          aria-label={t("studio.profile.aria")}
          value={value ?? ""}
          disabled={disabled}
          onChange={(e) => {
            if (e.target.value === IMPORT) {
              void importProfile();
            } else {
              onChange(e.target.value || undefined);
            }
          }}
        >
          <option value="">Rawtonic</option>
          {value && !selected && <option value={value}>{value}</option>}
          {profiles.map((p) => (
            <option key={p.value} value={p.value}>
              {p.name}
            </option>
          ))}
          <option value={IMPORT}>{t("studio.profile.importDcp")}</option>
        </select>
      </label>
      {selected?.camera && (
        <div className="studio-hint">{t("studio.profile.forCamera", { camera: selected.camera })}</div>
      )}
      {error && (
        <div className="studio-hint" role="alert">
          <OperationError detail={error} />
        </div>
      )}
    </>
  );
}
