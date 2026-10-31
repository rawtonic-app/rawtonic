import { useEffect, useMemo, useState } from "react";
import { useTranslation } from "react-i18next";
import type { ParseKeys, TFunction } from "i18next";
import { useShallow } from "zustand/react/shallow";
import { useLibraryStore } from "../../store/useLibraryStore";
import { useSettingsStore } from "../../store/useSettingsStore";
import { onCatalogChange } from "../../utils/catalogWrites";
import { folderViewMode } from "../../utils/subfolderView";
import {
  FACET_FIELDS,
  SOURCE_TYPES,
  cullingRules,
  libraryFacets,
  rawRules,
  scopeRule,
  textRules,
  type FacetField,
  type FacetValue,
  type Facets,
} from "../../utils/librarySearch";

/** How long changes pause before the value lists are counted again. */
const COUNT_DELAY_MS = 120;

/** A capture date list value naming a whole year (`2024`), not a month
 * (asked of capture dates only: ISO 3200 is no year). */
export const isYear = (value: string) => /^\d{4}$/.test(value);

/** One row of a list: a value, or (capture date) a year over its months. */
interface Row {
  value: string;
  count: number;
  /** A year's row: its months as listed. */
  months?: FacetValue[];
}

/**
 * The values from `anchor` to `clicked` (both included) in the order the
 * list shows them — numbers and dates are listed in order, so this is a
 * range of values (ISO 800 to 3200). Rows of the other kind (years among
 * months) and "no value" between them are left out.
 */
export function rangeOf(rows: Row[], anchor: string, clicked: string): string[] | null {
  const from = rows.findIndex((row) => row.value === anchor);
  const to = rows.findIndex((row) => row.value === clicked);
  const kind = (value: string) => rows.find((row) => row.value === value)?.months !== undefined;
  if (from < 0 || to < 0) return null;
  // "No value" is of either kind: the range takes the other end's.
  const years = clicked === "" ? kind(anchor) : kind(clicked);
  if (anchor !== "" && clicked !== "" && kind(anchor) !== years) return null;
  const [low, high] = from < to ? [from, to] : [to, from];
  return rows
    .slice(low, high + 1)
    .filter((row) => row.value === anchor || row.value === clicked || (row.value !== "" && (row.months !== undefined) === years))
    .map((row) => row.value);
}

/** The capture date list as years (newest first, as the months come), each
 * over its months; "no date" stays last. */
function yearRows(values: FacetValue[]): Row[] {
  const rows: Row[] = [];
  for (const month of values) {
    const year = month.value.slice(0, 4);
    const last = rows.at(-1);
    // A chosen year no photograph has any more (kept at 0): a year of no months.
    if (isYear(month.value)) rows.push({ value: month.value, count: month.count, months: [] });
    else if (month.value !== "" && last?.months && last.value === year) {
      last.count += month.count;
      last.months.push(month);
    } else if (month.value !== "") rows.push({ value: year, count: month.count, months: [month] });
    else rows.push(month);
  }
  return rows;
}

/** Lists whose values come in order (the catalog's: numbers ascending,
 * months newest first), so a Shift-click range is a range of values. */
const ORDERED: Partial<Record<FacetField, (a: string, b: string) => number>> = {
  captureDate: (a, b) => b.localeCompare(a),
  iso: (a, b) => Number(a) - Number(b),
  focalLength: (a, b) => Number(a) - Number(b),
  aperture: (a, b) => Number(a) - Number(b),
};

/** A list's values with the chosen ones no photograph has any more (at 0,
 * so they can be taken back): in their place in an ordered list, else last;
 * "no value" stays last. */
function withKept(field: FacetField, values: FacetValue[], kept: string[]): FacetValue[] {
  const listed = [...values, ...kept.map((value) => ({ value, count: 0 }))];
  const order = ORDERED[field];
  if (!order) return listed;
  return [
    ...listed.filter((v) => v.value !== "").sort((a, b) => order(a.value, b.value)),
    ...listed.filter((v) => v.value === ""),
  ];
}

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
      if (isYear(value)) return value;
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
    case "ai":
      return t(`studio.library.metadataFilter.aiFacts.${value}` as ParseKeys, { defaultValue: value });
    case "sourceType":
      return (SOURCE_TYPES as readonly string[]).includes(value)
        ? t(`studio.library.metadataFilter.sourceTypes.${value}` as ParseKeys)
        : value;
    default:
      return value;
  }
}

/**
 * The Library's metadata lists (Lightroom's metadata columns, Capture One's
 * Filters tool): capture month, camera, lens, keyword, file type, ISO, focal
 * length, aperture, AI edits (generated areas and the model that filled
 * them, AI selections, AI Denoise, AI results still missing) and the
 * digital source type a file states, each value with how many photographs of the shown
 * folder or album have it within the search box and the culling filters.
 * Choosing values narrows the grid: any chosen value of a list, every list;
 * Shift-click chooses a run of values (ISO 800 to 3200), and capture dates
 * come by year, a year's months listed below it on demand.
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
  const subfolderDefault = useLibraryStore((s) => s.subfolderDefaultRoot);
  const viewMode = useSettingsStore((s) => (folder ? folderViewMode(folder, s.appSettings, subfolderDefault) : undefined));
  // A refreshed list (rename, move, background tagging) is counted again.
  const imageList = useLibraryStore((s) => s.imageList);
  const selection = filter.metadata;
  const [facets, setFacets] = useState<Facets>({});
  // The value last clicked in each list (where a Shift-click range starts),
  // and the capture years shown with their months.
  const [anchor, setAnchor] = useState<Partial<Record<FacetField, string>>>({});
  const [open, setOpen] = useState<Record<string, boolean>>({});
  const base = useMemo(() => {
    const scope = scopeRule(folder, albumId, viewMode);
    return [...(scope ? [scope] : []), ...textRules(text), ...cullingRules(filter), ...rawRules(filter)];
    // The culling filters by value, not by object identity.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [folder, albumId, viewMode, text, filter.rating, filter.colors, filter.flag, filter.rawStatus]);

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
        libraryFacets(directory, base, selection, albumId ? null : folder)
          .then((result) => {
            if (!cancelled && mine === generation) setFacets(result ?? {});
          })
          .catch((e) => {
            // No lists rather than another filter's counts.
            if (!cancelled && mine === generation) setFacets({});
            console.error("Metadata lists failed:", e);
          });
      }, COUNT_DELAY_MS);
    };
    count();
    const stop = onCatalogChange(count);
    return () => {
      cancelled = true;
      clearTimeout(timer);
      stop();
    };
  }, [directory, base, selection, imageList, albumId, folder]);

  if (!directory) return null;
  /** A click chooses a value or takes it back; with Shift, every value from
   * the last one clicked to this one is chosen (Lightroom's columns). */
  const choose = (field: FacetField, value: string, rows: Row[], shift: boolean) => {
    const chosen = selection?.[field] ?? [];
    const last = anchor[field];
    const range = shift && last !== undefined ? rangeOf(rows, last, value) : null;
    let next: string[];
    if (range) next = [...chosen, ...range.filter((v) => !chosen.includes(v))];
    else if (chosen.includes(value)) next = chosen.filter((v) => v !== value);
    else {
      // A month of a chosen year: the year gives way to its other months.
      const year = field === "captureDate" && !isYear(value) ? value.slice(0, 4) : null;
      const yearRow = year && chosen.includes(year) ? rows.find((row) => row.value === year) : undefined;
      next = yearRow
        ? [...chosen.filter((v) => v !== year), ...(yearRow.months ?? []).map((m) => m.value).filter((m) => m !== value)]
        : [...chosen, value];
    }
    // A chosen year holds its months: they are not chosen besides it.
    if (field === "captureDate") {
      const years = new Set(next.filter(isYear));
      next = next.filter((v) => isYear(v) || !years.has(v.slice(0, 4)));
    }
    setAnchor((a) => ({ ...a, [field]: value }));
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
        const gone = chosen
          .filter((c) => !values.some((v) => v.value === c))
          .filter((c) => !(field === "captureDate" && isYear(c) && values.some((v) => v.value.startsWith(`${c}-`))));
        const kept = withKept(field, values, gone);
        const listed: Row[] = field === "captureDate" ? yearRows(kept) : kept;
        // Rows as shown: a year's months below it while it is open.
        const shown: Row[] = listed.flatMap((row) =>
          row.months && open[row.value] ? [row, ...row.months] : [row],
        );
        const heading = t(`studio.library.metadataFilter.fields.${field}`);
        const pressed = (value: string) =>
          chosen.includes(value) || (field === "captureDate" && !isYear(value) && chosen.includes(value.slice(0, 4)));
        return (
          <div key={field} className="studio-metadata-column" role="group" aria-label={heading}>
            <div className="studio-metadata-column-title" title={t("studio.library.metadataFilter.rangeHint")}>
              {heading}
            </div>
            <div className="studio-metadata-values">
              <button aria-pressed={chosen.length === 0} onClick={() => clear(field)}>
                <span>{t("studio.library.metadataFilter.all", { count: field === "captureDate" ? values.length : listed.length })}</span>
                {/* A photograph counts once per keyword or AI edit. */}
                {field !== "keyword" && field !== "ai" && <small>{total.toLocaleString()}</small>}
              </button>
              {shown.map(({ value, count, months }) => {
                const month = field === "captureDate" && value !== "" && !isYear(value);
                const label = facetLabel(field, value, t, i18n.language);
                return (
                  <div key={value} className={`studio-metadata-row${month ? " is-month" : ""}`}>
                    {months && months.length > 0 && (
                      <button
                        className="studio-metadata-disclosure"
                        aria-expanded={Boolean(open[value])}
                        aria-label={t(
                          open[value]
                            ? "studio.library.metadataFilter.hideMonths"
                            : "studio.library.metadataFilter.showMonths",
                          { year: value },
                        )}
                        onClick={() => setOpen((o) => ({ ...o, [value]: !o[value] }))}
                      >
                        {open[value] ? "▾" : "▸"}
                      </button>
                    )}
                    <button
                      aria-pressed={pressed(value)}
                      onClick={(event) => choose(field, value, shown, event.shiftKey)}
                      // Shift-click chooses a range; it selects no page text.
                      onMouseDown={(event) => event.shiftKey && event.preventDefault()}
                      className={value === "" ? "is-none" : undefined}
                    >
                      <span>{label}</span>
                      <small>{count.toLocaleString()}</small>
                    </button>
                  </div>
                );
              })}
            </div>
          </div>
        );
      })}
    </div>
  );
}
