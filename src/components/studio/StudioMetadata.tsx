import { useEffect, useMemo, useRef, useState, type KeyboardEvent } from "react";
import { create } from "zustand";
import { invoke } from "@tauri-apps/api/core";
import { open as openInBrowser } from "@tauri-apps/plugin-shell";
import { Check, X } from "lucide-react";
import { notify } from "../../activity/notify";
import i18n from "i18next";
import { useTranslation } from "react-i18next";
import { useLibraryStore } from "../../store/useLibraryStore";
import { useEditorStore } from "../../store/useEditorStore";
import { Invokes } from "../ui/AppProperties";
import {
  keywordOf,
  saveFailure,
  storedKeyword,
  useLibraryActions,
} from "../../hooks/useLibraryActions";
import { useSettingsStore } from "../../store/useSettingsStore";
import { expandGroupedPaths } from "../../utils/imageGrouping";
import { COLOR_LABELS } from "../../utils/adjustments";
import { ToolGroup } from "./StudioDevelop";
import { catalogChanged, invokeInOrder } from "../../utils/catalogWrites";
import { submitPhotoWrite } from "../../utils/photoWrites";
import { captureText } from "../../utils/captureTime";
import { useCaptureTimeDialog } from "./CaptureTimeDialog";

export interface TextMetadata {
  title: string;
  description: string;
  creator: string;
  copyright: string;
  /** IPTC fields (rawtonic_metadata::IPTC_FIELDS); explicit empty strings clear fields. */
  [iptc: string]: string | undefined;
}
type TextField = string;

export const IPTC_LIST_FIELDS = new Set<string>(["personInImage", "organisationInImageName", "organisationInImageCode", "modelAge", "subjectCode", "scene"]);
/** IPTC Core and Extension sections of the Metadata tool, as Capture One and Lightroom
 * group them: section ids and field ids (labels in `studio.metadata.iptc`). */
export const IPTC_SECTIONS = [
  ["content", ["headline", "subjectCode", "scene", "intellectualGenre", "captionWriter", "dateCreated"]],
  ["location", ["location", "city", "state", "country", "countryCode"]],
  ["status", ["jobId", "instructions", "credit", "source", "usageTerms", "rightsUrl"]],
  [
    "creatorContact",
    [
      "creatorJobTitle",
      "creatorEmail",
      "creatorUrl",
      "creatorPhone",
      "creatorAddress",
      "creatorCity",
      "creatorRegion",
      "creatorPostalCode",
      "creatorCountry",
    ],
  ],
  ["extension", ["personInImage", "organisationInImageName", "organisationInImageCode", "event", "modelAge", "additionalModelInfo", "maxAvailHeight", "maxAvailWidth", "digitalSourceType"]],
] as const;
const TEXT_FIELDS = ["title", "description", "creator", "copyright"] as const;

/** Display names of the colour labels (ids stay the stored tag names). */
const COLOR_NAME_KEYS = {
  red: "contextMenus.colors.red",
  yellow: "contextMenus.colors.yellow",
  green: "contextMenus.colors.green",
  blue: "contextMenus.colors.blue",
  purple: "contextMenus.colors.purple",
} as const;

/** Outside sessions the core fields are kept as EXIF overrides with each
 * photo's edits (update_exif_fields), which exports write: Description →
 * ImageDescription, Creator → Artist, Copyright → Copyright. A title has no
 * EXIF field and needs a session. */
const EXIF_TEXT_FIELDS: Record<string, string> = {
  description: "ImageDescription",
  creator: "Artist",
  copyright: "Copyright",
};

/** An EXIF text value as shown: without the quotes some readers keep, and
 * without the placeholder "default" some cameras write. */
export function exifText(value: string | undefined) {
  const clean = (value ?? "").replace(/^"|"$/g, "").trim();
  return clean.toLowerCase() === "default" ? "" : clean;
}

/** The colour label of a photo's tags (`color:red`), or null. */
export const colorLabelOf = (tags: string[] | null | undefined) =>
  tags?.find((tag) => tag.startsWith("color:"))?.slice(6) ?? null;

/** "24.0 MB" from bytes (decimal units, as Finder shows them). */
export function formatBytes(bytes: number) {
  if (bytes < 1000) return `${bytes} B`;
  const units = ["kB", "MB", "GB", "TB"];
  let value = bytes / 1000;
  let unit = 0;
  while (value >= 1000 && unit < units.length - 1) {
    value /= 1000;
    unit++;
  }
  return `${value.toFixed(value < 100 ? 1 : 0)} ${units[unit]}`;
}

/** Title/description/creator/copyright of the whole session, read in one
 * query when a session opens, so the Metadata tool shows them at once.
 * `null`: a registered photo without any of the four; absent: not registered. */
interface TextMetadataCache {
  session: string | null;
  loaded: boolean;
  byPath: Record<string, TextMetadata | null>;
  load(session: string): Promise<void>;
  merge(written: Record<string, TextMetadata>): void;
}
const basePath = (path: string) => path.split("?vc=")[0];
export const useTextMetadataCache = create<TextMetadataCache>((set, get) => ({
  session: null,
  loaded: false,
  byPath: {},
  async load(session) {
    if (get().session !== session) set({ session, loaded: false, byPath: {} });
    try {
      const byPath = await invoke<Record<string, TextMetadata | null>>(Invokes.SessionAllTextMetadata, {
        directory: session,
      });
      if (get().session === session) set({ loaded: true, byPath: byPath ?? {} });
    } catch (e) {
      notify.error(i18n.t("studio.metadata.toasts.readFailed", { error: String(e) }));
    }
  },
  merge(written) {
    set((state) => ({
      byPath: {
        ...state.byPath,
        ...Object.fromEntries(Object.entries(written).map(([path, meta]) => [basePath(path), meta])),
      },
    }));
  },
}));

/** EXIF of photos read from their headers (Capture One shows it at once;
 * the shown photo's EXIF otherwise only arrives with its decoded RAW). */
const exifRequests = new Set<string>();
export function requestHeaderExif(paths: string[]) {
  const needed = paths.filter((path) => !exifRequests.has(path));
  if (!needed.length) return;
  needed.forEach((path) => exifRequests.add(path));
  Promise.resolve()
    .then(() => invoke<Record<string, Record<string, string>>>(Invokes.ReadExifForPaths, { paths: needed }))
    .then((data) => {
      const requested = new Set(needed);
      useLibraryStore.getState().setLibrary((state) => ({
        imageList: state.imageList.map((image) =>
          requested.has(image.path) && image.exif == null ? { ...image, exif: data?.[image.path] ?? {} } : image,
        ),
      }));
    })
    .catch(() => {})
    .finally(() => needed.forEach((path) => exifRequests.delete(path)));
}

/** The photo whose details the tools show, and its EXIF (loaded from the file
 * header with its neighbours when missing, so browsing stays instant). */
export function useShownPhoto() {
  const paths = useMetadataTargets();
  const imageList = useLibraryStore((s) => s.imageList);
  const active = useLibraryStore((s) => s.libraryActivePath);
  const selectedImage = useEditorStore((s) => s.selectedImage);
  const path = selectedImage?.path ?? active ?? paths[0];
  const index = imageList.findIndex((image) => image.path === path);
  const listed = index >= 0 ? imageList[index] : undefined;
  const shown = selectedImage && selectedImage.path === path ? selectedImage : null;
  const exif: Record<string, string> | null = shown?.exif ?? listed?.exif ?? null;
  useEffect(() => {
    if (!path || exif || index < 0) return;
    const list = useLibraryStore.getState().imageList;
    requestHeaderExif(
      list
        .slice(Math.max(0, index - 2), index + 3)
        .filter((image) => image.exif == null)
        .map((image) => image.path),
    );
  }, [path, exif, index]);
  return { path, shown, exif: exif ?? {} };
}

/** The photographs metadata tools act on: the selection, else the photo shown. */
export function useMetadataTargets() {
  const selected = useLibraryStore((s) => s.multiSelectedPaths);
  const image = useEditorStore((s) => s.selectedImage?.path);
  return useMemo(
    () => (selected.length ? selected : image ? [image] : []),
    [selected, image],
  );
}

/** Keywords of the targets with how many targets carry each, in name order.
 * A keyword is one exact stored form, as the library keeps tags (`change_tags`
 * in src-tauri/src/tagging.rs sort and dedup them as strings): "Dog" and
 * "dog", "user:Dog" and "Dog", are different keywords, each written and
 * removed as its own string, so the chips show every keyword the selection
 * has instead of hiding a case variant behind one of them. `tags` holds the
 * stored forms of the keyword, which a removal takes off. */
export function keywordCounts(tagsByPath: (string[] | null | undefined)[]) {
  const counts = new Map<string, { keyword: string; tags: Set<string>; count: number }>();
  for (const tags of tagsByPath) {
    const seen = new Set<string>();
    for (const tag of tags ?? []) {
      const keyword = keywordOf(tag);
      if (!keyword) continue;
      const entry = counts.get(keyword) ?? { keyword, tags: new Set<string>(), count: 0 };
      entry.tags.add(tag);
      if (!seen.has(keyword)) entry.count++;
      seen.add(keyword);
      counts.set(keyword, entry);
    }
  }
  return [...counts.values()].sort((a, b) => a.keyword.localeCompare(b.keyword));
}

/** Keywords typed into the field: comma-separated, trimmed, without duplicates. */
export function parseKeywords(text: string) {
  const keywords: string[] = [];
  for (const part of text.split(",")) {
    const keyword = part.trim();
    if (keyword && !keywords.some((k) => k.toLowerCase() === keyword.toLowerCase()))
      keywords.push(keyword);
  }
  return keywords;
}

/** Capture One's Keywords tool / Lightroom's Keywording: the keywords of the
 * selection, partial ones marked, typed keywords added to every photograph. */
export function StudioKeywords() {
  const { t } = useTranslation();
  const { applyTagDelta } = useLibraryActions();
  const imageList = useLibraryStore((s) => s.imageList);
  // The photographs to keyword: the selection and, with grouping on, each
  // chosen photograph's variants too (as the tagging menu writes to a group,
  // and ratings and labels spread over one).
  const selected = useMetadataTargets();
  const grouping = useSettingsStore((s) => s.appSettings?.grouping ?? "off");
  const paths = useMemo(() => expandGroupedPaths(imageList, selected, grouping), [imageList, selected, grouping]);
  const [draft, setDraft] = useState("");
  const tagsByPath = useMemo(() => {
    const byPath = new Map(imageList.map((image) => [image.path, image.tags]));
    return paths.map((path) => byPath.get(path));
  }, [imageList, paths]);
  const keywords = useMemo(() => keywordCounts(tagsByPath), [tagsByPath]);
  const suggestions = useMemo(
    () =>
      keywordCounts(imageList.map((image) => image.tags))
        .map((entry) => entry.keyword)
        .slice(0, 500),
    [imageList],
  );

  const add = async (text: string) => {
    const typed = parseKeywords(text);
    if (!paths.length || !typed.length) return;
    try {
      const tagsOf = new Map(useLibraryStore.getState().imageList.map((image) => [image.path, image.tags]));
      for (const typedKeyword of typed) {
        // The UI's existing rule, as the tagging menu applies it to a typed
        // keyword (trimmed, lowercase): that form is compared and written.
        const keyword = typedKeyword.trim().toLowerCase();
        if (!keyword) continue;
        // Only photographs without the keyword—as the exact string the
        // library adds—get it; each keeps its other keywords. A stored form
        // that differs only in case ("user:Dog") is another keyword, so
        // "user:dog" written beside it shows as its own chip instead of
        // hiding behind the one shown. The catalog stores it without the
        // marker.
        const tag = storedKeyword(keyword);
        const has = (path: string) => (tagsOf.get(path) ?? []).includes(tag);
        const missing = paths.filter((path) => !has(path));
        if (!missing.length) continue;
        // The whole selection is written (adding is idempotent), so a group's
        // variant that has the keyword already is named too. The library
        // then changes that one tag on each photograph that saved: the ones
        // the failure names (`SaveFailure.failed`) keep their old keywords.
        try {
          await invokeInOrder(Invokes.AddTagForPaths, { paths, tag }, "keyword");
          applyTagDelta(missing, tag, "add");
        } catch (e) {
          const { failed } = saveFailure(e, paths);
          applyTagDelta(paths.filter((path) => !failed.includes(path)), tag, "add");
          throw e;
        }
      }
      setDraft("");
    } catch (e) {
      notify.error(t("studio.metadata.toasts.keywordsSaveFailed", { error: saveFailure(e, paths).message }));
    }
  };
  const remove = async (entry: (typeof keywords)[number]) => {
    try {
      for (const tag of entry.tags) {
        // As for adding: the photographs the library names did not save keep
        // the keyword.
        try {
          await invokeInOrder(Invokes.RemoveTagForPaths, { paths, tag }, "keyword");
          applyTagDelta(paths, tag, "remove");
        } catch (e) {
          const { failed } = saveFailure(e, paths);
          applyTagDelta(paths.filter((path) => !failed.includes(path)), tag, "remove");
          throw e;
        }
      }
    } catch (e) {
      notify.error(t("studio.metadata.toasts.keywordsRemoveFailed", { error: saveFailure(e, paths).message }));
    }
  };
  const onKeyDown = (e: KeyboardEvent<HTMLInputElement>) => {
    e.stopPropagation();
    if (e.key === "Enter") {
      e.preventDefault();
      void add(draft);
    } else if (e.key === "Escape") {
      setDraft("");
      e.currentTarget.blur();
    }
  };

  return (
    <ToolGroup title="Keywords" label={t("studio.metadata.keywords.title")}>
      {!paths.length ? (
        <p className="studio-help">{t("studio.metadata.keywords.selectToAdd")}</p>
      ) : (
        <>
          <div className="studio-keywords" aria-label={t("studio.metadata.keywords.ofSelection")}>
            {keywords.length === 0 && <span className="studio-keywords-empty">{t("studio.metadata.keywords.none")}</span>}
            {keywords.map((entry) => {
              const partial = entry.count < paths.length;
              return (
                <span
                  key={entry.keyword}
                  className={`studio-keyword ${partial ? "is-partial" : ""}`}
                  title={
                    partial
                      ? t("studio.metadata.keywords.partial", { on: entry.count, count: paths.length })
                      : undefined
                  }
                >
                  {entry.keyword}
                  {partial && <span aria-hidden="true">*</span>}
                  <button
                    aria-label={t("studio.metadata.keywords.removeAria", { keyword: entry.keyword })}
                    title={t("studio.metadata.keywords.removeTitle", { keyword: entry.keyword })}
                    onClick={() => void remove(entry)}
                  >
                    <X size={10} />
                  </button>
                </span>
              );
            })}
          </div>
          <input
            className="studio-metadata-input"
            aria-label={t("studio.metadata.keywords.add")}
            placeholder={
              paths.length > 1
                ? t("studio.metadata.keywords.addTo", { count: paths.length })
                : t("studio.metadata.keywords.add")
            }
            list="studio-keyword-suggestions"
            value={draft}
            onChange={(e) => setDraft(e.target.value)}
            onKeyDown={onKeyDown}
          />
          <datalist id="studio-keyword-suggestions">
            {suggestions.map((keyword) => (
              <option key={keyword} value={keyword} />
            ))}
          </datalist>
        </>
      )}
    </ToolGroup>
  );
}

/** The selection's value of one field, or `null` when the photographs differ. */
export function sharedValue(records: (TextMetadata | undefined)[], field: TextField) {
  const values = new Set(records.map((record) => record?.[field] ?? ""));
  return values.size === 1 ? [...values][0] : null;
}

function MetadataField({
  label,
  value,
  multiline,
  disabled,
  onCommit,
}: {
  label: string;
  /** `null`: the selected photographs differ. */
  value: string | null;
  multiline?: boolean;
  disabled: boolean;
  onCommit(value: string): void;
}) {
  const { t } = useTranslation();
  const [draft, setDraft] = useState(value ?? "");
  const editing = useRef(false);
  useEffect(() => {
    if (!editing.current) setDraft(value ?? "");
  }, [value]);
  const commit = () => {
    editing.current = false;
    // An untouched mixed field keeps every photograph's own value.
    if (draft.trim() !== (value ?? "") && !(value === null && draft === "")) onCommit(draft);
  };
  const props = {
    "aria-label": label,
    className: "studio-metadata-input",
    value: draft,
    disabled,
    placeholder: value === null ? t("studio.metadata.mixed") : "",
    onFocus: () => {
      editing.current = true;
    },
    onChange: (e: { target: { value: string } }) => setDraft(e.target.value),
    onBlur: commit,
    onKeyDown: (e: KeyboardEvent<HTMLInputElement | HTMLTextAreaElement>) => {
      e.stopPropagation();
      if (e.key === "Escape") {
        editing.current = false;
        setDraft(value ?? "");
        // Blur after the reset so the blur does not commit the old draft.
        requestAnimationFrame(() => (e.target as HTMLElement).blur());
      } else if (e.key === "Enter" && !(multiline && e.shiftKey)) {
        e.preventDefault();
        (e.target as HTMLElement).blur();
      }
    },
  };
  return (
    <label className="studio-metadata-field">
      <span>{label}</span>
      {multiline ? <textarea rows={2} {...props} /> : <input {...props} />}
    </label>
  );
}

function exposureSummary(exif: Record<string, string>) {
  const parts = [
    exif.ExposureTime && (String(exif.ExposureTime).endsWith("s") ? exif.ExposureTime : `${exif.ExposureTime} s`),
    exif.FNumber && (String(exif.FNumber).toLowerCase().startsWith("f") ? exif.FNumber : `f/${exif.FNumber}`),
    exif.PhotographicSensitivity && `ISO ${exif.PhotographicSensitivity}`,
    (exif.FocalLength || exif.FocalLengthIn35mmFilm) && `${exif.FocalLength || exif.FocalLengthIn35mmFilm}`.replace(/(\d)$/, "$1 mm"),
  ];
  return parts.filter(Boolean).join(" · ");
}

/** The EXIF of each target (the shown photo's from the editor), loading the
 * missing ones from their file headers. `null` entries are still loading. */
function useTargetExif(paths: string[]) {
  const imageList = useLibraryStore((s) => s.imageList);
  const selectedImage = useEditorStore((s) => s.selectedImage);
  const records = useMemo(() => {
    const byPath = new Map(imageList.map((image) => [image.path, image.exif]));
    return paths.map((path) =>
      selectedImage?.path === path && selectedImage.exif ? selectedImage.exif : (byPath.get(path) ?? null),
    );
  }, [imageList, selectedImage, paths]);
  useEffect(() => {
    const listed = new Set(useLibraryStore.getState().imageList.map((image) => image.path));
    const missing = paths.filter((path, i) => !records[i] && listed.has(path));
    if (missing.length) requestHeaderExif(missing.slice(0, 500));
  }, [paths, records]);
  return records;
}

/** The selection's value of one EXIF text field, or `null` when they differ. */
export function sharedExifValue(records: (Record<string, string> | null)[], key: string) {
  const values = new Set(records.map((record) => exifText(record?.[key])));
  return values.size === 1 ? [...values][0] : null;
}

/** Lightroom's Label / Capture One's Color Tag for the selection. */
function ColorLabelPicker({ paths }: { paths: string[] }) {
  const { t } = useTranslation();
  const imageList = useLibraryStore((s) => s.imageList);
  const { handleSetColorLabel } = useLibraryActions();
  const current = useMemo(() => {
    const byPath = new Map(imageList.map((image) => [image.path, image.tags]));
    const labels = new Set(paths.map((path) => colorLabelOf(byPath.get(path))));
    return labels.size === 1 ? [...labels][0] : undefined;
  }, [imageList, paths]);
  return (
    <div className="studio-metadata-field studio-color-labels" role="group" aria-label={t("studio.metadata.colorLabel.group")}>
      <span>{t("studio.metadata.colorLabel.label")}</span>
      <span>
        <button
          type="button"
          className={`studio-color-label is-none ${current === null ? "is-current" : ""}`}
          title={t("studio.metadata.colorLabel.none")}
          aria-label={t("studio.metadata.colorLabel.none")}
          aria-pressed={current === null}
          onClick={() => void handleSetColorLabel(null, paths)}
        >
          <X size={9} />
        </button>
        {current && !COLOR_LABELS.some((label) => label.name === current) && (
          <span>{current === "orange" ? "Orange" : current === "pink" ? "Pink" : current}</span>
        )}
        {COLOR_LABELS.map((label) => {
          const key = COLOR_NAME_KEYS[label.name as keyof typeof COLOR_NAME_KEYS];
          const name = key ? t(key) : label.name[0].toUpperCase() + label.name.slice(1);
          return (
            <button
              type="button"
              key={label.name}
              className={`studio-color-label ${current === label.name ? "is-current" : ""}`}
              style={{ background: label.color }}
              title={name}
              aria-label={t("studio.metadata.colorLabel.colorAria", { color: name })}
              aria-pressed={current === label.name}
              onClick={() => void handleSetColorLabel(label.name, paths)}
            >
              {current === label.name && <Check size={9} />}
            </button>
          );
        })}
      </span>
    </div>
  );
}

/** The shown photograph's file size, read once per file. */
function useFileSize(path: string | undefined) {
  const base = path?.split("?vc=")[0];
  const [size, setSize] = useState<{ path: string; bytes: number } | null>(null);
  useEffect(() => {
    if (!base) return;
    let current = true;
    invoke<number>(Invokes.FileSize, { path: base })
      .then((bytes) => current && typeof bytes === "number" && setSize({ path: base, bytes }))
      .catch(() => {});
    return () => {
      current = false;
    };
  }, [base]);
  return size && size.path === base ? size.bytes : null;
}

/** Capture One's Metadata tool / Lightroom's Metadata panel: IPTC core fields
 * for the whole selection (SQLite first, then the XMP sidecar), its colour
 * label and comment, and the active photograph's file and capture details. */
export function StudioMetadataTool() {
  const { t } = useTranslation();
  const paths = useMetadataTargets();
  const session = useLibraryStore((s) => s.sessionDirectory);
  const imageList = useLibraryStore((s) => s.imageList);
  // The listed files, not just their number: a rename keeps the count.
  const listed = useMemo(() => imageList.map((image) => basePath(image.path)).join("\n"), [imageList]);
  const cache = useTextMetadataCache();
  const { handleUpdateExif } = useLibraryActions();
  const loaded = cache.loaded && cache.session === session;
  useEffect(() => {
    // Reloaded when photos are added (ingest), renamed or moved, or the session changes.
    if (session) void useTextMetadataCache.getState().load(session);
  }, [session, listed]);

  const registered = paths.filter((path) => cache.byPath[basePath(path)] !== undefined);
  const values = paths.map((path) => cache.byPath[basePath(path)] ?? undefined);
  const save = async (field: TextField, value: string) => {
    if (!session) return;
    try {
      const written = await submitPhotoWrite(
        paths,
        () =>
          invoke<Record<string, TextMetadata>>(Invokes.SessionSetTextMetadata, {
            directory: session,
            paths,
            update: { [field]: value },
          }),
        { kind: 'text' },
      );
      catalogChanged();
      cache.merge(written);
    } catch (e) {
      notify.error(t("studio.metadata.toasts.saveFailed", { error: String(e) }));
    }
  };
  const exifRecords = useTargetExif(paths);
  const exifLoaded = exifRecords.every((record) => record !== null);
  const saveExif = (key: string, value: string) => void handleUpdateExif(paths, { [key]: value.trim() });
  const sessionReady = !!session && loaded && registered.length === paths.length;

  const { path: detailsPath, shown, exif } = useShownPhoto();
  const bytes = useFileSize(detailsPath);
  const camera = [exif.Make, exif.Model]
    .filter(Boolean)
    .join(" ")
    .replace(/^(\S+) \1 /i, "$1 ");
  const dimensions =
    shown?.width && shown.height
      ? `${shown.width} × ${shown.height} · ${((shown.width * shown.height) / 1e6).toFixed(1)} MP`
      : "";
  const fileName = detailsPath?.split("?vc=")[0].split(/[\\/]/).pop();
  const extension = fileName?.includes(".") ? fileName.split(".").pop()?.toUpperCase() : undefined;
  const details: [string, string, string | undefined][] = [
    ["file", t("studio.metadata.details.file"), fileName],
    [
      "format",
      t("studio.metadata.details.format"),
      [extension, detailsPath?.includes("?vc=") && t("studio.metadata.details.virtualCopy")]
        .filter(Boolean)
        .join(" · "),
    ],
    ["fileSize", t("studio.metadata.details.fileSize"), bytes !== null ? formatBytes(bytes) : undefined],
    ["camera", t("studio.metadata.details.camera"), camera],
    ["lens", t("studio.metadata.details.lens"), exif.LensModel],
    ["exposure", t("studio.metadata.details.exposure"), exposureSummary(exif)],
    [
      "captured",
      t("studio.metadata.details.captured"),
      exif.DateTimeOriginal &&
        captureText({ local: exif.DateTimeOriginal, subsec: exif.SubSecTimeOriginal, offset: exif.OffsetTimeOriginal }),
    ],
    ["size", t("studio.metadata.details.size"), dimensions],
  ];

  return (
    <ToolGroup title="Metadata" label={t("studio.metadata.title")}>
      {!paths.length ? (
        <p className="studio-help">{t("studio.metadata.selectToSee")}</p>
      ) : (
        <div className="studio-metadata">
          {!session && (
            <p className="studio-help">{t("studio.metadata.outsideSessions")}</p>
          )}
          {session && loaded && registered.length < paths.length && (
            <p className="studio-help">{t("studio.metadata.notInSession")}</p>
          )}
          {TEXT_FIELDS.map((field) =>
            session || !EXIF_TEXT_FIELDS[field] ? (
              <MetadataField
                key={field}
                label={t(`studio.metadata.fields.${field}`)}
                value={loaded ? sharedValue(values, field) : ""}
                multiline={field === "description"}
                disabled={!sessionReady}
                onCommit={(value) => void save(field, value)}
              />
            ) : (
              <MetadataField
                key={field}
                label={t(`studio.metadata.fields.${field}`)}
                value={exifLoaded ? sharedExifValue(exifRecords, EXIF_TEXT_FIELDS[field]) : ""}
                multiline={field === "description"}
                disabled={!exifLoaded}
                onCommit={(value) => saveExif(EXIF_TEXT_FIELDS[field], value)}
              />
            ),
          )}
          <MetadataField
            label={t("studio.metadata.fields.comment")}
            value={exifLoaded ? sharedExifValue(exifRecords, "UserComment") : ""}
            multiline
            disabled={!exifLoaded}
            onCommit={(value) => saveExif("UserComment", value)}
          />
          <ColorLabelPicker paths={paths} />
          <details className="studio-metadata-iptc">
            <summary>IPTC</summary>
            {IPTC_SECTIONS.map(([section, fields]) => (
              <fieldset key={section}>
                <legend>{t(`studio.metadata.iptc.sections.${section}`)}</legend>
                {fields.map((field) => (
                  <MetadataField
                    key={field}
                    label={t(`studio.metadata.iptc.fields.${field}`)}
                    value={loaded ? sharedValue(values, field) : ""}
                    multiline={IPTC_LIST_FIELDS.has(field) || field === "instructions" || field === "usageTerms"}
                    disabled={!sessionReady}
                    onCommit={(value) => void save(field, value)}
                  />
                ))}
              </fieldset>
            ))}
          </details>
          {paths.length > 1 && (
            <p className="studio-metadata-note">{t("studio.metadata.appliesTo", { count: paths.length })}</p>
          )}
          <dl className="studio-metadata-details">
            {details
              .filter(([, , value]) => value)
              .map(([id, label, value]) => (
                <div key={id}>
                  <dt>{label}</dt>
                  <dd title={value}>{value}</dd>
                </div>
              ))}
          </dl>
          {session && (
            <button
              type="button"
              className="studio-metadata-action"
              disabled={!sessionReady}
              onClick={() => useCaptureTimeDialog.getState().open(paths)}
            >
              {t("studio.metadata.shiftCaptureTime")}
            </button>
          )}
        </div>
      )}
    </ToolGroup>
  );
}

/** Decimal degrees from EXIF "47 deg 33 min 15.5 sec" and its N/S/E/W reference. */
export function gpsDegrees(value?: string, ref?: string) {
  const parts = value?.match(/(\d+\.?\d*)\s+deg\s+(\d+\.?\d*)\s+min\s+(\d+\.?\d*)\s+sec/);
  if (!parts || !ref) return null;
  const degrees = parseFloat(parts[1]) + parseFloat(parts[2]) / 60 + parseFloat(parts[3]) / 3600;
  return /^[SW]/i.test(ref) ? -degrees : degrees;
}

/** "ExposureBiasValue" → "Exposure Bias Value". */
export const exifLabel = (key: string) =>
  key.replace(/([a-z])([A-Z])/g, "$1 $2").replace(/([A-Z])([A-Z][a-z])/g, "$1 $2");

/** An OpenStreetMap view around a position. It loads from openstreetmap.org,
 * which then learns the position: only shown on request (PRIVACY.md). */
export function osmEmbedUrl(lat: number, lon: number) {
  const box = [lon - 0.01, lat - 0.01, lon + 0.01, lat + 0.01].join("%2C");
  return `https://www.openstreetmap.org/export/embed.html?bbox=${box}&layer=mapnik&marker=${lat}%2C${lon}`;
}

/** Capture One's EXIF section of the Metadata tool: every EXIF field of the
 * shown photograph, filterable, and its GPS position in decimal degrees, on
 * a map when asked for. */
export function StudioExif() {
  const { t } = useTranslation();
  const [filter, setFilter] = useState("");
  const { path, exif } = useShownPhoto();
  // The map asked for, per photograph (it never loads by itself).
  const [mapFor, setMapFor] = useState<string | null>(null);
  const lat = gpsDegrees(exif.GPSLatitude, exif.GPSLatitudeRef);
  const lon = gpsDegrees(exif.GPSLongitude, exif.GPSLongitudeRef);
  const entries = Object.entries(exif)
    .filter(([key, value]) => {
      const text = filter.trim().toLowerCase();
      return !text || exifLabel(key).toLowerCase().includes(text) || String(value).toLowerCase().includes(text);
    })
    .sort(([a], [b]) => a.localeCompare(b));
  return (
    <ToolGroup title="EXIF" open={false}>
      {!path || !Object.keys(exif).length ? (
        <p className="studio-help">{path ? t("studio.metadata.exif.noData") : t("studio.metadata.exif.selectPhoto")}</p>
      ) : (
        <>
          {lat !== null && lon !== null && (
            <div className="studio-exif-gps">
              <span>
                {lat.toFixed(6)}, {lon.toFixed(6)}
                {exif.GPSAltitude ? ` · ${exif.GPSAltitude}` : ""}
              </span>
              <button
                title={t("studio.metadata.exif.copyPosition")}
                onClick={() => void navigator.clipboard?.writeText(`${lat.toFixed(6)}, ${lon.toFixed(6)}`)}
              >
                {t("studio.metadata.exif.copy")}
              </button>
              {mapFor !== path && (
                <button title={t("studio.metadata.exif.mapHint")} onClick={() => setMapFor(path ?? null)}>
                  {t("studio.metadata.exif.showOnMap")}
                </button>
              )}
            </div>
          )}
          {lat !== null && lon !== null && mapFor === path && (
            <div className="studio-exif-map">
              <iframe
                title={t("studio.metadata.exif.mapFrame")}
                src={osmEmbedUrl(lat, lon)}
                loading="lazy"
              />
              <button
                onClick={() =>
                  void openInBrowser(`https://www.openstreetmap.org/?mlat=${lat}&mlon=${lon}#map=15/${lat}/${lon}`)
                }
              >
                {t("studio.metadata.exif.openInBrowser")}
              </button>
            </div>
          )}
          <input
            className="studio-metadata-input studio-preset-search"
            aria-label={t("studio.metadata.exif.findAria")}
            placeholder={t("studio.metadata.exif.findPlaceholder")}
            value={filter}
            onChange={(e) => setFilter(e.target.value)}
            onKeyDown={(e) => e.stopPropagation()}
          />
          <dl className="studio-metadata-details studio-exif">
            {entries.map(([key, value]) => (
              <div key={key}>
                <dt title={key}>{exifLabel(key)}</dt>
                <dd title={String(value)}>{String(value)}</dd>
              </div>
            ))}
          </dl>
        </>
      )}
    </ToolGroup>
  );
}
