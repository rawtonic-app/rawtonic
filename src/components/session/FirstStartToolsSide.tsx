import { useEffect, useId, useState } from "react";
import { invoke, isTauri } from "@tauri-apps/api/core";
import { PanelLeft, PanelRight } from "lucide-react";
import { useTranslation } from "react-i18next";
import { storedStudioLayout, useUIStore } from "../../store/useUIStore";
import { useSettingsStore } from "../../store/useSettingsStore";
import { Invokes } from "../ui/AppProperties";

/** Ask only after history confirms a new installation; even an offline last
 * session counts as prior use. A failed history read must never prompt users. */
export default function FirstStartToolsSide() {
  const { t } = useTranslation();
  const id = useId();
  const [candidate] = useState(() => storedStudioLayout("layout") === null);
  const [firstStart, setFirstStart] = useState(!isTauri() && candidate);
  const layout = useUIStore((s) => s.studioToolLayout);
  const setLayout = useUIStore((s) => s.setStudioToolLayout);
  const hasFolders = useSettingsStore((s) =>
    Boolean(s.appSettings?.rootFolders?.length),
  );
  useEffect(() => {
    if (!candidate || !isTauri()) return;
    let current = true;
    void invoke<string | null>(Invokes.SessionLastDirectory)
      .then((directory) => {
        if (current) setFirstStart(directory === null);
      })
      .catch(() => {});
    return () => {
      current = false;
    };
  }, [candidate]);
  if (!firstStart || hasFolders) return null;
  return (
    <fieldset className="session-tools-side" aria-describedby={`${id}-hint`}>
      <legend>{t("session.welcome.toolsSide")}</legend>
      <div className="session-tools-side-options">
        {(
          [
            ["tools-left", t("session.welcome.toolsLeft"), PanelLeft],
            ["split-tools", t("session.welcome.toolsRight"), PanelRight],
          ] as const
        ).map(([value, label, Icon]) => (
          <label key={value}>
            <input
              type="radio"
              name={`${id}-side`}
              value={value}
              checked={layout === value}
              onChange={() => setLayout(value)}
              onClick={() => {
                if (layout === value) setLayout(value);
              }}
            />
            <Icon size={24} aria-hidden />
            <span>{label}</span>
          </label>
        ))}
      </div>
      <p id={`${id}-hint`}>{t("session.welcome.toolsHint")}</p>
    </fieldset>
  );
}
