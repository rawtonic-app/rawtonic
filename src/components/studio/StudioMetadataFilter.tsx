import { useEffect, useMemo, useState } from "react";
import { useTranslation } from "react-i18next";
import type { ParseKeys, TFunction } from "i18next";
import { useShallow } from "zustand/react/shallow";
import { useLibraryStore } from "../../store/useLibraryStore";
import { useSettingsStore } from "../../store/useSettingsStore";
import { onCatalogChange } from "../../utils/catalogWrites";
import {
  FACET_FIELDS,
  cullingRules,
  libraryFacets,
  scopeRule,
  textRules,
  type FacetField,
  type FacetValue,
  type Facets,
} from "../../utils/librarySearch";

/** How long changes pause before the value lists are counted again. */
const COUNT_DELAY_MS = 120;

/** A list value as the Library shows it ("" is the photographs without one). */
export function facetLabel(
  field: FacetField,
  value: string,
  t: TFunction,
  locale?: string,
): string {
  if (value === "") return t(`studio.library.metadataFilter.none.${field}`);
  switch (field) {
    case "captureDate": {
      const [year, month] = value.split("-").map(Number);
      return new Date(Date.UTC(year, month - 1, 1)).toLocaleDateString(locale, {
        year: "numeric",
        month: "long",
        timeZone: "UTC",
      });
    }
    case "fileType":
      return t(`studio.library.metadataFilter.fileTypes.${value}` as ParseKeys);
    case "iso":
      return t("studio.library.metadataFilter.isoValue", { value });
    case "focalLength":
      return t("studio.library.metadataFilter.focalValue", { value });
    case "aperture":
      return t("studio.library.metadataFilter.apertureValue", { value });
    default:
      return value;
  }
}

/**
 * The Library's metadata lists (Lightroom's metadata columns, Capture One's
 * Filters tool): capture month, camera, lens, keyword, file type, ISO, focal
 * length and aperture, each value with how many photographs of the shown
 * folder or album have it within the search box and the culling filters.
 * Choosing values narrows the grid: any chosen value of a list, every list.
 * A list's counts leave out its own choice, so choosing another value of
 * it shows what that adds. Counted by the library's catalog.
 */
export default function StudioMetadataFilter() {
  const { t, i18n } = useTranslation();
  const { directory, folder, albumId, text, filter, setFilter } = useLibraryStore(
    useShallow((s) => ({
      directory: s.sessionDirectory,
      folder: s.currentFolderPath,
      albumId: s.activeAlbumId,
      text: s.searchCriteria.text,
      filter: s.filterCriteria,
      setFilter: s.setFilterCriteria,
    })),
  );
  const viewMode = useSettingsStore((s) => s.appSettings?.libraryViewMode);
  // A refreshed list (rename, move, background tagging) is counted again.
  const imageList = useLibraryStore((s) => s.imageList);
  const selection = filter.metadata;
  const [facets, setFacets] = useState<Facets>({});
  const base = useMemo(() => {
    const scope = scopeRule(folder, albumId, viewMode);
    return [...(scope ? [scope] : []), ...textRules(text), ...cullingRules(filter)];
    // The culling filters by value, not by object identity.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [folder, albumId, viewMode, text, filter.rating, filter.colors, filter.flag]);

  useEffect(() => {
    if (!directory) return;
    let cancelled = false;
    let timer: ReturnType<typeof setTimeout> | undefined;
    // Only the newest count publishes (an older one may answer late).
    let generation = 0;
    const count = () => {
      clearTimeout(timer);
      timer = setTimeout(() => {
        const mine = ++generation;
        libraryFacets(directory, base, selection)
          .then((result) => {
            if (!cancelled && mine === generation) setFacets(result ?? {});
          })
          .catch((e) => console.error("Metadata lists failed:", e));
      }, COUNT_DELAY_MS);
    };
    count();
    const stop = onCatalogChange(count);
    return () => {
      cancelled = true;
      clearTimeout(timer);
      stop();
    };
  }, [directory, base, selection, imageList]);

  if (!directory) return null;
  const toggle = (field: FacetField, value: string) => {
    const chosen = selection?.[field] ?? [];
    const next = chosen.includes(value) ? chosen.filter((v) => v !== value) : [...chosen, value];
    setFilter({ metadata: { ...selection, [field]: next } });
  };
  const clear = (field: FacetField) => setFilter({ metadata: { ...selection, [field]: [] } });
  return (
    <div className="studio-metadata-filter" role="group" aria-label={t("studio.library.metadataFilter.label")}>
      {FACET_FIELDS.map((field) => {
        const values: FacetValue[] = facets[field] ?? [];
        const chosen = selection?.[field] ?? [];
        const total = values.reduce((n, v) => n + v.count, 0);
        // A chosen value no photograph has any more stays listed (at 0),
        // so it can be taken back.
        const listed = [
          ...values,
          ...chosen.filter((c) => !values.some((v) => v.value === c)).map((value) => ({ value, count: 0 })),
        ];
        const heading = t(`studio.library.metadataFilter.fields.${field}`);
        return (
          <div key={field} className="studio-metadata-column" role="group" aria-label={heading}>
            <div className="studio-metadata-column-title">{heading}</div>
            <div className="studio-metadata-values">
              <button aria-pressed={chosen.length === 0} onClick={() => clear(field)}>
                <span>{t("studio.library.metadataFilter.all", { count: listed.length })}</span>
                {field !== "keyword" && <small>{total.toLocaleString()}</small>}
              </button>
              {listed.map(({ value, count }) => (
                <button
                  key={value}
                  aria-pressed={chosen.includes(value)}
                  onClick={() => toggle(field, value)}
                  className={value === "" ? "is-none" : undefined}
                >
                  <span>{facetLabel(field, value, t, i18n.language)}</span>
                  <small>{count.toLocaleString()}</small>
                </button>
              ))}
            </div>
          </div>
        );
      })}
    </div>
  );
}
