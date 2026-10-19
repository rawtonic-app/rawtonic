import { useEffect, useMemo, useRef, useState, type KeyboardEvent, type MouseEvent } from "react";
import { open, save } from "@tauri-apps/plugin-dialog";
import { ArrowDownAZ, ChevronRight, FolderInput, FolderPlus, Plus, Sparkles } from "lucide-react";
import { toast } from "react-toastify";
import { useTranslation } from "react-i18next";
import { panelsShowReady, useEditorStore } from "../../store/useEditorStore";
import { useLibraryStore } from "../../store/useLibraryStore";
import { useEditorActions } from "../../hooks/useEditorActions";
import { usePresets, type UserPreset } from "../../hooks/usePresets";
import { useContextMenu } from "../../context/ContextMenuContext";
import type { Preset } from "../ui/AppProperties";
import { ToolGroup } from "./StudioDevelop";
import { blendPreset } from "../../utils/presetAmount";
import { withAiDenoiseVersion, withDiffractionVersion, withGrainStructure, withSharpenVersion } from "../../utils/adjustments";

type Entry = { folder: { id: string; name: string } | null; presets: Preset[] };

/** Root presets first (as one unnamed group), then folders in their order. */
export function groupPresets(items: UserPreset[], query = ""): Entry[] {
  const match = (preset: Preset) => preset.name.toLowerCase().includes(query.trim().toLowerCase());
  const root: Preset[] = [];
  const folders: Entry[] = [];
  for (const item of items) {
    if (item.preset) {
      if (match(item.preset)) root.push(item.preset);
    } else if (item.folder) {
      const presets = item.folder.children.filter(match);
      if (presets.length || !query.trim())
        folders.push({ folder: { id: item.folder.id ?? "", name: item.folder.name ?? "" }, presets });
    }
  }
  return root.length ? [{ folder: null, presets: root }, ...folders] : folders;
}

/** The photographs a click applies to: the selection (Capture One), else the open photo. */
function applyTargets() {
  const open = useEditorStore.getState().selectedImage?.path;
  const selected = useLibraryStore.getState().multiSelectedPaths;
  return selected.length > 1 ? selected : open ? [open] : [];
}

function NewPresetForm({
  folders,
  onSave,
  onCancel,
}: {
  folders: { id: string; name: string }[];
  onSave(options: { name: string; folderId: string | null; style: boolean; crop: boolean; masks: boolean }): void;
  onCancel(): void;
}) {
  const { t } = useTranslation();
  const [name, setName] = useState("");
  const [folderId, setFolderId] = useState("");
  const [style, setStyle] = useState(false);
  const [crop, setCrop] = useState(false);
  const [masks, setMasks] = useState(false);
  const save = () => name.trim() && onSave({ name: name.trim(), folderId: folderId || null, style, crop, masks });
  return (
    <form
      className="studio-preset-form"
      aria-label={t("studio.presets.newPreset")}
      onSubmit={(e) => {
        e.preventDefault();
        save();
      }}
      onKeyDown={(e) => {
        e.stopPropagation();
        if (e.key === "Escape") onCancel();
      }}
    >
      <input
        className="studio-metadata-input"
        aria-label={t("studio.presets.form.nameAria")}
        placeholder={t("studio.presets.form.namePlaceholder")}
        autoFocus
        value={name}
        onChange={(e) => setName(e.target.value)}
      />
      <label className="studio-check">
        <input type="checkbox" checked={!style} onChange={(e) => setStyle(!e.target.checked)} />
        {t("studio.presets.form.onlyChanged")}
      </label>
      <label className="studio-check">
        <input type="checkbox" checked={crop} onChange={(e) => setCrop(e.target.checked)} />
        {t("studio.presets.form.crop")}
      </label>
      <label className="studio-check">
        <input type="checkbox" checked={masks} onChange={(e) => setMasks(e.target.checked)} />
        {t("studio.presets.form.masks")}
      </label>
      {folders.length > 0 && (
        <select
          aria-label={t("studio.presets.form.folderAria")}
          value={folderId}
          onChange={(e) => setFolderId(e.target.value)}
        >
          <option value="">{t("studio.presets.noFolder")}</option>
          {folders.map((folder) => (
            <option key={folder.id} value={folder.id}>
              {folder.name}
            </option>
          ))}
        </select>
      )}
      <div className="studio-preset-form-actions">
        <button type="button" onClick={onCancel}>
          {t("studio.presets.cancel")}
        </button>
        <button type="submit" disabled={!name.trim()}>
          {t("studio.presets.form.save")}
        </button>
      </div>
    </form>
  );
}

/** Lightroom's Presets panel / Capture One's Styles and Presets: presets by
 * folder, a click applies one to the open photo or the selection, "+" saves
 * the current settings, the context menu renames, updates or deletes. */
const currentAdjustments = () => useEditorStore.getState().adjustments;

export default function StudioPresets() {
  const { t } = useTranslation();
  const selectedImage = useEditorStore((s) => s.selectedImage);
  const selectionSize = useLibraryStore((s) => s.multiSelectedPaths.length);
  const { applyAdjustmentsToPaths, setAdjustments } = useEditorActions();
  const {
    presets,
    addPreset,
    deleteItem,
    renameItem,
    overwritePreset,
    duplicatePreset,
    importPresetsFromFiles,
    exportPresetsToFile,
    addFolder,
    movePreset,
    sortAllPresetsAlphabetically,
    isLoading,
  } = usePresets(currentAdjustments, Boolean(selectedImage?.isRaw), selectedImage?.width, selectedImage?.height);
  // Lightroom's and Capture One's preview: hovering a preset shows the open
  // photo with it in the viewer, without applying it.
  const hoverTimer = useRef<number | null>(null);
  const previewing = useRef(false);
  const endPreview = () => {
    if (hoverTimer.current) window.clearTimeout(hoverTimer.current);
    hoverTimer.current = null;
    if (previewing.current) {
      previewing.current = false;
      useEditorStore.getState().setEditor({ previewOverride: null });
    }
  };
  const startPreview = (preset: Preset) => {
    if (hoverTimer.current) window.clearTimeout(hoverTimer.current);
    hoverTimer.current = window.setTimeout(() => {
      const { selectedImage: image, adjustments: current, setEditor } = useEditorStore.getState();
      if (!image?.isReady) return;
      previewing.current = true;
      setEditor({ previewOverride: { ...current, ...withDiffractionVersion(withSharpenVersion(withAiDenoiseVersion(withGrainStructure(structuredClone(preset.adjustments))))) } });
    }, 120);
  };
  useEffect(() => endPreview, []);
  // Lightroom's Amount: the last preset applied to the open photo, with the
  // settings it had before, so that its strength can still be changed.
  const [amount, setAmount] = useState<{ path: string; preset: Preset; base: Record<string, unknown>; value: number } | null>(
    null,
  );
  const { showContextMenu } = useContextMenu();
  const [query, setQuery] = useState("");
  const [creating, setCreating] = useState(false);
  const [renaming, setRenaming] = useState<string | null>(null);
  const [deleting, setDeleting] = useState<Preset | null>(null);
  const [collapsed, setCollapsed] = useState<Set<string>>(new Set());
  const [applied, setApplied] = useState<string | null>(null);
  const groups = useMemo(() => groupPresets(presets, query), [presets, query]);
  const folders = groups.flatMap((group) => (group.folder ? [group.folder] : []));
  const count = presets.reduce((n, item) => n + (item.preset ? 1 : item.folder?.children.length ?? 0), 0);
  const ready = Boolean(selectedImage?.isReady);
  // Controls keep their look while the next photograph loads.
  const panelsReady = useEditorStore(panelsShowReady);

  const apply = (preset: Preset) => {
    endPreview();
    const targets = applyTargets();
    if (!targets.length || !ready) return;
    const open = selectedImage?.path;
    setAmount(
      targets.length === 1 && targets[0] === open && open
        ? { path: open, preset, base: structuredClone(currentAdjustments()) as unknown as Record<string, unknown>, value: 100 }
        : null,
    );
    applyAdjustmentsToPaths(structuredClone(preset.adjustments), targets);
    setApplied(preset.id);
  };
  const changeAmount = (value: number) => {
    if (!amount || amount.path !== selectedImage?.path) return;
    setAmount({ ...amount, value });
    const blended = blendPreset(
      amount.base,
      withDiffractionVersion(withSharpenVersion(withAiDenoiseVersion(withGrainStructure(amount.preset.adjustments)))) as unknown as Record<string, unknown>,
      value / 100,
    );
    setAdjustments((current) => ({ ...current, ...amount.base, ...blended }) as typeof current);
  };
  const exportPreset = async (preset: Preset) => {
    const path = await save({
      title: t("studio.presets.exportTitle", { name: preset.name }),
      defaultPath: `${preset.name}.rrpreset`,
      filters: [{ name: t("studio.presets.fileType"), extensions: ["rrpreset"] }],
    });
    if (typeof path !== "string") return;
    try {
      await exportPresetsToFile([{ preset }], path);
      toast.success(t("studio.presets.toasts.exported", { name: preset.name }));
    } catch (error) {
      toast.error(t("studio.presets.toasts.exportFailed", { error: String(error) }));
    }
  };
  const menu = (e: MouseEvent, preset: Preset) => {
    e.preventDefault();
    endPreview();
    showContextMenu(e.clientX, e.clientY, [
      { label: t("studio.presets.menu.rename"), onClick: () => setRenaming(preset.id) },
      {
        label: t("studio.presets.menu.update"),
        disabled: !ready,
        onClick: () => overwritePreset(preset.id),
      },
      { label: t("studio.presets.menu.duplicate"), onClick: () => duplicatePreset(preset.id) },
      {
        label: t("studio.presets.menu.moveTo"),
        disabled: folders.length === 0,
        submenu: [
          { label: t("studio.presets.noFolder"), onClick: () => movePreset(preset.id, null) },
          ...folders.map((folder) => ({ label: folder.name, onClick: () => movePreset(preset.id, folder.id) })),
        ],
      },
      { label: t("studio.presets.menu.export"), onClick: () => void exportPreset(preset) },
      { label: t("studio.presets.menu.delete"), onClick: () => setDeleting(preset) },
    ]);
  };
  // Lightroom's preset groups / Capture One's style folders.
  const [folderNaming, setFolderNaming] = useState(false);
  const [deletingFolder, setDeletingFolder] = useState<{ id: string; name: string; size: number } | null>(null);
  const folderMenu = (e: MouseEvent, folder: { id: string; name: string }, size: number) => {
    e.preventDefault();
    const item = presets.find((p) => p.folder?.id === folder.id);
    showContextMenu(e.clientX, e.clientY, [
      { label: t("studio.presets.menu.rename"), onClick: () => setRenaming(folder.id) },
      {
        label: t("studio.presets.menu.export"),
        disabled: !item,
        onClick: async () => {
          const path = await save({
            title: t("studio.presets.exportTitle", { name: folder.name }),
            defaultPath: `${folder.name}.rrpreset`,
            filters: [{ name: t("studio.presets.fileType"), extensions: ["rrpreset"] }],
          });
          if (typeof path !== "string" || !item) return;
          try {
            await exportPresetsToFile([item], path);
            toast.success(t("studio.presets.toasts.exported", { name: folder.name }));
          } catch (error) {
            toast.error(t("studio.presets.toasts.exportFailed", { error: String(error) }));
          }
        },
      },
      { label: t("studio.presets.menu.delete"), onClick: () => setDeletingFolder({ ...folder, size }) },
    ]);
  };
  // Lightroom presets (.xmp, .lrtemplate), Capture One styles (.costyle,
  // translated into Rawtonic's tools) and Rawtonic's own files.
  const importFiles = async () => {
    const chosen = await open({
      multiple: true,
      title: t("studio.presets.importTitle"),
      filters: [
        {
          name: t("studio.presets.importFilter"),
          extensions: ["rrpreset", "xmp", "lrtemplate", "costyle"],
        },
      ],
    });
    const paths = Array.isArray(chosen) ? chosen : chosen ? [chosen] : [];
    if (!paths.length) return;
    try {
      const { failures, untranslated = [] } = await importPresetsFromFiles(paths);
      const imported = paths.length - failures.length;
      if (failures.length)
        toast.warn(
          t("studio.presets.toasts.importedWithFailures", {
            imported,
            failed: failures.length,
            files: failures.map((f) => f.fileName).join(", "),
          }),
        );
      else toast.success(t("studio.presets.toasts.imported", { count: imported }));
      // What the translation left out, per file, so a look that comes over
      // only partly is not mistaken for a faithful one.
      if (untranslated.length)
        toast.info(
          t("studio.presets.toasts.untranslated", {
            files: untranslated
              .map(
                ({ fileName, settings, profile }) =>
                  `${fileName}: ${settings
                    .map((code) =>
                      code === "profile" && profile
                        ? t("studio.presets.untranslated.profileNamed", { name: profile })
                        : t(`studio.presets.untranslated.${code}`, { defaultValue: code }),
                    )
                    .join(", ")}`,
              )
              .join(" · "),
          }),
          { autoClose: 12000 },
        );
    } catch (error) {
      toast.error(t("studio.presets.toasts.importFailed", { error: String(error) }));
    }
  };
  const renameKeys = (e: KeyboardEvent<HTMLInputElement>, preset: Preset) => {
    e.stopPropagation();
    if (e.key === "Enter") {
      const name = e.currentTarget.value.trim();
      if (name && name !== preset.name) renameItem(preset.id, name);
      setRenaming(null);
    } else if (e.key === "Escape") setRenaming(null);
  };

  return (
    <ToolGroup
      title="Styles & presets"
      label={t("studio.presets.title")}
      open={false}
      actions={
        <>
          <button
            type="button"
            title={t("studio.presets.importButtonTitle")}
            aria-label={t("studio.presets.importButton")}
            onClick={(e) => {
              e.preventDefault();
              void importFiles();
            }}
          >
            <FolderInput size={12} />
          </button>
          <button
            type="button"
            title={t("studio.presets.newFolderTitle")}
            aria-label={t("studio.presets.newFolder")}
            onClick={(e) => {
              e.preventDefault();
              const details = e.currentTarget.closest("details");
              if (details) details.open = true;
              setFolderNaming(true);
            }}
          >
            <FolderPlus size={12} />
          </button>
          <button
            type="button"
            title={t("studio.presets.sortTitle")}
            aria-label={t("studio.presets.sort")}
            disabled={count === 0}
            onClick={(e) => {
              e.preventDefault();
              sortAllPresetsAlphabetically();
            }}
          >
            <ArrowDownAZ size={12} />
          </button>
          <button
            type="button"
            title={t("studio.presets.newPresetTitle")}
            aria-label={t("studio.presets.newPreset")}
            disabled={!panelsReady}
            onClick={(e) => {
              e.preventDefault();
              const details = e.currentTarget.closest("details");
              if (details) details.open = true;
              setCreating(true);
            }}
          >
            <Plus size={12} />
          </button>
        </>
      }
    >
      {creating && (
        <NewPresetForm
          folders={folders}
          onCancel={() => setCreating(false)}
          onSave={({ name, folderId, style, crop, masks }) => {
            addPreset(name, folderId, masks, crop, style ? "style" : "tool");
            setCreating(false);
          }}
        />
      )}
      {folderNaming && (
        <input
          className="studio-metadata-input"
          aria-label={t("studio.presets.folderName")}
          placeholder={t("studio.presets.folderName")}
          autoFocus
          onKeyDown={(e) => {
            e.stopPropagation();
            if (e.key === "Enter") {
              const name = e.currentTarget.value.trim();
              if (name) addFolder(name);
              setFolderNaming(false);
            } else if (e.key === "Escape") setFolderNaming(false);
          }}
          onBlur={() => setFolderNaming(false)}
        />
      )}
      {deletingFolder && (
        <div
          className="studio-inline studio-preset-confirm"
          role="alertdialog"
          aria-label={t("studio.presets.deleteFolder")}
        >
          <span>
            {deletingFolder.size
              ? t("studio.presets.deleteFolderWithPresets", {
                  name: deletingFolder.name,
                  count: deletingFolder.size,
                })
              : t("studio.presets.deleteConfirm", { name: deletingFolder.name })}
          </span>
          <span>
            <button onClick={() => setDeletingFolder(null)}>{t("studio.presets.cancel")}</button>
            <button
              onClick={() => {
                deleteItem(deletingFolder.id);
                setDeletingFolder(null);
              }}
            >
              {t("studio.presets.delete")}
            </button>
          </span>
        </div>
      )}
      {deleting && (
        <div
          className="studio-inline studio-preset-confirm"
          role="alertdialog"
          aria-label={t("studio.presets.deletePreset")}
        >
          <span>{t("studio.presets.deleteConfirm", { name: deleting.name })}</span>
          <span>
            <button onClick={() => setDeleting(null)}>{t("studio.presets.cancel")}</button>
            <button
              onClick={() => {
                deleteItem(deleting.id);
                setDeleting(null);
              }}
            >
              {t("studio.presets.delete")}
            </button>
          </span>
        </div>
      )}
      {count > 10 && (
        <input
          className="studio-metadata-input studio-preset-search"
          aria-label={t("studio.presets.find")}
          placeholder={t("studio.presets.find")}
          value={query}
          onChange={(e) => setQuery(e.target.value)}
          onKeyDown={(e) => e.stopPropagation()}
        />
      )}
      {!isLoading && count === 0 && !creating && (
        <p className="studio-help">{t("studio.presets.empty")}</p>
      )}
      {selectionSize > 1 && count > 0 && (
        <p className="studio-metadata-note">
          {t("studio.presets.appliesToSelection", { count: selectionSize })}
        </p>
      )}
      {amount && amount.path === selectedImage?.path && (
        <label className="studio-preset-amount">
          <span>{amount.preset.name}</span>
          <input
            type="range"
            min={0}
            max={100}
            step={1}
            aria-label={t("studio.presets.amount")}
            value={amount.value}
            onChange={(e) => changeAmount(Number(e.target.value))}
            onKeyDown={(e) => e.stopPropagation()}
          />
          <output>{amount.value}%</output>
        </label>
      )}
      <div className="studio-presets" role="list">
        {groups.map((group) => {
          const key = group.folder?.id ?? "";
          const open = !group.folder || query.trim() !== "" || !collapsed.has(key);
          return (
            <div key={key} role="listitem">
              {group.folder && renaming === group.folder.id ? (
                <input
                  className="studio-metadata-input"
                  aria-label={t("studio.presets.renameAria", { name: group.folder.name })}
                  defaultValue={group.folder.name}
                  autoFocus
                  onKeyDown={(e) => {
                    e.stopPropagation();
                    if (e.key === "Enter") {
                      const name = e.currentTarget.value.trim();
                      if (name && group.folder && name !== group.folder.name) renameItem(group.folder.id, name);
                      setRenaming(null);
                    } else if (e.key === "Escape") setRenaming(null);
                  }}
                  onBlur={() => setRenaming(null)}
                />
              ) : group.folder && (
                <button
                  className={`studio-preset-folder ${open ? "is-open" : ""}`}
                  aria-expanded={open}
                  onContextMenu={(e) => group.folder && folderMenu(e, group.folder, group.presets.length)}
                  onClick={() =>
                    setCollapsed((previous) => {
                      const next = new Set(previous);
                      if (next.has(key)) next.delete(key);
                      else next.add(key);
                      return next;
                    })
                  }
                >
                  <ChevronRight size={11} />
                  {group.folder.name}
                  <small>{group.presets.length}</small>
                </button>
              )}
              {open &&
                group.presets.map((preset) =>
                  renaming === preset.id ? (
                    <input
                      key={preset.id}
                      className="studio-metadata-input"
                      aria-label={t("studio.presets.renameAria", { name: preset.name })}
                      defaultValue={preset.name}
                      autoFocus
                      onKeyDown={(e) => renameKeys(e, preset)}
                      onBlur={() => setRenaming(null)}
                    />
                  ) : (
                    <button
                      key={preset.id}
                      className={`studio-preset ${group.folder ? "is-nested" : ""} ${applied === preset.id ? "is-applied" : ""}`}
                      disabled={!panelsReady}
                      title={
                        preset.presetType === "tool"
                          ? t("studio.presets.appliesSaved")
                          : t("studio.presets.appliesAll")
                      }
                      onClick={() => apply(preset)}
                      onContextMenu={(e) => menu(e, preset)}
                      onMouseEnter={() => startPreview(preset)}
                      onMouseLeave={endPreview}
                    >
                      {preset.presetType !== "tool" && <Sparkles size={10} aria-label={t("studio.presets.style")} />}
                      <span>{preset.name}</span>
                    </button>
                  ),
                )}
            </div>
          );
        })}
      </div>
    </ToolGroup>
  );
}
