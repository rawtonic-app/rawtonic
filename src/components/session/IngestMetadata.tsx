import { invoke } from "@tauri-apps/api/core";
import { Invokes } from "../ui/AppProperties";
import { useEffect, useId, useState } from "react";
import { useTranslation } from "react-i18next";
import { useSettingsStore } from "../../store/useSettingsStore";

/** Mirrors ingest::metadata::ImportMetadata; IPTC keys are shared with the Metadata tool. */
export interface ImportMetadata {
  creator: string;
  copyright: string;
  description: string;
  iptc: Record<string, string>;
  keywords: string[];
  includeCreatorContact: boolean;
  overwrite: string[];
}
export interface ImportMetadataPreset extends ImportMetadata { id: string; name: string }
const empty = (): ImportMetadata => ({ creator: "", copyright: "", description: "", iptc: {}, keywords: [], includeCreatorContact: false, overwrite: [] });
const contactFields = ["creatorEmail", "creatorPhone", "creatorUrl", "creatorAddress", "creatorCity", "creatorRegion", "creatorPostalCode", "creatorCountry"] as const;

export default function IngestMetadata({ value, onChange, adding = false }: { adding?: boolean; value: ImportMetadata | null; onChange: (value: ImportMetadata | null) => void }) {
  const { t } = useTranslation();
  const prefix = useId();
  const settings = useSettingsStore((s) => s.appSettings);
  const presets = settings?.importMetadataPresets ?? [];
  const [selected, setSelected] = useState("");
  const [name, setName] = useState("");
  const [keywords, setKeywords] = useState("");
  const [saving, setSaving] = useState(false);
  const [error, setError] = useState<string | null>(null);
  useEffect(() => { if (!value) setSelected(""); }, [value]);
  const change = (patch: Partial<ImportMetadata>) => onChange({ ...(value ?? empty()), ...patch });
  const select = (id: string) => {
    setSelected(id);
    setError(null);
    const preset = presets.find((p) => p.id === id);
    setName(preset?.name ?? "");
    setKeywords(preset?.keywords.join(", ") ?? "");
    onChange(id === "" ? null : preset ? structuredClone(preset) : empty());
  };
  const persist = async (next: ImportMetadataPreset[]) => {
    setSaving(true);
    setError(null);
    try {
      const settings = { ...useSettingsStore.getState().appSettings!, importMetadataPresets: next };
      await invoke(Invokes.SaveSettings, { settings });
      useSettingsStore.getState().setAppSettings(settings);
      return true;
    } catch (e) { setError(String(e)); return false; }
    finally { setSaving(false); }
  };
  const save = async () => {
    if (!value || !name.trim() || !settings) return;
    const id = selected === "new" ? crypto.randomUUID() : selected;
    if (await persist([...presets.filter((p) => p.id !== id), { ...value, id, name: name.trim() }])) setSelected(id);
  };
  const field = (key: string, label: string) => {
    const id = `${prefix}-${key}`;
    const text = key === "creator" || key === "copyright" || key === "description" ? value![key] : value!.iptc[key] ?? "";
    return <div className="ingest-metadata-field" key={key}>
      <label className="session-field" htmlFor={id}><span>{label}</span>
        <input id={id} value={text} onChange={(e) => key === "creator" || key === "copyright" || key === "description"
          ? change({ [key]: e.target.value }) : change({ iptc: { ...value!.iptc, [key]: e.target.value } })} />
      </label>
      <label className="ingest-metadata-overwrite"><input type="checkbox" aria-label={t("session.ingest.metadata.overwriteField", { field: label })}
        checked={value!.overwrite.includes(key)} onChange={(e) => change({ overwrite: e.target.checked
          ? [...value!.overwrite, key] : value!.overwrite.filter((k) => k !== key) })} />{t("session.ingest.metadata.overwrite")}</label>
    </div>;
  };
  return <section className="session-ingest-metadata" aria-labelledby={`${prefix}-title`}>
    <h3 id={`${prefix}-title`}>{t("session.ingest.metadata.title")}</h3>
    <label className="session-field"><span>{t("session.ingest.metadata.preset")}</span>
      <select value={selected} onChange={(e) => select(e.target.value)} disabled={saving}>
        <option value="">{t("session.ingest.metadata.none")}</option>
        {presets.map((p) => <option key={p.id} value={p.id}>{p.name}</option>)}
        <option value="new">{t("session.ingest.metadata.new")}</option>
      </select>
    </label>
    {adding && <p className="ingest-metadata-hint">{t("session.ingest.metadata.addNote")}</p>}
    {value && <>
      <p className="ingest-metadata-hint">{t("session.ingest.metadata.fillEmpty")}</p>
      <label className="session-field"><span>{t("session.ingest.metadata.name")}</span><input value={name} onChange={(e) => setName(e.target.value)} /></label>
      <div className="ingest-metadata-fields">
        {field("description", t("studio.metadata.fields.description"))}
        {field("creator", t("session.ingest.metadata.creator"))}
        {field("copyright", t("session.ingest.metadata.copyright"))}
        {field("usageTerms", t("studio.metadata.iptc.fields.usageTerms"))}
        {field("credit", t("studio.metadata.iptc.fields.credit"))}
      </div>
      <p className="ingest-metadata-hint">{t("session.ingest.metadata.yearHint")}</p>
      <label className="session-field"><span>{t("session.ingest.metadata.keywords")}</span><input aria-label={t("session.ingest.metadata.keywords")} value={keywords} onChange={(e) => {
        setKeywords(e.target.value);
        change({ keywords: [...new Set(e.target.value.split(",").map((k) => k.trim()).filter(Boolean))] });
      }} /><small>{t("session.ingest.metadata.mergeKeywords")}</small></label>
      <label className="session-ingest-album"><input type="checkbox" checked={value.includeCreatorContact}
        onChange={(e) => change({ includeCreatorContact: e.target.checked })} />{t("session.ingest.metadata.contacts")}</label>
      {value.includeCreatorContact && <div className="ingest-metadata-fields">{contactFields.map((key) => field(key, t(`studio.metadata.iptc.fields.${key}`)))}</div>}
      <div className="ingest-metadata-actions">
        <button className="session-action" type="button" disabled={saving || !name.trim() || !settings} onClick={() => void save()}>{t("session.ingest.metadata.save")}</button>
        {selected !== "new" && <button className="session-action" type="button" disabled={saving} onClick={async () => {
          if (await persist(presets.filter((p) => p.id !== selected))) select("");
        }}>{t("session.ingest.metadata.delete")}</button>}
      </div>
      {error && <p className="session-error" role="alert">{error}</p>}
    </>}
  </section>;
}
