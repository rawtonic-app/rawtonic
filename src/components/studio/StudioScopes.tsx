import { memo, useEffect, useMemo, useRef } from "react";
import { useTranslation } from "react-i18next";
import { useEditorStore } from "../../store/useEditorStore";
import { useSettingsStore } from "../../store/useSettingsStore";
import type { WaveformData } from "../ui/AppProperties";

/** The scope modes, as `calculate_waveform_from_image` (scopes.rs) names them. */
export const SCOPE_MODES = ["luma", "rgb", "parade", "vectorscope"] as const;
export type ScopeMode = (typeof SCOPE_MODES)[number];

/** Size of the scope images (`SCOPE_SIZE`). */
const SIZE = 256;
/** Parade strips (`PARADE_STRIP`, `PARADE_GAP`). */
const STRIP = 82;
const GAP = 5;

export function scopeMode(value: unknown): ScopeMode {
  return SCOPE_MODES.includes(value as ScopeMode) ? (value as ScopeMode) : "luma";
}

/** The y of a code (0–255) in the scope image: row 0 is 255, centres at +0.5. */
export function waveformY(percent: number): number {
  return 255.5 - (percent / 100) * 255;
}

/** The RGBA bytes of one scope image, or null when it was not computed. */
export function scopePixels(
  waveform: WaveformData | null,
  mode: ScopeMode,
): Uint8ClampedArray<ArrayBuffer> | null {
  const encoded = waveform?.[mode];
  if (!encoded || waveform.width !== SIZE || waveform.height !== SIZE) return null;
  const binary = atob(encoded);
  if (binary.length !== SIZE * SIZE * 4) return null;
  const bytes = new Uint8ClampedArray(binary.length);
  for (let i = 0; i < binary.length; i++) bytes[i] = binary.charCodeAt(i);
  return bytes;
}

function WaveformGraticule({ parade }: { parade: boolean }) {
  return (
    <g className="studio-scope-graticule">
      {[0, 25, 50, 75, 100].map((p) => (
        <line key={p} x1="0" x2={SIZE} y1={waveformY(p)} y2={waveformY(p)} data-percent={p} />
      ))}
      {parade &&
        [STRIP + GAP / 2, 2 * STRIP + GAP * 1.5].map((x) => (
          <line key={x} className="studio-scope-divider" x1={x} x2={x} y1="0" y2={SIZE} />
        ))}
    </g>
  );
}

function VectorscopeGraticule({ waveform }: { waveform: WaveformData | null }) {
  const c = SIZE / 2;
  const angle = ((waveform?.skinLineDegrees ?? 123) * Math.PI) / 180;
  return (
    <g className="studio-scope-graticule">
      <line x1="0" x2={SIZE} y1={c} y2={c} />
      <line x1={c} x2={c} y1="0" y2={SIZE} />
      <line
        className="studio-scope-skin"
        data-degrees={waveform?.skinLineDegrees}
        x1={c}
        y1={c}
        x2={c + 124 * Math.cos(angle)}
        y2={c - 124 * Math.sin(angle)}
      />
      {(waveform?.vectorscopeTargets ?? []).map((t) => {
        const half = t.percent === 100 ? 4 : 3;
        return (
          <g key={`${t.name}${t.percent}`} className={`studio-scope-target is-${t.percent}`}>
            <rect
              data-target={`${t.name}${t.percent}`}
              x={t.x - half}
              y={t.y - half}
              width={half * 2}
              height={half * 2}
            />
            {t.percent === 100 && (
              <text
                x={c + (t.x - c) * 1.09}
                y={c + (t.y - c) * 1.09}
                textAnchor="middle"
                dominantBaseline="middle"
              >
                {t.name}
              </text>
            )}
          </g>
        );
      })}
    </g>
  );
}

/** Waveform, RGB waveform, parade and vectorscope of the shown frame, in its
 * output space (`analytics-update`). Shown with the Analysis shortcut. */
export default memo(function StudioScopes() {
  const { t } = useTranslation();
  const waveform = useEditorStore((s) => s.waveform);
  const mode = scopeMode(useEditorStore((s) => s.activeWaveformChannel));
  const setEditor = useEditorStore((s) => s.setEditor);
  const canvas = useRef<HTMLCanvasElement>(null);
  const pixels = useMemo(() => scopePixels(waveform, mode), [waveform, mode]);

  useEffect(() => {
    const context = canvas.current?.getContext("2d");
    if (!context) return;
    context.clearRect(0, 0, SIZE, SIZE);
    if (pixels) context.putImageData(new ImageData(pixels, SIZE, SIZE), 0, 0);
  }, [pixels]);

  const choose = (next: ScopeMode) => {
    setEditor({ activeWaveformChannel: next });
    const { appSettings, handleSettingsChange } = useSettingsStore.getState();
    if (appSettings) handleSettingsChange({ ...appSettings, activeWaveformChannel: next });
  };
  const vectorscope = mode === "vectorscope";
  const labels: Record<ScopeMode, [string, string]> = {
    luma: [t("studio.scopes.luma"), t("studio.scopes.lumaAria")],
    rgb: [t("studio.scopes.rgb"), t("studio.scopes.rgbAria")],
    parade: [t("studio.scopes.parade"), t("studio.scopes.paradeAria")],
    vectorscope: [t("studio.scopes.vectorscope"), t("studio.scopes.vectorscopeAria")],
  };

  return (
    <section className="studio-scopes" aria-label={t("studio.scopes.aria")}>
      <div className="studio-section-caption">
        <span>{t("studio.scopes.title")}</span>
        <span className="studio-scope-modes" role="group" aria-label={t("studio.scopes.mode")}>
          {SCOPE_MODES.map((m) => (
            <button
              key={m}
              type="button"
              className="studio-clipping-toggle"
              aria-pressed={mode === m}
              onClick={() => choose(m)}
            >
              {labels[m][0]}
            </button>
          ))}
        </span>
      </div>
      <div className={`studio-scope-plot${vectorscope ? " is-vectorscope" : ""}`}>
        <canvas ref={canvas} width={SIZE} height={SIZE} aria-hidden="true" />
        <svg
          viewBox={`0 0 ${SIZE} ${SIZE}`}
          preserveAspectRatio="none"
          role="img"
          aria-label={pixels ? labels[mode][1] : t("studio.scopes.empty")}
        >
          {vectorscope ? (
            <VectorscopeGraticule waveform={waveform} />
          ) : (
            <WaveformGraticule parade={mode === "parade"} />
          )}
        </svg>
        {!vectorscope && (
          <div className="studio-scope-scale" aria-hidden="true">
            {[100, 50, 0].map((p) => (
              <span key={p} style={{ top: `${(waveformY(p) / SIZE) * 100}%` }}>
                {p}
              </span>
            ))}
          </div>
        )}
      </div>
    </section>
  );
});

