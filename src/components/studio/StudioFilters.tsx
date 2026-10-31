import type { ReactNode } from "react";
import { useTranslation } from "react-i18next";
import { Flag, FlagOff, ListFilter, X, Images, ListPlus } from "lucide-react";
import { useShallow } from "zustand/react/shallow";
import { useLibraryStore } from "../../store/useLibraryStore";
import { useSettingsStore } from "../../store/useSettingsStore";
import { RawStatus, EditedStatus } from "../ui/AppProperties";
import type { FlagCounts, FlagFilter } from "../../utils/culling";
import { filterAsRule, scopeRule, selectionCount } from "../../utils/librarySearch";
import { useSmartAlbumEditor } from "./SmartAlbumEditor";
import { checkEyes } from "../../hooks/useEyeCheck";

const flags = [
  { value: "all", label: "studio.library.flags.all", Icon: Images },
  { value: "picked", label: "studio.library.flags.picked", Icon: Flag },
  { value: "unflagged", label: "studio.library.flags.unflagged", Icon: FlagOff },
  { value: "rejected", label: "studio.library.flags.rejected", Icon: X },
] as const;

const colors = ["red", "yellow", "green", "blue", "purple"] as const;

/** Lightroom's filter bar in one row: flags with counts, colour labels, the
 * caller's extra filters (rating), the metadata lists' toggle, and while
 * anything filters, Clear and (in a library) Save as Smart Album. */
export default function StudioFilters({
  counts,
  children,
  metadataOpen,
  onToggleMetadata,
}: {
  counts: FlagCounts;
  children?: ReactNode;
  /** Whether the metadata lists show (`StudioMetadataFilter`). */
  metadataOpen?: boolean;
  onToggleMetadata?: () => void;
}) {
  const { t } = useTranslation();
  const { filter, search, sessionDirectory, folder, albumId, setFilter, setSearch } =
    useLibraryStore(
      useShallow((s) => ({
        filter: s.filterCriteria,
        search: s.searchCriteria,
        sessionDirectory: s.sessionDirectory,
        folder: s.currentFolderPath,
        albumId: s.activeAlbumId,
        setFilter: s.setFilterCriteria,
        setSearch: s.setSearchCriteria,
      })),
    );
  const viewMode = useSettingsStore((s) => s.appSettings?.libraryViewMode);
  const eyeCheck = useSettingsStore((s) => !!s.appSettings?.eyeCheck);
  const openSmartAlbum = useSmartAlbumEditor((s) => s.open);
  const chosen = selectionCount(filter.metadata);
  const asSmartAlbum = filterAsRule(
    scopeRule(folder, albumId, viewMode),
    search.text,
    filter.metadata,
    filter,
    search.tags,
  );
  const active =
    (filter.flag ?? "all") !== "all" ||
    (filter.eyes ?? "all") !== "all" ||
    filter.rating !== 0 ||
    filter.colors.length > 0 ||
    filter.rawStatus !== RawStatus.All ||
    (filter.editedStatus ?? EditedStatus.All) !== EditedStatus.All ||
    search.text.trim() !== "" ||
    search.tags.length > 0 ||
    chosen > 0;
  return (
    <div className="studio-library-filters" aria-label={t("studio.library.filters.label")}>
      {sessionDirectory && (
        <div
          className="studio-flag-filters"
          role="group"
          aria-label={t("studio.library.filters.byFlag")}
        >
          {flags.map(({ value, label: labelKey, Icon }) => {
            const label = t(labelKey);
            return (
            <button
              key={value}
              aria-label={`${label} ${counts[value].toLocaleString()}`}
              aria-pressed={(filter.flag ?? "all") === value}
              onClick={() => setFilter({ flag: value as FlagFilter })}
              title={t("studio.library.filters.matching", {
                label,
                count: counts[value],
                n: counts[value].toLocaleString(),
              })}
            >
              {Icon && <Icon size={12} />}
              <span className="studio-flag-label">{label}</span>{" "}
              <span className="studio-filter-count">
                {counts[value].toLocaleString()}
              </span>
            </button>
            );
          })}
        </div>
      )}
      <select
        aria-label={t("studio.library.filters.byColor")}
        value={
          filter.colors.length > 1 ? "multiple" : (filter.colors[0] ?? "all")
        }
        onChange={(e) =>
          setFilter({
            colors: e.target.value === "all" ? [] : [e.target.value],
          })
        }
      >
        <option value="all">{t("studio.library.filters.allLabels")}</option>
        <option value="none">{t("studio.library.filters.noLabel")}</option>
        {colors.map((color) => (
          <option key={color} value={color}>
            {t(`studio.library.colors.${color}`)}
          </option>
        ))}
        {filter.colors.length > 1 && (
          <option value="multiple" disabled>
            {t("studio.library.filters.multipleLabels")}
          </option>
        )}
      </select>
      {children}
      {sessionDirectory && (
        // Faces' eyes (a suggestion, never a reject): closed or partly, or
        // in need of a check. Until turned on, the one entry starts it.
        <select
          className="studio-eyes-filter"
          aria-label={t("studio.eyes.filter")}
          title={t("studio.eyes.suggestion")}
          value={eyeCheck ? (filter.eyes ?? "all") : "all"}
          onChange={(e) => {
            if (e.target.value === "start") void checkEyes(sessionDirectory);
            else setFilter({ eyes: e.target.value as "all" | "closed" | "check" });
          }}
        >
          <option value="all">{t("studio.eyes.filterAll")}</option>
          {eyeCheck ? (
            <>
              <option value="closed">{t("studio.eyes.filterClosed")}</option>
              <option value="check">{t("studio.eyes.filterCheck")}</option>
            </>
          ) : (
            <option value="start">{t("studio.eyes.start")}</option>
          )}
        </select>
      )}
      {sessionDirectory && onToggleMetadata && (
        <button
          className="studio-metadata-toggle"
          aria-label={`${t("studio.library.metadataFilter.button")}${chosen > 0 ? ` ${chosen.toLocaleString()}` : ""}`}
          aria-pressed={!!metadataOpen}
          title={t("studio.library.metadataFilter.toggle")}
          onClick={onToggleMetadata}
        >
          <ListFilter size={12} />
          <span>{t("studio.library.metadataFilter.button")}</span>
          {chosen > 0 && <span className="studio-filter-count">{chosen}</span>}
        </button>
      )}
      {sessionDirectory && (
        <button
          className="studio-clear-filters"
          aria-label={t("studio.library.smartAlbums.fromFilter")}
          hidden={!active}
          disabled={!asSmartAlbum}
          title={
            asSmartAlbum
              ? t("studio.library.smartAlbums.fromFilterTitle")
              : t("studio.library.smartAlbums.fromFilterUnsupported")
          }
          onClick={() =>
            asSmartAlbum &&
            openSmartAlbum({
              name: t("studio.library.smartAlbums.defaultName"),
              parent: null,
              rules: asSmartAlbum,
            })
          }
        >
          <ListPlus size={12} />
          <span>{t("studio.library.smartAlbums.fromFilter")}</span>
        </button>
      )}
      <button
        className="studio-clear-filters"
        aria-label={t("studio.library.filters.clearFilters")}
        title={t("studio.library.filters.clearFilters")}
        hidden={!active}
        onClick={() => {
          setFilter({
            flag: "all",
            eyes: "all",
            rating: 0,
            colors: [],
            rawStatus: RawStatus.All,
            editedStatus: EditedStatus.All,
            metadata: {},
          });
          setSearch({ text: "", tags: [] });
        }}
      >
        <X size={12} />
        <span>{t("studio.library.filters.clear")}</span>
      </button>
    </div>
  );
}
