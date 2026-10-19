import { useTranslation } from "react-i18next";
import type { NextCaptureForm, PresetChoice } from "./nextCapture";

/** What new captures start with (Capture One's Next Capture Adjustments
 * and Metadata): adjustments, rating and keywords — for the hot folder and
 * the tether alike. */
export default function NextCaptureFields({
  value,
  onChange,
  presets,
}: {
  value: NextCaptureForm;
  onChange: (value: NextCaptureForm) => void;
  presets: PresetChoice[];
}) {
  const { t } = useTranslation();
  return (
    <>
      <label className="session-field">
        {t("session.hotFolder.nextCapture")}
        <select
          aria-label={t("session.hotFolder.nextCapture")}
          value={value.choice}
          onChange={(e) => onChange({ ...value, choice: e.target.value })}
        >
          <option value="defaults">{t("session.hotFolder.defaults")}</option>
          <option value="copyFromLast">{t("session.hotFolder.copyFromLast")}</option>
          {presets.map((p) => (
            <option key={p.id} value={`preset:${p.id}`}>
              {t("session.hotFolder.preset", { name: p.name })}
            </option>
          ))}
        </select>
      </label>
      <label className="session-field">
        {t("session.hotFolder.rating")}
        <select
          aria-label={t("session.hotFolder.rating")}
          value={value.rating}
          onChange={(e) => onChange({ ...value, rating: Number(e.target.value) })}
        >
          <option value={0}>{t("session.hotFolder.noRating")}</option>
          {[1, 2, 3, 4, 5].map((stars) => (
            <option key={stars} value={stars}>
              {"★".repeat(stars)}
            </option>
          ))}
        </select>
      </label>
      <label className="session-field">
        {t("session.hotFolder.keywords")}
        <input
          aria-label={t("session.hotFolder.keywords")}
          value={value.keywords}
          placeholder={t("session.hotFolder.keywordsPlaceholder")}
          spellCheck={false}
          onChange={(e) => onChange({ ...value, keywords: e.target.value })}
        />
      </label>
    </>
  );
}
