import { useEffect, useLayoutEffect, useRef, useState } from "react";
import { createPortal } from "react-dom";
import { useTranslation } from "react-i18next";
import { ArrowLeftRight } from "lucide-react";
import { panelsShowReady, useEditorStore } from "../../store/useEditorStore";
import { useLibraryStore } from "../../store/useLibraryStore";
import { useEditorActions } from "../../hooks/useEditorActions";
import { withSharpenVersion, type Adjustments } from "../../utils/adjustments";

/** A tool's parameters: recipe key and (translated) label. */
export type ToolParams = [keyof Adjustments & string, string][];

const fileName = (path: string) => path.split(/[\\/]/).pop() ?? path;

/** Recipe keys without a control of their own that travel with a copied
 * parameter, as in a full copy: the colour noise amounts mean what the
 * recipe's colour noise filter makes of them, Shadows what its tone-slider
 * response makes of it. */
const COPIED_ALONG: Partial<Record<string, string[]>> = {
  colorNoiseReduction: ["colorNoiseVersion"],
  colorNoiseDetail: ["colorNoiseVersion"],
  colorNoiseSmoothness: ["colorNoiseVersion"],
  // Shadows means what the recipe's tone-slider response makes of it.
  shadows: ["toneVersion"],
  // Defringe means what the recipe's fringe detector makes of it.
  defringePurpleAmount: ["defringeVersion"],
  defringeGreenAmount: ["defringeVersion"],
  // The sharpening means what the recipe's sharpening response makes of it.
  sharpness: ["sharpenVersion"],
  sharpnessThreshold: ["sharpenVersion"],
  sharpenRadius: ["sharpenVersion"],
  sharpenDetail: ["sharpenVersion"],
  sharpenMasking: ["sharpenVersion"],
  sharpnessFalloff: ["sharpenVersion"],
  // AI Denoise means what the recipe's AI Denoise version makes of it.
  aiDenoise: ["aiDenoiseVersion"],
};

function pick(values: Adjustments, keys: string[]): Partial<Adjustments> {
  const all = new Set(keys.flatMap((key) => [key, ...(COPIED_ALONG[key] ?? [])]));
  const picked = Object.fromEntries(
    [...all].map((key) => [key, structuredClone(values[key as keyof Adjustments])]),
  ) as Partial<Adjustments>;
  // A missing response key would vanish on the way: say it explicitly.
  return withSharpenVersion(picked, values);
}

/** Capture One's copy/apply for one tool: a press opens a list of the tool's
 * parameters to copy from the open photo or apply to the other selected
 * photos; Option-press copies all at once, Shift-press copies and applies.
 * Values copied from another photo can be applied to this one. */
export default function StudioToolCopy({
  tool,
  label = tool,
  params,
}: {
  /** Stable tool id: the copied values are offered to the same tool only. */
  tool: string;
  /** The tool's shown (translated) name. */
  label?: string;
  params: ToolParams;
}) {
  const { t } = useTranslation();
  const [anchor, setAnchor] = useState<{ x: number; y: number } | null>(null);
  const selectedImage = useEditorStore((s) => s.selectedImage);
  const clipboard = useEditorStore((s) => s.toolClipboard);
  const panelsReady = useEditorStore(panelsShowReady);
  const { applyAdjustmentsToPaths } = useEditorActions();
  const keys = params.map(([key]) => key);

  const others = () => {
    const current = useEditorStore.getState().selectedImage?.path;
    return useLibraryStore
      .getState()
      .multiSelectedPaths.filter((path) => path !== current);
  };
  const copy = (chosen: string[]) => {
    const { adjustments, selectedImage: image, setEditor } =
      useEditorStore.getState();
    if (!image) return null;
    const values = pick(adjustments, chosen);
    setEditor({ toolClipboard: { tool, source: image.path, values } });
    return values;
  };
  const applyToOthers = (values: Partial<Adjustments>) => {
    const targets = others();
    if (targets.length > 0) applyAdjustmentsToPaths(values, targets);
  };

  return (
    <>
      <button
        type="button"
        title={t("studio.tools.copy.buttonTitle", { tool: label })}
        aria-label={t("studio.tools.copy.aria", { tool: label })}
        disabled={!panelsReady}
        onClick={(e) => {
          e.preventDefault();
          if (!useEditorStore.getState().selectedImage?.isReady) return;
          if (e.altKey) {
            copy(keys);
          } else if (e.shiftKey) {
            const values = copy(keys);
            if (values) applyToOthers(values);
          } else {
            setAnchor({ x: e.clientX, y: e.clientY });
          }
        }}
      >
        <ArrowLeftRight size={12} />
      </button>
      {anchor && selectedImage && (
        <CopyPopover
          tool={label}
          params={params}
          anchor={anchor}
          source={fileName(selectedImage.path)}
          otherCount={others().length}
          pasteFrom={
            clipboard &&
            clipboard.tool === tool &&
            clipboard.source !== selectedImage.path
              ? clipboard
              : null
          }
          onClose={() => setAnchor(null)}
          onCopy={(chosen) => copy(chosen)}
          onApply={(chosen) => {
            const values = copy(chosen);
            if (values) applyToOthers(values);
          }}
          onPaste={(values) =>
            applyAdjustmentsToPaths(values, [selectedImage.path])
          }
        />
      )}
    </>
  );
}

function CopyPopover({
  tool,
  params,
  anchor,
  source,
  otherCount,
  pasteFrom,
  onClose,
  onCopy,
  onApply,
  onPaste,
}: {
  tool: string;
  params: ToolParams;
  anchor: { x: number; y: number };
  source: string;
  otherCount: number;
  pasteFrom: { source: string; values: Partial<Adjustments> } | null;
  onClose(): void;
  onCopy(keys: string[]): void;
  onApply(keys: string[]): void;
  onPaste(values: Partial<Adjustments>): void;
}) {
  const { t } = useTranslation();
  const ref = useRef<HTMLDivElement>(null);
  const [chosen, setChosen] = useState(() => new Set(params.map(([k]) => k)));
  const [position, setPosition] = useState({ left: anchor.x, top: anchor.y });
  useLayoutEffect(() => {
    const box = ref.current?.getBoundingClientRect();
    if (!box) return;
    const margin = 8;
    setPosition({
      left: Math.max(margin, Math.min(anchor.x, window.innerWidth - box.width - margin)),
      top: Math.max(margin, Math.min(anchor.y, window.innerHeight - box.height - margin)),
    });
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
  const selected = params.map(([k]) => k).filter((k) => chosen.has(k));
  return createPortal(
    <div
      ref={ref}
      className="studio-tool-popover studio-copy-popover"
      role="dialog"
      aria-label={t("studio.tools.copy.aria", { tool })}
      style={{ left: position.left, top: position.top }}
    >
      <div className="studio-tool-popover-title">
        {tool}
        <span>{t("studio.tools.copy.from", { source })}</span>
      </div>
      {params.map(([key, label]) => (
        <label key={key} className="studio-inline studio-check">
          <input
            type="checkbox"
            checked={chosen.has(key)}
            onChange={(e) => {
              const next = new Set(chosen);
              if (e.target.checked) next.add(key);
              else next.delete(key);
              setChosen(next);
            }}
          />
          {label}
        </label>
      ))}
      <div className="studio-inline">
        <button onClick={onClose}>{t("studio.tools.copy.cancel")}</button>
        <button
          disabled={selected.length === 0}
          onClick={() => {
            onCopy(selected);
            onClose();
          }}
        >
          {t("studio.tools.copy.copy")}
        </button>
        <button
          disabled={selected.length === 0 || otherCount === 0}
          title={
            otherCount === 0
              ? t("studio.tools.copy.noOthers")
              : t("studio.tools.copy.applyTo", { count: otherCount })
          }
          onClick={() => {
            onApply(selected);
            onClose();
          }}
        >
          {otherCount > 0
            ? t("studio.tools.copy.applyCount", { n: otherCount })
            : t("studio.tools.copy.apply")}
        </button>
      </div>
      {pasteFrom && (
        <div className="studio-inline">
          <span>
            {t("studio.tools.copy.copiedFrom", {
              source: fileName(pasteFrom.source),
            })}
          </span>
          <button
            onClick={() => {
              onPaste(pasteFrom.values);
              onClose();
            }}
          >
            {t("studio.tools.copy.applyHere")}
          </button>
        </div>
      )}
    </div>,
    document.querySelector(".studio-workspace") ?? document.body,
  );
}
