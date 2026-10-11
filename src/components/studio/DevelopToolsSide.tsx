import { useTranslation } from "react-i18next";
import { useUIStore } from "../../store/useUIStore";

/** Which side Develop's tools are on (Preferences > General); on the right,
 * the Navigator, Folders and Albums are on the left. The same setting as the
 * top bar's side button. */
export default function DevelopToolsSide() {
  const { t } = useTranslation();
  const layout = useUIStore((s) => s.studioToolLayout);
  const setLayout = useUIStore((s) => s.setStudioToolLayout);
  const sides = [
    { value: "tools-left", label: t("settings.general.developToolsLeft") },
    { value: "split-tools", label: t("settings.general.developToolsRight") },
  ] as const;
  return (
    <div
      className="studio-segments pref-segments"
      role="radiogroup"
      aria-label={t("settings.general.developTools")}
    >
      {sides.map((side) => (
        <button
          key={side.value}
          type="button"
          role="radio"
          aria-checked={layout === side.value}
          onClick={() => setLayout(side.value)}
        >
          {side.label}
        </button>
      ))}
    </div>
  );
}
