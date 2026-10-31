import { runExportJobs, submittedFrom } from "../../utils/runExportJobs";
import { validNamingTemplate, validSequenceStart, type NamingPreview } from "../../utils/namingTemplates";
import { readJobName, writeJobName } from "../../utils/exportJob";
import NamingEditor from "../ui/NamingEditor";
import { useEffect, useState } from "react";
import { useTranslation } from "react-i18next";
import { invoke } from "@tauri-apps/api/core";
import { open } from "@tauri-apps/plugin-dialog";
import { useLibraryStore } from "../../store/useLibraryStore";
import { currentEditForExport, useEditorStore } from "../../store/useEditorStore";
import { useProcessStore } from "../../store/useProcessStore";
import { renderErrorMessage } from "../../utils/renderErrors";
import OperationError from "../ui/OperationError";
import { formatFileSize } from "../../utils/formatFileSize";
import { ExportQueueJobs } from "./StudioExportQueue";
import { Invokes } from "../ui/AppProperties";
import {
  BorderBasis,
  Status,
  WatermarkAnchor,
  type ExportPreset,
  type OutputColorSpace,
} from "../ui/ExportImportProperties";
import { useSettingsStore } from "../../store/useSettingsStore";
import {
  DEFAULT_RECIPE_DRAFT,
  RESIZE_MODES,
  defaultResizeValue,
  draftFromPreset,
  exportSettingsFromDraft,
  presetFromDraft,
  printRadius,
  resizeRange,
  savedRecipes,
  subfolderName,
  subfolderProblem,
  watermarkReady,
  type RecipeDraft,
  type ResizeMode,
  COLOR_SPACES,
  COLOR_SPACE_NAMES,
} from "../../utils/exportRecipes";
import { selectOnFocus } from "../../utils/selectOnFocus";

const CHECKED_KEY = "rawtonic.export.recipes";
const readChecked = (): string[] => {
  try {
    const value = JSON.parse(localStorage.getItem(CHECKED_KEY) ?? "[]");
    return Array.isArray(value) ? value : [];
  } catch {
    return [];
  }
};
const writeChecked = (ids: string[]) => {
  try {
    localStorage.setItem(CHECKED_KEY, JSON.stringify(ids));
  } catch {
    // Remembering the ticks is a convenience.
  }
};

// Format names stay as they are; only the LUT entry has words to translate.
const FORMATS: { extension: string; name: string; translated?: boolean }[] = [
  { extension: "jpg", name: "JPEG" },
  { extension: "tiff", name: "TIFF" },
  { extension: "png", name: "PNG" },
  { extension: "webp", name: "WebP" },
  { extension: "jxl", name: "JPEG XL" },
  { extension: "avif", name: "AVIF" },
  // The photo's colour edits as a 3D LUT for video or other software.
  { extension: "cube", name: "CUBE LUT", translated: true },
];
const QUALITY_FORMATS = ["jpg", "webp", "jxl", "avif"];


const RESIZE_UNITS: Record<ResizeMode, string> = {
  longEdge: "px",
  shortEdge: "px",
  width: "px",
  height: "px",
  dimensions: "px",
  megapixels: "MP",
  percentage: "%",
};

const ANCHORS = [
  { anchor: WatermarkAnchor.TopLeft, name: "topLeft" },
  { anchor: WatermarkAnchor.TopCenter, name: "topCenter" },
  { anchor: WatermarkAnchor.TopRight, name: "topRight" },
  { anchor: WatermarkAnchor.CenterLeft, name: "centerLeft" },
  { anchor: WatermarkAnchor.Center, name: "center" },
  { anchor: WatermarkAnchor.CenterRight, name: "centerRight" },
  { anchor: WatermarkAnchor.BottomLeft, name: "bottomLeft" },
  { anchor: WatermarkAnchor.BottomCenter, name: "bottomCenter" },
  { anchor: WatermarkAnchor.BottomRight, name: "bottomRight" },
] as const;

const fileName = (path: string) => path.split(/[\\/]/).pop() ?? path;

/**
 * `onStarted`: called once exports are queued, as the dialog closes then
 * (Lightroom's Export closes; progress stays on the top bar's button).
 */
export default function StudioExport({ onStarted }: { onStarted?: () => void } = {}) {
  const { t } = useTranslation();
  const selected = useLibraryStore((s) => s.multiSelectedPaths);
  const image = useEditorStore((s) => s.selectedImage);
  const state = useProcessStore((s) => s.exportState);
  const paths = selected.length ? selected : image ? [image.path] : [];
  const session = useLibraryStore((s) => s.sessionDirectory);
  // Capture One sessions export into their Output folder by default.
  const [draft, setDraft] = useState<RecipeDraft>(() => ({
    ...DEFAULT_RECIPE_DRAFT,
    where: session ? "output" : "choose",
  }));
  const set = (patch: Partial<RecipeDraft>) =>
    setDraft((current) => ({ ...current, ...patch }));
  const [choosing, setChoosing] = useState(false);
  const target = draft.where === "output" && !session ? "choose" : draft.where;
  // A folder chosen in the form is used for this export (and remembered by a
  // recipe saved from it); "choose" without one still asks on Export.
  const form: RecipeDraft = {
    ...draft,
    where: target,
    folder: target === "choose" ? draft.folder : undefined,
  };
  const chooseFolder = async () => {
    const chosen = await open({
      title: t("studio.export.destination.choose"),
      directory: true,
      multiple: false,
      defaultPath: draft.folder,
    });
    if (typeof chosen === "string") set({ where: "choose", folder: chosen });
  };
  const busy =
    state.status === Status.Exporting || state.status === Status.Cancelling;
  const appSettings = useSettingsStore((s) => s.appSettings);
  const recipes = savedRecipes(appSettings?.exportPresets);
  // Capture One's process recipes: tick several, each makes its own export.
  const [checkedIds, setCheckedIds] = useState<string[]>(readChecked);
  const checked = recipes.filter((r) => checkedIds.includes(r.id));
  const [editing, setEditing] = useState<string | null>(null);
  const [newName, setNewName] = useState("");
  const subfolderError =
    subfolderProblem(draft.subfolder) ??
    (validNamingTemplate(draft.subfolder, "export", "folder") ? null : "tokens");
  // The job name of {job}, remembered per library (`exportJob`).
  const [jobName, setJobName] = useState(() => readJobName(session));
  useEffect(() => setJobName(readJobName(session)), [session]);
  const changeJobName = (name: string) => {
    setJobName(name);
    writeJobName(session, name);
  };
  const watermarkMissing = draft.watermark && !watermarkReady(draft);
  // Installed fonts for a text watermark, read once when first needed.
  const [fonts, setFonts] = useState<string[] | null>(null);
  useEffect(() => {
    if (!draft.watermark || draft.watermarkKind !== "text" || fonts) return;
    let active = true;
    invoke<string[]>(Invokes.ListFontFamilies)
      .then((list) => active && setFonts(list ?? []))
      .catch(() => active && setFonts([]));
    return () => {
      active = false;
    };
  }, [draft.watermark, draft.watermarkKind, fonts]);
  const formInvalid = !!subfolderError || watermarkMissing || !validNamingTemplate(draft.template, "export") || !validSequenceStart(draft.sequenceStart);
  const range = resizeRange(draft.resizeMode);

  // The exports a click would start: each ticked recipe, else the form (the
  // recipe shown, when one is). The recipe being edited counts with the
  // form's values even before they are saved to it.
  const editedRecipe = recipes.find((r) => r.id === editing) ?? null;
  const recipeDraft = (recipe: ExportPreset) =>
    draftFromPreset(
      recipe.id === editing && !formInvalid
        ? presetFromDraft(form, recipe.id, recipe.name, recipe)
        : recipe,
    );
  const planned = (
    checked.length
      ? checked.map((recipe) => ({ recipe, draft: recipeDraft(recipe) }))
      : [{ recipe: editedRecipe, draft: form }]
  ).map((job) => ({ ...job, ...exportSettingsFromDraft(job.draft) }));

  // Estimated file size, as the inherited panel showed (first photograph
  // rendered, extrapolated by the backend to the whole selection).
  const [estimated, setEstimated] = useState<{
    request: string;
    size: number;
  } | null>(null);
  const estimateRequest = JSON.stringify({
    paths,
    jobs: planned.map(({ settings, outputFormat }) => ({
      settings,
      outputFormat,
    })),
  });
  // Estimated again once the open photograph's own recipe is in.
  const editPath = image?.isReady ? image.path : null;
  useEffect(() => {
    const { paths, jobs } = JSON.parse(estimateRequest) as {
      paths: string[];
      jobs: { settings: unknown; outputFormat: string }[];
    };
    if (!paths.length) return;
    let stale = false;
    const timer = setTimeout(async () => {
      try {
        let total = 0;
        for (const job of jobs) {
          const size = await invoke<number>(Invokes.EstimateExportSizes, {
            paths,
            exportSettings: job.settings,
            outputFormat: job.outputFormat,
            ...currentEditForExport(useEditorStore.getState()),
          });
          if (typeof size !== "number") return;
          total += size;
        }
        if (!stale) setEstimated({ request: estimateRequest, size: total });
      } catch {
        // Without an estimate the export still works.
      }
    }, 500);
    return () => {
      stale = true;
      clearTimeout(timer);
    };
  }, [estimateRequest, editPath]);
  // Only the estimate for the current settings is shown.
  const estimate =
    estimated?.request === estimateRequest ? estimated.size : null;

  // The live example: what the form's name and subfolder templates give
  // the first photo to export, from its own metadata (`export_naming_preview`).
  const [named, setNamed] = useState<{ request: string; preview: NamingPreview } | null>(null);
  const namingRequest = JSON.stringify({
    path: paths[0] ?? null,
    exportSettings: { ...exportSettingsFromDraft(form).settings, jobName, recipeName: editedRecipe?.name ?? "" },
    outputFormat: form.format,
    total: paths.length,
    library: submittedFrom(session),
  });
  const namingValid = validNamingTemplate(draft.template, "export") && !subfolderError;
  useEffect(() => {
    const request = JSON.parse(namingRequest) as { path: string | null };
    if (!request.path || !namingValid) return;
    let stale = false;
    const timer = setTimeout(() => {
      invoke<NamingPreview>(Invokes.ExportNamingPreview, request)
        .then((preview) => !stale && preview && setNamed({ request: namingRequest, preview }))
        .catch(() => {
          // The illustrative example stays.
        });
    }, 250);
    return () => {
      stale = true;
      clearTimeout(timer);
    };
  }, [namingRequest, namingValid]);
  const namePreview = named?.request === namingRequest ? named.preview : null;

  /** Changes the saved recipes as they are now in the settings, so several
   * changes in a row (a folder remembered per recipe while exporting) all stay. */
  const changeRecipes = (change: (current: ExportPreset[]) => ExportPreset[]) => {
    const { appSettings: settings, handleSettingsChange } = useSettingsStore.getState();
    if (!settings) return;
    const current = savedRecipes(settings.exportPresets);
    const kept = (settings.exportPresets ?? []).filter(
      (p) => !current.some((r) => r.id === p.id),
    );
    void handleSettingsChange({
      ...settings,
      exportPresets: [...kept, ...change(current)],
    });
  };
  // The settings on the right are always one thing: the current, unsaved
  // settings or the recipe being edited. A recipe's changes are saved to it
  // as they are made (Capture One's recipes), so a ticked recipe exports
  // exactly what it shows; the unsaved settings wait while one is edited.
  const [unsaved, setUnsaved] = useState<RecipeDraft | null>(null);
  const loadRecipe = (preset: ExportPreset) => {
    if (!editing) setUnsaved(draft);
    setDraft(draftFromPreset(preset));
    setEditing(preset.id);
  };
  const editUnsaved = () => {
    if (!editing) return;
    if (unsaved) setDraft(unsaved);
    setUnsaved(null);
    setEditing(null);
  };
  useEffect(() => {
    if (!editedRecipe || formInvalid) return;
    const next = presetFromDraft(form, editedRecipe.id, editedRecipe.name, editedRecipe);
    if (JSON.stringify(next) === JSON.stringify(editedRecipe)) return;
    const timer = setTimeout(() => {
      changeRecipes((saved) =>
        saved.map((r) =>
          r.id === editedRecipe.id ? presetFromDraft(form, r.id, r.name, r) : r,
        ),
      );
    }, 250);
    return () => clearTimeout(timer);
    // The form and recipe are compared by value above.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [draft, editing, formInvalid, appSettings?.exportPresets]);
  const toggleRecipe = (id: string) => {
    const next = checkedIds.includes(id)
      ? checkedIds.filter((c) => c !== id)
      : [...checkedIds, id];
    setCheckedIds(next);
    writeChecked(next);
  };
  const addRecipe = () => {
    const name = newName.trim();
    if (!name) return;
    const preset = presetFromDraft(form, crypto.randomUUID(), name);
    changeRecipes((current) => [...current, preset]);
    setEditing(preset.id);
    setNewName("");
  };
  const deleteRecipe = (id: string) => {
    changeRecipes((current) => current.filter((r) => r.id !== id));
    if (editing === id) editUnsaved();
    if (checkedIds.includes(id)) toggleRecipe(id);
  };
  const chooseWatermark = async () => {
    const file = await open({
      title: t("studio.export.dialogs.watermarkTitle"),
      multiple: false,
      directory: false,
      filters: [
        {
          name: t("studio.export.dialogs.imageFilter"),
          extensions: ["png", "jpg", "jpeg", "tif", "tiff", "webp"],
        },
      ],
    });
    if (typeof file === "string") set({ watermarkPath: file });
  };

  const run = async () => {
    if (!paths.length || choosing || (!checked.length && formInvalid)) return;
    setChoosing(true);
    try {
      if (await runExportJobs(planned, paths)) onStarted?.();
    } finally {
      setChoosing(false);
    }
  };
  const cancel = async () => {
    useProcessStore.getState().setExportState({ status: Status.Cancelling });
    try {
      await invoke(Invokes.CancelExport);
    } catch (error) {
      useProcessStore
        .getState()
        .setExportState({ status: Status.Error, errorMessage: String(error) });
    }
  };
  const formatLabel = (format: (typeof FORMATS)[number]) =>
    format.translated ? t("studio.export.file.cubeLut") : format.name;
  const currentFormat = FORMATS.find((f) => f.extension === draft.format);
  const formatName = currentFormat ? formatLabel(currentFormat) : draft.format;
  /** One planned export in a line: format, quality, size, destination. */
  const describeJob = (job: RecipeDraft) => {
    const format = FORMATS.find((f) => f.extension === job.format);
    const unit = RESIZE_UNITS[job.resizeMode];
    const subfolder = subfolderName(job.subfolder);
    const size =
      job.resizeMode === "dimensions"
        ? `${job.resizeValue}×${job.resizeValue2} ${unit}`
        : `${job.resizeValue}${unit === "%" ? unit : ` ${unit}`}`;
    const where =
      job.where === "output" && session
        ? t("studio.export.destination.output")
        : job.where === "original"
          ? t("studio.export.destination.original")
          : job.folder
            ? fileName(job.folder)
            : t("studio.export.destination.choose");
    return [
      format ? formatLabel(format) : job.format,
      QUALITY_FORMATS.includes(job.format) &&
        t("studio.export.plan.quality", { quality: job.quality }),
      job.resize
        ? `${t(`studio.export.sizing.modes.${job.resizeMode}`)} ${size}`
        : t("studio.export.plan.fullSize"),
      subfolder ? `${where} / ${subfolder}` : where,
    ]
      .filter(Boolean)
      .join(" · ");
  };
  const resizeLabel = t("studio.export.sizing.valueLabel", {
    name: t(`studio.export.sizing.modes.${draft.resizeMode}`),
    unit: RESIZE_UNITS[draft.resizeMode],
  });
  return (
    <form
      className="studio-export-form"
      onSubmit={(e) => {
        e.preventDefault();
        void run();
      }}
    >
      <p className="studio-export-selection">{t("studio.export.selected", { count: paths.length })}</p>
      <div className="studio-export-layout">
      <aside className="studio-export-sidebar">
      <fieldset disabled={choosing} className="studio-export-recipes">
        <legend>{t("studio.export.recipes.legend")}</legend>
        <p className="studio-export-recipe-hint">{t("naming.recipeHint")}</p>
        {recipes.length === 0 && (
          <small>{t("studio.export.recipes.empty")}</small>
        )}
        <ul>
          <li className={editing ? "" : "is-editing"}>
            <button
              type="button"
              className="studio-export-current"
              aria-pressed={!editing}
              onClick={editUnsaved}
            >
              {t("studio.export.recipes.current")}
            </button>
          </li>
          {recipes.map((recipe) => (
            <li
              key={recipe.id}
              className={editing === recipe.id ? "is-editing" : ""}
            >
              <input
                type="checkbox"
                aria-label={t("studio.export.recipes.exportWith", {
                  name: recipe.name,
                })}
                checked={checkedIds.includes(recipe.id)}
                onChange={() => toggleRecipe(recipe.id)}
              />
              <button
                type="button"
                onClick={() => loadRecipe(recipe)}
                title={t("studio.export.recipes.edit")}
              >
                {recipe.name}
              </button>
              <button
                type="button"
                aria-label={t("studio.export.recipes.delete", {
                  name: recipe.name,
                })}
                onClick={() => deleteRecipe(recipe.id)}
              >
                ×
              </button>
            </li>
          ))}
        </ul>
        <div className="studio-export-recipe-actions">
          <input
            aria-label={t("studio.export.recipes.newName")}
            placeholder={t("studio.export.recipes.newName")}
            value={newName}
            onChange={(e) => setNewName(e.target.value)}
            onKeyDown={(e) => {
              // Return saves the recipe; without this it submits the form
              // and starts an export.
              if (e.key !== "Enter" || e.nativeEvent.isComposing) return;
              e.preventDefault();
              if (newName.trim() && !formInvalid) addRecipe();
            }}
          />
          <button
            type="button"
            disabled={!newName.trim() || formInvalid}
            onClick={addRecipe}
          >
            {t("studio.export.recipes.save")}
          </button>
        </div>
      </fieldset>
      </aside>
      <div className="studio-export-settings">
      <p className="studio-export-editing" role="status">
        {editedRecipe
          ? t("studio.export.recipes.editingRecipe", { name: editedRecipe.name })
          : checked.length
            ? t("studio.export.recipes.currentUnused")
            : t("studio.export.recipes.editingCurrent")}
      </p>
      <fieldset disabled={choosing}>
        <legend>{t("studio.export.destination.legend")}</legend>
        <label>
          {t("studio.export.destination.exportTo")}
          <select
            aria-label={t("studio.export.destination.aria")}
            value={target}
            onChange={(e) => {
              const where = e.target.value as RecipeDraft["where"];
              // Choosing a folder asks for it now, not only on Export.
              if (where === "choose") void chooseFolder();
              else set({ where });
            }}
          >
            {session && (
              <option value="output">
                {t("studio.export.destination.output")}
              </option>
            )}
            <option value="original">
              {t("studio.export.destination.original")}
            </option>
            <option value="choose">
              {target === "choose" && draft.folder
                ? fileName(draft.folder)
                : t("studio.export.destination.choose")}
            </option>
          </select>
          {target === "choose" && (
            // Also before a folder is set: the select already shows
            // "Choose a folder…" then, so picking it again would not fire.
            <button
              type="button"
              title={draft.folder ?? t("studio.export.destination.choose")}
              aria-label={t("studio.export.destination.choose")}
              onClick={() => void chooseFolder()}
            >
              {draft.folder ? "…" : t("studio.export.watermark.choose")}
            </button>
          )}
        </label>
        <NamingEditor
          kind="folder"
          mode="export"
          value={draft.subfolder}
          onChange={(subfolder) => set({ subfolder })}
          label={t("studio.export.destination.subfolder")}
          original={paths[0]}
          extension={draft.format}
          job={jobName}
          total={paths.length}
          preview={namePreview ? { name: namePreview.folder ?? "" } : null}
          invalidMessage={
            subfolderError && subfolderError !== "tokens"
              ? t(`studio.export.destination.errors.${subfolderError}`)
              : null
          }
        />
        {target !== "original" && (
          <label className="studio-check">
            <input
              type="checkbox"
              checked={draft.preserveFolders}
              onChange={(e) => set({ preserveFolders: e.target.checked })}
            />
            {t("studio.export.destination.keepStructure")}
          </label>
        )}
      </fieldset>
      <fieldset disabled={choosing}>
        <legend>{t("studio.export.file.legend")}</legend>
        <label>
          {t("studio.export.file.format")}
          <select
            aria-label={t("studio.export.file.format")}
            value={draft.format}
            onChange={(e) => set({ format: e.target.value })}
          >
            {FORMATS.map((f) => (
              <option key={f.extension} value={f.extension}>
                {formatLabel(f)}
              </option>
            ))}
          </select>
        </label>
        {QUALITY_FORMATS.includes(draft.format) && (
          <label>
            {t("studio.export.file.quality")}
            <input
              {...selectOnFocus}
              aria-label={t("studio.export.file.qualityAria", {
                format: formatName,
              })}
              type="number"
              min="1"
              max="100"
              required
              value={draft.quality}
              onChange={(e) => set({ quality: Number(e.target.value) })}
            />
          </label>
        )}
        {draft.format === "jxl" && (
          <small>{t("studio.export.file.jxlLossless")}</small>
        )}
        {draft.format === "tiff" && (
          <label>
            {t("studio.export.file.bitDepth")}
            <select
              aria-label={t("studio.export.file.bitDepth")}
              value={draft.depth}
              onChange={(e) => set({ depth: Number(e.target.value) as 8 | 16 })}
            >
              <option value="16">
                {t("studio.export.file.bits", { bits: 16 })}
              </option>
              <option value="8">
                {t("studio.export.file.bits", { bits: 8 })}
              </option>
            </select>
          </label>
        )}
        {draft.format !== "cube" && (
          <label>
            {t("studio.export.file.colorSpace")}
            <select
              aria-label={t("studio.export.file.colorSpace")}
              value={draft.colorSpace}
              onChange={(e) => set({ colorSpace: e.target.value as OutputColorSpace })}
            >
              {COLOR_SPACES.map((space) => (
                <option key={space} value={space}>
                  {COLOR_SPACE_NAMES[space]}
                </option>
              ))}
            </select>
          </label>
        )}
        {draft.colorSpace === "proPhoto" &&
          draft.format !== "cube" &&
          !(draft.format === "tiff" && draft.depth === 16) && (
            <small>{t("studio.export.file.proPhotoWarning")}</small>
          )}
      </fieldset>
      <fieldset disabled={choosing} className="studio-export-naming">
        <legend>{t("naming.title")}</legend>
        <label className="studio-export-job">
          {t("studio.export.job.label")}
          <input
            value={jobName}
            placeholder={t("studio.export.job.placeholder")}
            maxLength={120}
            spellCheck={false}
            onChange={(e) => changeJobName(e.target.value)}
          />
        </label>
        <NamingEditor mode="export" value={draft.template} onChange={(template) => set({ template })}
          label={t("studio.export.file.template")} original={paths[0]} extension={draft.format} total={paths.length}
          job={jobName} preview={namePreview ? { name: namePreview.name } : null}
          sequenceStart={draft.sequenceStart} onSequenceStartChange={(sequenceStart) => set({ sequenceStart })} />
      </fieldset>
      <fieldset disabled={choosing}>
        <legend>{t("studio.export.sizing.legend")}</legend>
        <label className="studio-check">
          <input
            type="checkbox"
            checked={draft.resize}
            onChange={(e) => set({ resize: e.target.checked })}
          />
          {t("studio.export.sizing.resize")}
        </label>
        {draft.resize && (
          <>
            <label>
              {t("studio.export.sizing.fit")}
              <select
                aria-label={t("studio.export.sizing.modeAria")}
                value={draft.resizeMode}
                onChange={(e) => {
                  const mode = e.target.value as ResizeMode;
                  const unit = (m: ResizeMode) => RESIZE_UNITS[m];
                  set({
                    resizeMode: mode,
                    resizeValue:
                      unit(mode) === unit(draft.resizeMode)
                        ? draft.resizeValue
                        : defaultResizeValue(mode),
                  });
                }}
              >
                {RESIZE_MODES.map((mode) => (
                  <option key={mode} value={mode}>
                    {t(`studio.export.sizing.modes.${mode}`)}
                  </option>
                ))}
              </select>
            </label>
            <label>
              {resizeLabel}
              <input
                {...selectOnFocus}
                aria-label={resizeLabel}
                type="number"
                required
                min={range.min}
                max={range.max}
                step={range.step}
                value={draft.resizeValue}
                onChange={(e) => set({ resizeValue: Number(e.target.value) })}
              />
            </label>
            {draft.resizeMode === "dimensions" && (
              <label>
                × (px)
                <input
                  {...selectOnFocus}
                  aria-label={t("studio.export.sizing.otherSideAria")}
                  type="number"
                  required
                  min={range.min}
                  max={range.max}
                  step={range.step}
                  value={draft.resizeValue2}
                  onChange={(e) =>
                    set({ resizeValue2: Number(e.target.value) })
                  }
                />
              </label>
            )}
            <label className="studio-check">
              <input
                type="checkbox"
                checked={draft.dontEnlarge}
                onChange={(e) => set({ dontEnlarge: e.target.checked })}
              />
              {t("studio.export.sizing.dontEnlarge")}
            </label>
          </>
        )}
        <small>
          {!draft.resize
            ? t("studio.export.sizing.hints.fullResolution")
            : draft.resizeMode === "dimensions"
              ? t("studio.export.sizing.hints.box")
              : draft.dontEnlarge
                ? t("studio.export.sizing.hints.noEnlarge")
                : t("studio.export.sizing.hints.enlarge")}
        </small>
      </fieldset>
      <details className="studio-export-section">
        <summary>{t("studio.export.sharpening.legend")}</summary>
      <fieldset disabled={choosing}>
        <legend>{t("studio.export.sharpening.legend")}</legend>
        <label>
          {t("studio.export.sharpening.sharpenFor")}
          <select
            aria-label={t("studio.export.sharpening.sharpenFor")}
            value={draft.sharpening}
            onChange={(e) =>
              set({ sharpening: e.target.value as RecipeDraft["sharpening"] })
            }
          >
            <option value="off">{t("studio.export.sharpening.off")}</option>
            <option value="screen">
              {t("studio.export.sharpening.screen")}
            </option>
            <option value="print">{t("studio.export.sharpening.print")}</option>
          </select>
        </label>
        {draft.sharpening !== "off" && (
          <>
            <label>
              {t("studio.export.sharpening.amount")}
              <input
                {...selectOnFocus}
                aria-label={t("studio.export.sharpening.amountAria")}
                type="number"
                min="0"
                max="500"
                value={draft.sharpenAmount}
                onChange={(e) => set({ sharpenAmount: Number(e.target.value) })}
              />
            </label>
            {draft.sharpening === "screen" ? (
              <label>
                {t("studio.export.sharpening.radius")}
                <input
                  {...selectOnFocus}
                  aria-label={t("studio.export.sharpening.radiusAria")}
                  type="number"
                  min="0.2"
                  max="10"
                  step="0.1"
                  value={draft.sharpenRadius}
                  onChange={(e) =>
                    set({ sharpenRadius: Number(e.target.value) })
                  }
                />
              </label>
            ) : (
              <>
                <label>
                  {t("studio.export.sharpening.distance")}
                  <input
                    {...selectOnFocus}
                    aria-label={t("studio.export.sharpening.distance")}
                    type="number"
                    min="10"
                    max="1000"
                    value={draft.printDistanceCm}
                    onChange={(e) =>
                      set({ printDistanceCm: Number(e.target.value) })
                    }
                  />
                </label>
                <label>
                  {t("studio.export.sharpening.ppi")}
                  <input
                    {...selectOnFocus}
                    aria-label={t("studio.export.sharpening.ppi")}
                    type="number"
                    min="50"
                    max="2400"
                    value={draft.printPpi}
                    onChange={(e) => set({ printPpi: Number(e.target.value) })}
                  />
                </label>
              </>
            )}
            <label>
              {t("studio.export.sharpening.threshold")}
              <input
                {...selectOnFocus}
                aria-label={t("studio.export.sharpening.thresholdAria")}
                type="number"
                min="0"
                max="50"
                step="0.5"
                value={draft.sharpenThreshold}
                onChange={(e) =>
                  set({ sharpenThreshold: Number(e.target.value) })
                }
              />
            </label>
          </>
        )}
        <small>
          {draft.sharpening === "off"
            ? t("studio.export.sharpening.hints.off")
            : draft.sharpening === "print"
              ? t("studio.export.sharpening.hints.print", {
                  radius: printRadius(
                    draft.printDistanceCm,
                    draft.printPpi,
                  ).toFixed(1),
                })
              : t("studio.export.sharpening.hints.screen")}
        </small>
      </fieldset>
      </details>
      {!draft.metadata && <small>{t("studio.export.metadata.rightsKept")}</small>}
      <details className="studio-export-section">
        <summary>{t("studio.export.metadata.legend")}</summary>
      <fieldset disabled={choosing}>
        <legend>{t("studio.export.metadata.legend")}</legend>
        <label className="studio-check">
          <input
            type="checkbox"
            checked={draft.metadata}
            onChange={(e) => set({ metadata: e.target.checked })}
          />
          {t("studio.export.metadata.include")}
        </label>
        <label className="studio-check">
          <input
            type="checkbox"
            checked={!!draft.keepCreatorContact}
            disabled={draft.metadata}
            onChange={(e) => set({ keepCreatorContact: e.target.checked })}
          />
          {t("studio.export.metadata.keepCreatorContact")}
        </label>
        <label className="studio-check">
          <input
            type="checkbox"
            checked={draft.stripGps}
            disabled={!draft.metadata}
            onChange={(e) => set({ stripGps: e.target.checked })}
          />
          {t("studio.export.metadata.stripGps")}
        </label>
        <label className="studio-check">
          <input
            type="checkbox"
            checked={draft.preserveTimestamps}
            onChange={(e) => set({ preserveTimestamps: e.target.checked })}
          />
          {t("studio.export.metadata.timestamps")}
        </label>
      </fieldset>
      </details>
      <details className="studio-export-section">
        <summary>{t("studio.export.watermark.legend")}</summary>
      <fieldset disabled={choosing}>
        <legend>{t("studio.export.watermark.legend")}</legend>
        <label className="studio-check">
          <input
            type="checkbox"
            checked={draft.watermark}
            onChange={(e) => set({ watermark: e.target.checked })}
          />
          {t("studio.export.watermark.add")}
        </label>
        {draft.watermark && (
          <>
            <label>
              {t("studio.export.watermark.kind")}
              <select
                aria-label={t("studio.export.watermark.kindAria")}
                value={draft.watermarkKind}
                onChange={(e) =>
                  set({
                    watermarkKind: e.target
                      .value as RecipeDraft["watermarkKind"],
                  })
                }
              >
                <option value="image">
                  {t("studio.export.watermark.image")}
                </option>
                <option value="text">{t("studio.export.watermark.text")}</option>
              </select>
            </label>
            {draft.watermarkKind === "image" ? (
              <div className="studio-export-row">
                {t("studio.export.watermark.image")}
                <span className="studio-export-file">
                  <span title={draft.watermarkPath ?? undefined}>
                    {draft.watermarkPath
                      ? fileName(draft.watermarkPath)
                      : t("studio.export.none")}
                  </span>
                  <button
                    type="button"
                    aria-label={t("studio.export.watermark.chooseAria")}
                    onClick={() => void chooseWatermark()}
                  >
                    {t("studio.export.watermark.choose")}
                  </button>
                </span>
              </div>
            ) : (
              <>
                <label>
                  {t("studio.export.watermark.text")}
                  <textarea
                    aria-label={t("studio.export.watermark.textAria")}
                    rows={2}
                    value={draft.watermarkText}
                    onChange={(e) => set({ watermarkText: e.target.value })}
                  />
                </label>
                <label>
                  {t("studio.export.watermark.font")}
                  <select
                    aria-label={t("studio.export.watermark.fontAria")}
                    value={draft.watermarkFont ?? ""}
                    onChange={(e) =>
                      set({ watermarkFont: e.target.value || null })
                    }
                  >
                    <option value="">
                      {t("studio.export.watermark.systemFont")}
                    </option>
                    {draft.watermarkFont &&
                      !fonts?.includes(draft.watermarkFont) && (
                        <option value={draft.watermarkFont}>
                          {draft.watermarkFont}
                        </option>
                      )}
                    {(fonts ?? []).map((family) => (
                      <option key={family} value={family}>
                        {family}
                      </option>
                    ))}
                  </select>
                </label>
                <label>
                  {t("studio.export.watermark.color")}
                  <input
                    aria-label={t("studio.export.watermark.colorAria")}
                    type="color"
                    value={draft.watermarkColor}
                    onChange={(e) => set({ watermarkColor: e.target.value })}
                  />
                </label>
                <label className="studio-check">
                  <input
                    type="checkbox"
                    checked={draft.watermarkShadow}
                    onChange={(e) => set({ watermarkShadow: e.target.checked })}
                  />
                  {t("studio.export.watermark.shadow")}
                </label>
              </>
            )}
            {watermarkMissing && (
              <small className="studio-export-error" role="alert">
                {draft.watermarkKind === "text"
                  ? t("studio.export.watermark.missingText")
                  : t("studio.export.watermark.missingImage")}
              </small>
            )}
            <div
              className="studio-export-anchor"
              role="radiogroup"
              aria-label={t("studio.export.watermark.positionAria")}
            >
              {ANCHORS.map(({ anchor, name }) => (
                <input
                  key={anchor}
                  type="radio"
                  name="watermark-anchor"
                  aria-label={t(`studio.export.watermark.anchors.${name}`)}
                  title={t(`studio.export.watermark.anchors.${name}`)}
                  checked={draft.watermarkAnchor === anchor}
                  onChange={() => set({ watermarkAnchor: anchor })}
                />
              ))}
            </div>
            <label>
              {t("studio.export.watermark.width")}
              <input
                {...selectOnFocus}
                type="number"
                min="1"
                max="100"
                required
                value={draft.watermarkScale}
                onChange={(e) =>
                  set({ watermarkScale: Number(e.target.value) })
                }
              />
            </label>
            <label>
              {t("studio.export.watermark.inset")}
              <input
                {...selectOnFocus}
                type="number"
                min="0"
                max="25"
                required
                value={draft.watermarkSpacing}
                onChange={(e) =>
                  set({ watermarkSpacing: Number(e.target.value) })
                }
              />
            </label>
            <label>
              {t("studio.export.watermark.opacity")}
              <input
                {...selectOnFocus}
                type="number"
                min="0"
                max="100"
                required
                value={draft.watermarkOpacity}
                onChange={(e) =>
                  set({ watermarkOpacity: Number(e.target.value) })
                }
              />
            </label>
          </>
        )}
      </fieldset>
      </details>
      <details className="studio-export-more">
        <summary>{t("studio.export.more")}</summary>
        <fieldset disabled={choosing}>
          <legend>{t("studio.export.border.legend")}</legend>
          <label className="studio-check">
            <input
              type="checkbox"
              checked={draft.border}
              onChange={(e) => set({ border: e.target.checked })}
            />
            {t("studio.export.border.add")}
          </label>
          {draft.border && (
            <>
              <label>
                {t("studio.export.border.measuredOn")}
                <select
                  aria-label={t("studio.export.border.measuredOnAria")}
                  value={draft.borderBasis}
                  onChange={(e) =>
                    set({ borderBasis: e.target.value as BorderBasis })
                  }
                >
                  <option value={BorderBasis.LongEdge}>
                    {t("studio.export.border.longEdge")}
                  </option>
                  <option value={BorderBasis.ShortEdge}>
                    {t("studio.export.border.shortEdge")}
                  </option>
                  <option value={BorderBasis.EachEdge}>
                    {t("studio.export.border.eachEdge")}
                  </option>
                </select>
              </label>
              <label>
                {t("studio.export.border.leftRight")}
                <input
                  {...selectOnFocus}
                  type="number"
                  min="0"
                  max="100"
                  step="any"
                  required
                  value={draft.borderHorizontal}
                  onChange={(e) =>
                    set({ borderHorizontal: Number(e.target.value) })
                  }
                />
              </label>
              <label>
                {t("studio.export.border.topBottom")}
                <input
                  {...selectOnFocus}
                  type="number"
                  min="0"
                  max="100"
                  step="any"
                  required
                  value={draft.borderVertical}
                  onChange={(e) =>
                    set({ borderVertical: Number(e.target.value) })
                  }
                />
              </label>
              <label>
                {t("studio.export.border.color")}
                <input
                  type="color"
                  value={draft.borderColor}
                  onChange={(e) => set({ borderColor: e.target.value })}
                />
              </label>
            </>
          )}
        </fieldset>
        <fieldset disabled={choosing}>
          <legend>{t("studio.export.canvas.legend")}</legend>
          <label className="studio-check">
            <input
              type="checkbox"
              checked={draft.pad}
              onChange={(e) => set({ pad: e.target.checked })}
            />
            {t("studio.export.canvas.pad")}
          </label>
          {draft.pad && (
            <>
              <label>
                {t("studio.export.canvas.ratio")}
                <span className="studio-export-ratio">
                  <input
                    {...selectOnFocus}
                    aria-label={t("studio.export.canvas.ratioWidth")}
                    type="number"
                    min="0.01"
                    max="1000"
                    step="any"
                    required
                    value={draft.padWidth}
                    onChange={(e) => set({ padWidth: Number(e.target.value) })}
                  />
                  :
                  <input
                    {...selectOnFocus}
                    aria-label={t("studio.export.canvas.ratioHeight")}
                    type="number"
                    min="0.01"
                    max="1000"
                    step="any"
                    required
                    value={draft.padHeight}
                    onChange={(e) => set({ padHeight: Number(e.target.value) })}
                  />
                </span>
              </label>
              <label>
                {t("studio.export.canvas.color")}
                <input
                  type="color"
                  value={draft.padColor}
                  onChange={(e) => set({ padColor: e.target.value })}
                />
              </label>
            </>
          )}
        </fieldset>
        <fieldset disabled={choosing}>
          <legend>{t("studio.export.masks.legend")}</legend>
          <label className="studio-check">
            <input
              type="checkbox"
              checked={draft.exportMasks}
              onChange={(e) => set({ exportMasks: e.target.checked })}
            />
            {t("studio.export.masks.exportEach")}
          </label>
          <small>{t("studio.export.masks.hint")}</small>
        </fieldset>
      </details>
      {estimate !== null && paths.length > 0 && (
        <p className="studio-export-estimate">
          {paths.length > 1
            ? t("studio.export.estimatePerFile", {
                size: formatFileSize(estimate),
                perFile: formatFileSize(
                  estimate / paths.length / planned.length,
                ),
              })
            : t("studio.export.estimate", { size: formatFileSize(estimate) })}
        </p>
      )}
      {busy && (
        <div role="status">
          <progress
            max={state.progress.total || 1}
            value={state.progress.current}
          />
          <p>
            {state.status === Status.Cancelling
              ? t("studio.export.progress.cancelling")
              : t("studio.export.progress.exporting", {
                  current: state.progress.current,
                  total: state.progress.total,
                })}
          </p>
        </div>
      )}
      {state.status === Status.Error && (
        <p className="studio-export-error" role="alert">
          <OperationError detail={renderErrorMessage(state.errorMessage)} />
        </p>
      )}
      {state.status === Status.Success && (
        <p role="status">{t("studio.export.progress.complete")}</p>
      )}
      <ExportQueueJobs />
      </div>
      </div>
      <footer>
        <section
          className="studio-export-plan"
          aria-label={t("studio.export.plan.legend")}
        >
          <h4>{t("studio.export.plan.legend")}</h4>
          <ul>
            {planned.map((job) => (
              <li key={job.recipe?.id ?? "current"}>
                <strong>
                  {job.recipe?.name ?? t("studio.export.recipes.current")}
                </strong>
                <span>{describeJob(job.draft)}</span>
              </li>
            ))}
          </ul>
        </section>
        {busy && (
          <button
            type="button"
            onClick={() => void cancel()}
            disabled={state.status === Status.Cancelling}
          >
            {t("studio.export.actions.cancel")}
          </button>
        )}
        <button
          type="submit"
          disabled={
            !paths.length || choosing || (!checked.length && formInvalid)
          }
          className="studio-export-submit"
        >
          {choosing
            ? t("studio.export.actions.choosing")
            : busy
              ? t("studio.export.actions.addToQueue")
              : checked.length > 1
                ? t("studio.export.actions.exportWithRecipes", {
                    count: checked.length,
                  })
                : checked.length === 1 || planned[0].recipe
                  ? t("studio.export.recipes.exportWith", {
                      name: (checked[0] ?? planned[0].recipe)?.name,
                    })
                  : target === "choose"
                    ? t("studio.export.actions.chooseAndExport")
                    : t("studio.export.actions.export")}
        </button>
      </footer>

    </form>
  );
}

export { describeExportJob } from "./StudioExportQueue";
