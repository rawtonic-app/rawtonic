import { useEffect, useId, useRef, type KeyboardEvent, type PointerEvent } from "react";
import { useTranslation } from "react-i18next";
import { useEditorStore } from "../../store/useEditorStore";
import {
  debouncedSetHistory,
  updateAdjustments,
} from "../../hooks/useEditorActions";
import {
  relightBallPatch,
  relightBallPoint,
  type RelightLight,
  type RelightPoint,
} from "../../utils/relight";

const SIZE = 112;
const R = SIZE / 2 - 6;

/**
 * The light-direction ball: a lit sphere seen from the camera. Dragging (or
 * the arrow keys) moves where the light comes from; the centre is light from
 * the camera, the rim light grazing the photo. One history step per drag,
 * Esc cancels it.
 */
export default function RelightDirectionBall({
  light,
  disabled,
  reason,
}: {
  light: RelightLight;
  disabled: boolean;
  /** Why the ball cannot act now; shown under it and greys it out. */
  reason?: string | null;
}) {
  const { t } = useTranslation();
  const gradient = useId();
  const reasonId = useId();
  const drag = useRef<{ before: RelightLight; path: string; moved: boolean } | null>(null);
  const inactive = disabled || !!reason;
  const patch = (values: Partial<RelightLight>) =>
    updateAdjustments((prev) => ({
      ...prev,
      relightLights: prev.relightLights?.map((l) =>
        l.id === light.id ? { ...l, ...values } : l,
      ),
    }));
  const pointAt = (e: PointerEvent): RelightPoint => {
    const box = e.currentTarget.getBoundingClientRect();
    const scale = box.width / SIZE || 1;
    return {
      x: (e.clientX - box.left - box.width / 2) / (R * scale),
      y: (e.clientY - box.top - box.height / 2) / (R * scale),
    };
  };
  const finish = (cancel: boolean) => {
    const d = drag.current;
    if (!d) return;
    drag.current = null;
    const state = useEditorStore.getState();
    if (state.selectedImage?.path === d.path) {
      if (cancel && d.moved)
        updateAdjustments((prev) => ({
          ...prev,
          relightLights: prev.relightLights?.map((l) =>
            l.id === d.before.id ? d.before : l,
          ),
        }));
      debouncedSetHistory.cancel();
      if (!cancel && d.moved) state.pushHistory(useEditorStore.getState().adjustments);
    }
    useEditorStore.getState().setEditor({ isSliderDragging: false });
  };
  // A drag cut short by closing the tool keeps what it did, as one step.
  useEffect(() => () => finish(false), []); // eslint-disable-line react-hooks/exhaustive-deps
  const onPointerDown = (e: PointerEvent<HTMLDivElement>) => {
    const image = useEditorStore.getState().selectedImage;
    if (inactive || e.button !== 0 || !image?.isReady) return;
    e.preventDefault();
    e.currentTarget.focus({ preventScroll: true });
    debouncedSetHistory.flush();
    drag.current = { before: light, path: image.path, moved: false };
    try {
      e.currentTarget.setPointerCapture(e.pointerId);
    } catch {
      /* Synthetic QA pointer. */
    }
    useEditorStore.getState().setEditor({ isSliderDragging: true });
    drag.current.moved = true;
    patch(relightBallPatch(light, pointAt(e)));
  };
  const onPointerMove = (e: PointerEvent<HTMLDivElement>) => {
    if (!drag.current) return;
    patch(relightBallPatch(drag.current.before, pointAt(e)));
    debouncedSetHistory.cancel();
  };
  const onKeyDown = (e: KeyboardEvent<HTMLDivElement>) => {
    if (e.key === "Escape" && drag.current) {
      e.preventDefault();
      e.stopPropagation();
      finish(true);
      return;
    }
    if (inactive || e.metaKey || e.ctrlKey || e.altKey) return;
    const dx = e.key === "ArrowRight" ? 1 : e.key === "ArrowLeft" ? -1 : 0;
    const dy = e.key === "ArrowDown" ? 1 : e.key === "ArrowUp" ? -1 : 0;
    if (!dx && !dy) return;
    e.preventDefault();
    e.stopPropagation();
    const step = e.shiftKey ? 0.1 : 0.02;
    const p = relightBallPoint(light);
    patch(relightBallPatch(light, { x: p.x + dx * step, y: p.y + dy * step }));
  };
  const p = relightBallPoint(light);
  const px = SIZE / 2 + p.x * R;
  const py = SIZE / 2 + p.y * R;
  const label = t("studio.relight.direction");
  return (
    <div className="studio-relight-direction">
      <div
        role="slider"
        tabIndex={inactive ? -1 : 0}
        aria-label={label}
        aria-disabled={inactive || undefined}
        aria-describedby={reason ? reasonId : undefined}
        aria-valuetext={`${Math.round(light.rotation)}°, ${Math.round(light.elevation)}°`}
        aria-valuenow={Math.round(light.rotation)}
        aria-valuemin={-180}
        aria-valuemax={180}
        title={reason ?? t("studio.relight.directionHelp")}
        className="studio-relight-ball"
        data-disabled={inactive || undefined}
        style={{ width: SIZE, height: SIZE }}
        onPointerDown={onPointerDown}
        onPointerMove={onPointerMove}
        onPointerUp={() => finish(false)}
        onPointerCancel={() => finish(true)}
        onLostPointerCapture={() => finish(false)}
        onKeyDown={onKeyDown}
      >
        <svg width={SIZE} height={SIZE} aria-hidden="true">
          <defs>
            <radialGradient
              id={gradient}
              cx={px / SIZE}
              cy={py / SIZE}
              r={0.9}
              fx={px / SIZE}
              fy={py / SIZE}
            >
              <stop offset="0" stopColor={light.color} stopOpacity={0.95} />
              <stop offset="0.55" stopColor="#4a4a4a" />
              <stop offset="1" stopColor="#141414" />
            </radialGradient>
          </defs>
          <circle cx={SIZE / 2} cy={SIZE / 2} r={R} fill={`url(#${gradient})`} />
          <circle className="studio-relight-ball-ring" cx={SIZE / 2} cy={SIZE / 2} r={R} />
          <circle className="studio-relight-ball-ring" cx={SIZE / 2} cy={SIZE / 2} r={R / 2} />
          <circle className="studio-relight-ball-dot" cx={px} cy={py} r={6} />
        </svg>
      </div>
      {reason ? (
        <p id={reasonId} className="studio-relight-help studio-relight-reason">
          {reason}
        </p>
      ) : (
        <p className="studio-relight-help">{t("studio.relight.directionHelp")}</p>
      )}
    </div>
  );
}
