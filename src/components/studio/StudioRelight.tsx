import { useEffect, useRef, useState } from "react";
import { useTranslation } from "react-i18next";
import { useShallow } from "zustand/react/shallow";
import {
  Plus,
  Trash2,
  Eye,
  EyeOff,
  RotateCcw,
  ChevronDown,
  Headphones,
  ImagePlus,
} from "lucide-react";
import { useEditorStore } from "../../store/useEditorStore";
import { useEditorActions } from "../../hooks/useEditorActions";
import {
  createRelightLight,
  kelvinToHex,
  MAX_RELIGHT_LIGHTS,
  matchesRelightGeometry,
  nextRelightNumber,
  RELIGHT_FIRST_LIGHT_SCENE,
  RELIGHT_PATTERNS,
  RELIGHT_PRESETS,
  RELIGHT_SCENE_DEFAULTS,
  RELIGHT_TEMPERATURES,
  relightPatternRotation,
  relightPresetLight,
  relightSource,
  type RelightLight,
  type RelightDepth,
  type RelightPreset,
} from "../../utils/relight";
import type { Adjustments } from "../../utils/adjustments";
import { openDialog } from "../../utils/fileDialog";
import { useRelightStore } from "../../store/useRelightStore";
import RelightPatternPreview from "./RelightPatternPreview";
import AdjustmentControl from "./AdjustmentControl";
import { Invokes } from "../ui/AppProperties";
import { invokeEditStep } from "../../utils/editGate";

const errorText = (e: unknown) =>
  e instanceof Error ? e.message : typeof e === "string" ? e : "";

export default function StudioRelight({
  disabled,
  onInteraction,
}: {
  disabled: boolean;
  onInteraction(active: boolean): void;
}) {
  const { t } = useTranslation();
  // Only the values the tool shows: other edits (a slider drag's commits)
  // do not re-render it.
  const adjustments = useEditorStore(
    useShallow((s) => ({
      relightLights: s.adjustments.relightLights,
      relightDepth: s.adjustments.relightDepth,
      masks: s.adjustments.masks,
      relightOriginalLight: s.adjustments.relightOriginalLight,
      relightDepthAmount: s.adjustments.relightDepthAmount,
      relightSkinReflections: s.adjustments.relightSkinReflections,
      depthValid:
        !!s.adjustments.relightDepth &&
        matchesRelightGeometry(s.adjustments, s.adjustments.relightDepth.geometry),
    })),
  );
  const selectedImage = useEditorStore((s) => s.selectedImage);
  const { setAdjustments } = useEditorActions();
  const selected = useRelightStore((s) => s.selected);
  const showHandles = useRelightStore((s) => s.showHandles);
  const solo = useRelightStore((s) => s.solo);
  const setSelected = (selected: string) =>
    useRelightStore.setState({ selected });
  const patternToggle = useRef<HTMLButtonElement>(null);
  const [patternsOpen, setPatternsOpen] = useState(false);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [importError, setImportError] = useState<string | null>(null);
  const request = useRef(0);
  useEffect(() => {
    return () => {
      request.current += 1;
      useRelightStore.setState({ solo: null });
    };
  }, []);
  const lights = adjustments.relightLights ?? [];
  const light = lights.find((l) => l.id === selected) ?? lights[0];
  const number = (l: RelightLight) => l.number ?? lights.indexOf(l) + 1;
  const depth = adjustments.relightDepth;
  const depthValid = !!depth && adjustments.depthValid;
  const masks = adjustments.masks ?? [];
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
    const asked = useEditorStore.getState().adjustments;
    const id = ++request.current;
    setBusy(true);
    setError(null);
    try {
      const result = await invokeEditStep<RelightDepth>(Invokes.GenerateRelightDepth, {
        jsAdjustments: asked,
      });
      const current = useEditorStore.getState();
      if (
        id !== request.current ||
        current.selectedImage?.path !== path ||
        !current.adjustments.relightLights?.length ||
        current.adjustments.relightDepth !== asked.relightDepth
      )
        return;
      if (!matchesRelightGeometry(current.adjustments, result.geometry)) return;
      setAdjustments((prev) => ({
        ...prev,
        relightDepth: { ...result, cacheKey: crypto.randomUUID() },
      }));
    } catch (e) {
      if (id === request.current) setError(errorText(e));
    } finally {
      if (id === request.current) setBusy(false);
    }
  };
  // The first light seeds the scene values; later ones keep them.
  const withLights = (
    prev: Adjustments,
    relightLights: RelightLight[],
  ): Adjustments => ({
    ...prev,
    relightOriginalLight:
      prev.relightOriginalLight ?? RELIGHT_FIRST_LIGHT_SCENE.originalLight,
    relightDepthAmount:
      prev.relightDepthAmount ?? RELIGHT_FIRST_LIGHT_SCENE.depthAmount,
    relightLights,
  });
  const afterAdding = (id: string) => {
    setSelected(id);
    useRelightStore.setState({ showHandles: true });
    if (!depthValid && !busy) void analyze();
  };
  const addLight = () => {
    const next = { ...createRelightLight(), number: nextRelightNumber(lights) };
    // Give new sources a free corner so every light is easy to grab.
    const position = [
      [20, 20, 135],
      [80, 20, -135],
      [20, 80, 45],
      [80, 80, -45],
    ].find(
      ([x, y]) =>
        // Presets and drags leave sources near, not on, a corner.
        !lights.some((l) => {
          const source = relightSource(l, 1, 1);
          return Math.hypot(source.x - x, source.y - y) < 15;
        }),
    );
    if (position) [next.sourceX, next.sourceY, next.rotation] = position;
    next.patternRotation = 0;
    setAdjustments((prev) =>
      withLights(prev, [
        ...(prev.relightLights ?? []).slice(0, MAX_RELIGHT_LIGHTS - 1),
        next,
      ]),
    );
    afterAdding(next.id);
  };
  const applyPreset = (preset: RelightPreset) => {
    const next = { ...relightPresetLight(preset), number: 1 };
    setAdjustments((prev) => withLights(prev, [next]));
    useRelightStore.setState({ solo: null });
    afterAdding(next.id);
  };
  const removeLight = () =>
    setAdjustments((prev) => {
      const rest = prev.relightLights?.filter((l) => l.id !== light?.id) ?? [];
      // Depth only serves lights: don't keep the bitmap in an unlit recipe.
      return rest.length
        ? { ...prev, relightLights: rest }
        : { ...prev, relightLights: rest, relightDepth: undefined };
    });
  const importPattern = async () => {
    setImportError(null);
    const path = await openDialog({
      multiple: false,
      filters: [
        {
          name: t("studio.relight.patternImages"),
          extensions: ["png", "jpg", "jpeg", "tif", "tiff", "webp"],
        },
      ],
    });
    if (typeof path !== "string") return;
    try {
      const data = await invokeEditStep<string>(Invokes.LoadRelightPattern, { path });
      update({
        pattern: "custom",
        customPattern: {
          id: crypto.randomUUID(),
          data,
          name: path.split(/[\\/]/).pop(),
        },
      });
      setPatternsOpen(false);
      patternToggle.current?.focus();
    } catch (e) {
      setImportError(errorText(e));
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
            ? relightPatternRotation(light)
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
  const patterned = light?.pattern !== "none";
  const lightName = light
    ? t("studio.relight.light", { number: number(light) })
    : "";
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
      <label className="studio-inline">
        <span>{t("studio.relight.preset")}</span>
        <select
          value=""
          disabled={disabled}
          onChange={(e) => {
            if (e.target.value)
              applyPreset(e.target.value as RelightPreset);
          }}
        >
          <option value="">{t("studio.relight.presetChoose")}</option>
          {RELIGHT_PRESETS.map((p) => (
            <option key={p} value={p}>
              {t(`studio.relight.presets.${p}`)}
            </option>
          ))}
        </select>
      </label>
      <div className="studio-relight-lights">
        {lights.map((l) => (
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
            {t("studio.relight.light", { number: number(l) })}
            {!l.enabled && <EyeOff size={12} />}
          </button>
        ))}
        <button
          type="button"
          disabled={disabled || lights.length >= MAX_RELIGHT_LIGHTS}
          onClick={addLight}
        >
          <Plus size={13} />
          {t("studio.relight.add")}
        </button>
      </div>
      {lights.length > 0 && (
        <section
          className="studio-relight-section"
          aria-label={t("studio.relight.scene")}
        >
          <div className="studio-subheading">{t("studio.relight.scene")}</div>
          <AdjustmentControl
            label={t("studio.relight.originalLight")}
            value={
              adjustments.relightOriginalLight ??
              RELIGHT_SCENE_DEFAULTS.originalLight
            }
            min={0}
            max={100}
            defaultValue={RELIGHT_SCENE_DEFAULTS.originalLight}
            disabled={disabled}
            onChange={(value) =>
              setAdjustments({ relightOriginalLight: value })
            }
            onInteraction={onInteraction}
          />
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
          {error !== null && (
            <p role="alert" className="studio-relight-help">
              {t("studio.relight.error")}
              {error && (
                <>
                  <br />
                  <span className="studio-relight-error-detail">{error}</span>
                </>
              )}
            </p>
          )}
          {depthValid && !!depth.faces?.length && (
            <p className="studio-relight-help">
              {t("studio.relight.faceDetail", { faces: depth.faces.length })}
            </p>
          )}
          {depthValid && (
            <>
              <AdjustmentControl
                label={t("studio.relight.depthAmount")}
                value={
                  adjustments.relightDepthAmount ??
                  RELIGHT_SCENE_DEFAULTS.depthAmount
                }
                min={0}
                max={100}
                defaultValue={RELIGHT_SCENE_DEFAULTS.depthAmount}
                disabled={disabled}
                onChange={(value) =>
                  setAdjustments({ relightDepthAmount: value })
                }
                onInteraction={onInteraction}
              />
              {!!depth.faces?.length && (
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
              <button
                type="button"
                className="studio-relight-analyze"
                disabled={disabled}
                onClick={() => setAdjustments({ relightDepth: undefined })}
              >
                {t("studio.relight.removeDepth")}
              </button>
            </>
          )}
        </section>
      )}
      {light && (
        <section className="studio-relight-section" aria-label={lightName}>
          <div className="studio-relight-light-header">
            <div className="studio-subheading">{lightName}</div>
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
                disabled={disabled || lights.length < 2}
                aria-label={t("studio.relight.solo")}
                title={t("studio.relight.solo")}
                aria-pressed={solo === light.id}
                onClick={() =>
                  useRelightStore.setState({
                    solo: solo === light.id ? null : light.id,
                  })
                }
              >
                <Headphones size={14} />
              </button>
              <button
                type="button"
                disabled={disabled}
                aria-label={t("studio.relight.resetLight")}
                title={t("studio.relight.resetLight")}
                onClick={() =>
                  // Settings return to a new light's; placement stays.
                  update({
                    ...createRelightLight(),
                    id: light.id,
                    number: light.number,
                    x: light.x,
                    y: light.y,
                    sourceX: light.sourceX,
                    sourceY: light.sourceY,
                    rotation: light.rotation,
                    type: light.type,
                    maskId: light.maskId,
                    temperature: undefined,
                    customPattern: undefined,
                  })
                }
              >
                <RotateCcw size={14} />
              </button>
              <button
                type="button"
                disabled={disabled}
                aria-label={t("studio.relight.remove")}
                title={t("studio.relight.remove")}
                onClick={removeLight}
              >
                <Trash2 size={14} />
              </button>
            </div>
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
            <span>{t("studio.relight.maskLayer")}</span>
            <select
              value={light.maskId ?? ""}
              disabled={disabled}
              onChange={(e) =>
                update({ maskId: e.target.value || undefined })
              }
            >
              <option value="">{t("studio.relight.wholePhoto")}</option>
              {masks.map((m) => (
                <option key={m.id} value={m.id}>
                  {m.name || m.id}
                </option>
              ))}
            </select>
          </label>
          {light.maskId &&
            masks.some(
              (m) =>
                m.id === light.maskId && (!m.visible || !m.subMasks?.length),
            ) && (
              <p className="studio-relight-help">
                {t("studio.relight.maskHidden")}
              </p>
            )}
          {slider("intensity")}
          <div
            className="studio-relight-temperatures"
            role="group"
            aria-label={t("studio.relight.temperature")}
          >
            {RELIGHT_TEMPERATURES.map((k) => (
              <button
                key={k}
                type="button"
                disabled={disabled}
                aria-label={`${k} K`}
                title={`${k} K`}
                aria-pressed={light.temperature === k}
                style={{ backgroundColor: kelvinToHex(k) }}
                onClick={() =>
                  update({ temperature: k, color: kelvinToHex(k) })
                }
              />
            ))}
            <input
              type="color"
              aria-label={t("studio.relight.color")}
              value={light.color}
              disabled={disabled}
              onChange={(e) =>
                update({ color: e.target.value, temperature: undefined })
              }
            />
          </div>
          <AdjustmentControl
            label={t("studio.relight.temperature")}
            value={light.temperature ?? 5500}
            min={1900}
            max={10000}
            step={100}
            unit="K"
            defaultValue={5500}
            disabled={disabled}
            onChange={(k) => update({ temperature: k, color: kelvinToHex(k) })}
            onInteraction={onInteraction}
          />
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
            <RelightPatternPreview
              pattern={light.pattern}
              image={light.customPattern?.data}
            />
            <span>
              {light.pattern === "custom"
                ? (light.customPattern?.name ??
                  t("studio.relight.patterns.custom"))
                : t(`studio.relight.patterns.${light.pattern}`)}
            </span>
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
              {light.customPattern && (
                <button
                  type="button"
                  disabled={disabled}
                  aria-label={t("studio.relight.patterns.custom")}
                  aria-pressed={light.pattern === "custom"}
                  onClick={() => {
                    update({ pattern: "custom" });
                    setPatternsOpen(false);
                    patternToggle.current?.focus();
                  }}
                >
                  <RelightPatternPreview
                    pattern="custom"
                    image={light.customPattern.data}
                  />
                  <span>{t("studio.relight.patterns.custom")}</span>
                </button>
              )}
              <button
                type="button"
                disabled={disabled}
                aria-label={t("studio.relight.importPattern")}
                onClick={() => void importPattern()}
              >
                <ImagePlus size={20} />
                <span>{t("studio.relight.importPattern")}</span>
              </button>
            </div>
          )}
          {importError !== null && (
            <p role="alert" className="studio-relight-help">
              {t("studio.relight.importError")}
              {importError && (
                <>
                  <br />
                  <span className="studio-relight-error-detail">
                    {importError}
                  </span>
                </>
              )}
            </p>
          )}
          {patterned && (
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
            {/* Only controls that change this light's result are shown. */}
            {patterned && slider("x", -100, 200)}
            {patterned && slider("y", -100, 200)}
            {light.type === "spot" && slider("size", 5, 200)}
            {(light.type === "sun" || (patterned && depthValid)) &&
              slider("rotation", -180, 180)}
            {depthValid &&
              (light.type === "sun"
                ? slider("elevation", 5, 90)
                : slider("distance", 10, 300))}
            {(patterned || (depthValid && !!depth.faces?.length)) &&
              slider("shadows")}
          </details>
        </section>
      )}
    </div>
  );
}
