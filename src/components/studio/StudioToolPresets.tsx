import { useEffect, useLayoutEffect, useMemo, useRef, useState } from "react";
import { createPortal } from "react-dom";
import { useTranslation } from "react-i18next";
import { Bookmark, Trash2 } from "lucide-react";
import { panelsShowReady, useEditorStore } from "../../store/useEditorStore";
import { useSettingsStore } from "../../store/useSettingsStore";
import { useEditorActions } from "../../hooks/useEditorActions";
import { usePresets } from "../../hooks/usePresets";
import { loadBuiltinPresets, presetName, useBuiltinPresets } from "../../utils/builtinPresets";
import type { Preset } from "../ui/AppProperties";
import { usePopoverFocus } from "./usePopoverFocus";
import type { ToolParams } from "./StudioToolCopy";
import { presetFitsTool, presetPreview, toolPresetToSave } from "./toolPresets";
import { noteTool } from "../contribute/usageStats";

const currentAdjustments = () => useEditorStore.getState().adjustments;

/** Capture One's per-tool presets: the header button opens the tool's
 * built-in presets and the photographer's own (saved tool presets whose
 * settings all belong to this tool); hovering one shows it on the photo, a
 * click applies it to the open photo as one step. */
export default function StudioToolPresets({
  tool,
  label = tool,
  params,
}: {
  /** Stable tool id (`ToolGroup`'s `title`). */
  tool: string;
  /** The tool's shown (translated) name; also the folder its presets go in. */
  label?: string;
  params: ToolParams;
}) {
  const { t } = useTranslation();
  const [anchor, setAnchor] = useState<{ x: number; y: number } | null>(null);
  const panelsReady = useEditorStore(panelsShowReady);
  return (
    <>
      <button
        type="button"
        className="studio-tool-presets-button"
        title={t("studio.tools.presets.buttonTitle", { tool: label })}
        aria-label={t("studio.tools.presets.aria", { tool: label })}
        aria-haspopup="dialog"
        disabled={!panelsReady}
        onClick={(e) => {
          e.preventDefault();
          if (!useEditorStore.getState().selectedImage?.isReady) return;
          e.currentTarget.focus({ preventScroll: true });
          const rect = e.currentTarget.getBoundingClientRect();
          setAnchor(e.detail === 0 ? { x: rect.left, y: rect.bottom } : { x: e.clientX, y: e.clientY });
        }}
      >
        <Bookmark size={12} />
      </button>
      {anchor && (
        <ToolPresetsPopover
          tool={tool}
          label={label}
          params={params}
          anchor={anchor}
          onClose={() => setAnchor(null)}
        />
      )}
    </>
  );
}

function ToolPresetsPopover({
  tool,
  label,
  params,
  anchor,
  onClose,
}: {
  tool: string;
  label: string;
  params: ToolParams;
  anchor: { x: number; y: number };
  onClose(): void;
}) {
  const { t } = useTranslation();
  const ref = useRef<HTMLDivElement>(null);
  usePopoverFocus(ref);
  const selectedImage = useEditorStore((s) => s.selectedImage);
  const { applyAdjustmentsToPaths } = useEditorActions();
  const { presets, addToolPreset, deleteItem } = usePresets(currentAdjustments, Boolean(selectedImage?.isRaw));
  const showBuiltins = useSettingsStore((s) => s.appSettings?.showBuiltinPresets !== false);
  const builtins = useBuiltinPresets((s) => s.presets);
  useEffect(() => {
    void loadBuiltinPresets();
  }, []);
  const keys = useMemo(() => params.map(([key]) => key as string), [params]);
  // Built-in styles stay in the Styles panel, even one that sets only this tool.
  const ownBuiltins = showBuiltins
    ? builtins.filter((preset) => preset.presetType === "tool" && presetFitsTool(preset, keys))
    : [];
  const own = presets
    .flatMap((item) => (item.preset ? [item.preset] : item.folder?.children ?? []))
    .filter((preset) => preset.presetType === "tool" && presetFitsTool(preset, keys));
  const [naming, setNaming] = useState(false);
  const [deleting, setDeleting] = useState<Preset | null>(null);

  // The photo with the preset while the pointer rests on it (the renderer
  // draws the override; nothing is applied).
  const previewing = useRef(false);
  const endPreview = () => {
    if (!previewing.current) return;
    previewing.current = false;
    useEditorStore.getState().setEditor({ previewOverride: null });
  };
  const preview = (preset: Preset) => {
    const { selectedImage: image, adjustments, setEditor } = useEditorStore.getState();
    if (!image?.isReady) return;
    const previewed = presetPreview(adjustments, preset.adjustments, Boolean(image.isRaw));
    if (!previewed) return endPreview();
    previewing.current = true;
    setEditor({ previewOverride: previewed });
  };
  useEffect(() => endPreview, []);

  const apply = (preset: Preset) => {
    endPreview();
    const image = useEditorStore.getState().selectedImage;
    if (!image?.isReady) return;
    applyAdjustmentsToPaths(structuredClone(preset.adjustments), [image.path]);
    noteTool("presets");
    onClose();
  };
  const saveCurrent = (name: string) => {
    addToolPreset(name, toolPresetToSave(currentAdjustments(), keys), label, keys);
    setNaming(false);
  };

  // Inside the window, also when it grows (presets read, the name form).
  const [position, setPosition] = useState({ left: anchor.x, top: anchor.y });
  useLayoutEffect(() => {
    const panel = ref.current;
    if (!panel) return;
    const place = () => {
      const box = panel.getBoundingClientRect();
      const margin = 8;
      const left = Math.max(margin, Math.min(anchor.x, window.innerWidth - box.width - margin));
      const top = Math.max(margin, Math.min(anchor.y, window.innerHeight - box.height - margin));
      setPosition((p) => (p.left === left && p.top === top ? p : { left, top }));
    };
    place();
    if (typeof ResizeObserver === "undefined") return;
    const observer = new ResizeObserver(place);
    observer.observe(panel);
    return () => observer.disconnect();
  }, [anchor]);
  useEffect(() => {
    const onPointerDown = (e: PointerEvent) => {
      if (!ref.current?.contains(e.target as Node)) onClose();
    };
    const onKeyDown = (e: KeyboardEvent) => {
      if (e.key !== "Escape") return;
      e.preventDefault();
      e.stopImmediatePropagation();
      onClose();
    };
    document.addEventListener("pointerdown", onPointerDown, true);
    window.addEventListener("keydown", onKeyDown, true);
    return () => {
      document.removeEventListener("pointerdown", onPointerDown, true);
      window.removeEventListener("keydown", onKeyDown, true);
    };
  }, [onClose]);

  const item = (preset: Preset) => (
    <button
      key={preset.id}
      type="button"
      className="studio-preset"
      onClick={() => apply(preset)}
      onMouseEnter={() => preview(preset)}
      onMouseLeave={endPreview}
      onFocus={() => preview(preset)}
      onBlur={endPreview}
    >
      <span>{presetName(t, preset)}</span>
    </button>
  );

  return createPortal(
    <div
      ref={ref}
      tabIndex={-1}
      className="studio-tool-popover studio-tool-presets"
      role="dialog"
      aria-label={t("studio.tools.presets.aria", { tool: label })}
      data-tool={tool}
      style={{ left: position.left, top: position.top }}
    >
      <div className="studio-tool-popover-title">{t("studio.tools.presets.title", { tool: label })}</div>
      {ownBuiltins.length > 0 && (
        <div role="group" aria-label={t("studio.presets.builtinFolder")}>
          <div className="studio-subheading">{t("studio.presets.builtinFolder")}</div>
          {ownBuiltins.map(item)}
        </div>
      )}
      <div role="group" aria-label={t("studio.tools.presets.mine")}>
        <div className="studio-subheading">{t("studio.tools.presets.mine")}</div>
        {own.length === 0 && <p className="studio-help">{t("studio.tools.presets.none")}</p>}
        {own.map((preset) =>
          deleting?.id === preset.id ? (
            <div
              key={preset.id}
              className="studio-inline studio-preset-confirm"
              role="alertdialog"
              aria-label={t("studio.presets.deletePreset")}
            >
              <span>{t("studio.presets.deleteConfirm", { name: preset.name })}</span>
              <span>
                <button type="button" onClick={() => setDeleting(null)}>
                  {t("studio.presets.cancel")}
                </button>
                <button
                  type="button"
                  onClick={() => {
                    deleteItem(preset.id);
                    setDeleting(null);
                  }}
                >
                  {t("studio.presets.delete")}
                </button>
              </span>
            </div>
          ) : (
            <div key={preset.id} className="studio-tool-preset-row">
              {item(preset)}
              <button
                type="button"
                className="studio-tool-preset-delete"
                title={t("studio.tools.presets.delete", { name: preset.name })}
                aria-label={t("studio.tools.presets.delete", { name: preset.name })}
                onClick={() => setDeleting(preset)}
              >
                <Trash2 size={11} />
              </button>
            </div>
          ),
        )}
      </div>
      {naming ? (
        <form
          className="studio-preset-form"
          aria-label={t("studio.tools.presets.save")}
          onSubmit={(e) => {
            e.preventDefault();
            const name = new FormData(e.currentTarget).get("name")?.toString().trim();
            if (name) saveCurrent(name);
          }}
          onKeyDown={(e) => {
            e.stopPropagation();
            if (e.key === "Escape") {
              e.preventDefault();
              setNaming(false);
            }
          }}
        >
          <input
            name="name"
            className="studio-metadata-input"
            aria-label={t("studio.presets.form.nameAria")}
            placeholder={t("studio.presets.form.namePlaceholder")}
            autoFocus
          />
          <div className="studio-preset-form-actions">
            <button type="button" onClick={() => setNaming(false)}>
              {t("studio.presets.cancel")}
            </button>
            <button type="submit">{t("studio.presets.form.save")}</button>
          </div>
        </form>
      ) : (
        <button type="button" className="studio-text-button" onClick={() => setNaming(true)}>
          {t("studio.tools.presets.save")}
        </button>
      )}
    </div>,
    document.querySelector(".studio-workspace") ?? document.body,
  );
}
