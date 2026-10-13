import { useEffect, useRef, useState } from "react";
import { useTranslation } from "react-i18next";
import { invoke } from "@tauri-apps/api/core";
import {
  Plus,
  Trash2,
  Eye,
  EyeOff,
  RotateCcw,
  ChevronDown,
} from "lucide-react";
import { useEditorStore } from "../../store/useEditorStore";
import { useEditorActions } from "../../hooks/useEditorActions";
import {
  createRelightLight,
  MAX_RELIGHT_LIGHTS,
  matchesRelightGeometry,
  RELIGHT_PATTERNS,
  type RelightLight,
  type RelightDepth,
} from "../../utils/relight";
import { useRelightStore } from "../../store/useRelightStore";
import RelightPatternPreview from "./RelightPatternPreview";
import AdjustmentControl from "./AdjustmentControl";
import { Invokes } from "../ui/AppProperties";

export default function StudioRelight({
  disabled,
  onInteraction,
}: {
  disabled: boolean;
  onInteraction(active: boolean): void;
}) {
  const { t } = useTranslation();
  const adjustments = useEditorStore((s) => s.adjustments);
  const selectedImage = useEditorStore((s) => s.selectedImage);
  const { setAdjustments } = useEditorActions();
  const selected = useRelightStore((s) => s.selected);
  const showHandles = useRelightStore((s) => s.showHandles);
  const setSelected = (selected: string) =>
    useRelightStore.setState({ selected });
  const patternToggle = useRef<HTMLButtonElement>(null);
  const [patternsOpen, setPatternsOpen] = useState(false);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState(false);
  const request = useRef(0);
  useEffect(() => {
    return () => {
      request.current += 1;
    };
  }, []);
  const lights = adjustments.relightLights ?? [];
  const light = lights.find((l) => l.id === selected) ?? lights[0];
  const depth = adjustments.relightDepth;
  const depthValid =
    !!depth && matchesRelightGeometry(adjustments, depth.geometry);
  const update = (patch: Partial<RelightLight>) =>
    setAdjustments((prev) => ({
      ...prev,
      relightLights: (prev.relightLights ?? []).map((l) =>
        l.id === light?.id ? { ...l, ...patch } : l,
      ),
    }));
  const analyze = async () => {
    if (!selectedImage) return;
    const path = selectedImage.path;
    const id = ++request.current;
    setBusy(true);
    setError(false);
    try {
      const result = await invoke<RelightDepth>(Invokes.GenerateRelightDepth, {
        jsAdjustments: adjustments,
      });
      const current = useEditorStore.getState();
      if (
        id !== request.current ||
        current.selectedImage?.path !== path ||
        !current.adjustments.relightLights?.length ||
        current.adjustments.relightDepth !== adjustments.relightDepth
      )
        return;
      if (!matchesRelightGeometry(current.adjustments, result.geometry)) return;
      setAdjustments((prev) => ({ ...prev, relightDepth: result }));
    } catch {
      if (id === request.current) setError(true);
    } finally {
      if (id === request.current) setBusy(false);
    }
  };
  const slider = (
    key:
      | "intensity"
      | "x"
      | "y"
      | "size"
      | "rotation"
      | "scale"
      | "softness"
      | "diffusion"
      | "shadows"
      | "elevation"
      | "distance"
      | "patternRotation",
    min = 0,
    max = 100,
  ) =>
    light && (
      <AdjustmentControl
        key={key}
        label={t(`studio.relight.${key}`)}
        value={
          key === "patternRotation"
            ? (light.patternRotation ?? light.rotation)
            : key === "distance"
              ? (light.distance ?? 60)
              : light[key]
        }
        min={min}
        max={max}
        disabled={disabled}
        onChange={(value) => update({ [key]: value })}
        onInteraction={onInteraction}
      />
    );
  return (
    <div className="studio-relight">
      <label className="studio-relight-edit">
        <input
          type="checkbox"
          checked={showHandles}
          disabled={disabled}
          onChange={(e) =>
            useRelightStore.setState({ showHandles: e.target.checked })
          }
        />
        <span>{t("studio.relight.showGuides")}</span>
      </label>
      <p className="studio-relight-help">{t("studio.relight.dragHint")}</p>
      <div className="studio-relight-lights">
        {lights.map((l, i) => (
          <button
            key={l.id}
            type="button"
            disabled={disabled}
            aria-pressed={light?.id === l.id}
            onClick={() => setSelected(l.id)}
          >
            <span
              className="studio-relight-swatch"
              style={{ backgroundColor: l.color }}
            />
            {t("studio.relight.light", { number: i + 1 })}
            {!l.enabled && <EyeOff size={12} />}
          </button>
        ))}
        <button
          type="button"
          disabled={disabled || lights.length >= MAX_RELIGHT_LIGHTS}
          onClick={() => {
            const next = createRelightLight();
            // Give new sources a free corner so every light is easy to grab.
            const position = [
              [20, 20, 135],
              [80, 20, -135],
              [20, 80, 45],
              [80, 80, -45],
            ].find(
              ([x, y]) =>
                !lights.some(
                  (l) =>
                    Math.abs((l.sourceX ?? l.x) - x) < 5 &&
                    Math.abs((l.sourceY ?? l.y) - y) < 5,
                ),
            );
            if (position)
              [next.sourceX, next.sourceY, next.rotation] = position;
            setAdjustments((prev) => ({
              ...prev,
              relightOriginalLight: prev.relightOriginalLight ?? 60,
              relightDepthAmount: prev.relightDepthAmount ?? 100,
              relightLights: [
                ...(prev.relightLights ?? []).slice(0, MAX_RELIGHT_LIGHTS - 1),
                next,
              ],
            }));
            setSelected(next.id);
            useRelightStore.setState({ showHandles: true });
            if (!depthValid && !busy) void analyze();
          }}
        >
          <Plus size={13} />
          {t("studio.relight.add")}
        </button>
      </div>
      {light && (
        <>
          <div className="studio-relight-actions">
            <button
              type="button"
              disabled={disabled}
              aria-label={t("studio.relight.enabled")}
              aria-pressed={light.enabled}
              onClick={() => update({ enabled: !light.enabled })}
            >
              {light.enabled ? <Eye size={14} /> : <EyeOff size={14} />}
            </button>
            <button
              type="button"
              disabled={disabled}
              aria-label={t("studio.relight.resetLight")}
              onClick={() => update({ ...createRelightLight(), id: light.id })}
            >
              <RotateCcw size={14} />
            </button>
            <button
              type="button"
              disabled={disabled}
              aria-label={t("studio.relight.remove")}
              onClick={() =>
                setAdjustments((prev) => ({
                  ...prev,
                  relightLights: prev.relightLights?.filter(
                    (l) => l.id !== light.id,
                  ),
                }))
              }
            >
              <Trash2 size={14} />
            </button>
          </div>
          <label className="studio-inline">
            <span>{t("studio.relight.type")}</span>
            <select
              value={light.type}
              disabled={disabled}
              onChange={(e) =>
                update({ type: e.target.value as RelightLight["type"] })
              }
            >
              <option value="sun">{t("studio.relight.sun")}</option>
              <option value="spot">{t("studio.relight.spot")}</option>
            </select>
          </label>
          <label className="studio-inline">
            <span>{t("studio.relight.color")}</span>
            <input
              type="color"
              value={light.color}
              disabled={disabled}
              onChange={(e) => update({ color: e.target.value })}
            />
          </label>
          {slider("intensity")}
          {slider("softness")}
          {slider("diffusion")}
          <button
            ref={patternToggle}
            type="button"
            className="studio-relight-pattern-toggle"
            disabled={disabled}
            aria-expanded={patternsOpen}
            aria-label={t("studio.relight.pattern")}
            onClick={() => setPatternsOpen(!patternsOpen)}
          >
            <RelightPatternPreview pattern={light.pattern} />
            <span>{t(`studio.relight.patterns.${light.pattern}`)}</span>
            <ChevronDown size={14} />
          </button>
          {patternsOpen && (
            <div
              className="studio-relight-patterns"
              role="group"
              aria-label={t("studio.relight.pattern")}
              onKeyDown={(e) => {
                if (e.key === "Escape") {
                  e.stopPropagation();
                  setPatternsOpen(false);
                  patternToggle.current?.focus();
                }
              }}
            >
              {RELIGHT_PATTERNS.map((p) => (
                <button
                  key={p}
                  type="button"
                  disabled={disabled}
                  aria-label={t(`studio.relight.patterns.${p}`)}
                  aria-pressed={light.pattern === p}
                  onClick={() => {
                    update({ pattern: p });
                    setPatternsOpen(false);
                    patternToggle.current?.focus();
                  }}
                >
                  <RelightPatternPreview pattern={p} />
                  <span>{t(`studio.relight.patterns.${p}`)}</span>
                </button>
              ))}
            </div>
          )}
          {light.pattern !== "none" && (
            <>
              {slider("scale", 5, 300)}
              {slider("patternRotation", -180, 180)}
              <label className="studio-inline">
                <span>{t("studio.relight.invert")}</span>
                <input
                  type="checkbox"
                  checked={light.invert}
                  disabled={disabled}
                  onChange={(e) => update({ invert: e.target.checked })}
                />
              </label>
            </>
          )}
          <details className="studio-relight-advanced">
            <summary>{t("studio.relight.advanced")}</summary>
            {slider("x", -100, 200)}
            {slider("y", -100, 200)}
            {light.type === "spot" && slider("size", 5, 200)}
            {slider("rotation", -180, 180)}
            {depthValid && !!depth.faces?.length && (
              <AdjustmentControl
                label={t("studio.relight.skinReflections")}
                value={
                  adjustments.relightSkinReflections ??
                  ((depth.version ?? 0) >= 3 ? 25 : 0)
                }
                min={0}
                max={100}
                disabled={disabled}
                onChange={(value) =>
                  setAdjustments({ relightSkinReflections: value })
                }
                onInteraction={onInteraction}
              />
            )}
            {depthValid &&
              (light.type === "sun"
                ? slider("elevation", 5, 90)
                : slider("distance", 10, 300))}
            {(light.pattern !== "none" ||
              (depthValid && !!depth.faces?.length)) &&
              slider("shadows")}
          </details>
          <AdjustmentControl
            label={t("studio.relight.originalLight")}
            value={adjustments.relightOriginalLight ?? 100}
            min={0}
            max={100}
            disabled={disabled}
            onChange={(value) =>
              setAdjustments({ relightOriginalLight: value })
            }
            onInteraction={onInteraction}
          />
          <div className="studio-subheading">{t("studio.relight.depth")}</div>
          <p className="studio-relight-help" role="status">
            {busy
              ? t("studio.relight.analyzing")
              : depthValid
                ? t("studio.relight.depthReady")
                : depth
                  ? t("studio.relight.depthStale")
                  : t("studio.relight.depthHint")}
          </p>
          <button
            className="studio-relight-analyze"
            type="button"
            disabled={disabled || busy}
            onClick={() => void analyze()}
          >
            {t(
              depthValid
                ? "studio.relight.reanalyze"
                : "studio.relight.analyze",
            )}
          </button>
          {error && <p role="alert">{t("studio.relight.error")}</p>}
          {depthValid && !!depth.faces?.length && (
            <p className="studio-relight-help">
              {t("studio.relight.faceDetail", { faces: depth.faces.length })}
            </p>
          )}
          {depthValid && (
            <>
              <AdjustmentControl
                label={t("studio.relight.depthAmount")}
                value={adjustments.relightDepthAmount ?? 60}
                min={0}
                max={100}
                disabled={disabled}
                onChange={(value) =>
                  setAdjustments({ relightDepthAmount: value })
                }
                onInteraction={onInteraction}
              />
              <button
                type="button"
                disabled={disabled}
                onClick={() => setAdjustments({ relightDepth: undefined })}
              >
                {t("studio.relight.removeDepth")}
              </button>
            </>
          )}
        </>
      )}
    </div>
  );
}
