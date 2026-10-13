import {
  useEffect,
  useRef,
  useId,
  type PointerEvent,
  type KeyboardEvent,
} from "react";
import { Sun, Lightbulb, Move, RotateCw, Scaling } from "lucide-react";
import { useTranslation } from "react-i18next";
import { useEditorStore } from "../../../store/useEditorStore";
import { useRelightStore } from "../../../store/useRelightStore";
import {
  updateAdjustments,
  debouncedSetHistory,
} from "../../../hooks/useEditorActions";
import {
  relightSource,
  relightDragPatch,
  type RelightHandle,
  type RelightLight,
  type RelightPoint,
} from "../../../utils/relight";

export default function RelightOverlay({
  rect,
  zoom,
}: {
  rect: { left: number; top: number; width: number; height: number };
  zoom: number;
}) {
  const { t } = useTranslation();
  const arrowId = useId();
  const root = useRef<HTMLDivElement>(null);
  const lights = useEditorStore((s) => s.adjustments.relightLights) ?? [];
  const selected = useRelightStore((s) => s.selected);
  const active = lights.find((l) => l.id === selected) ?? lights[0];
  const drag = useRef<{
    light: RelightLight;
    handle: RelightHandle;
    start: RelightPoint;
    now: RelightPoint;
    path: string;
    frame: number | null;
    moved: boolean;
  } | null>(null);
  const patch = (id: string, values: Partial<RelightLight>) =>
    updateAdjustments((prev) => ({
      ...prev,
      relightLights: prev.relightLights?.map((l) =>
        l.id === id ? { ...l, ...values } : l,
      ),
    }));
  const apply = () => {
    const d = drag.current;
    if (!d || useEditorStore.getState().selectedImage?.path !== d.path) return;
    d.frame = null;
    patch(
      d.light.id,
      relightDragPatch(
        d.light,
        d.handle,
        d.start,
        d.now,
        rect.width,
        rect.height,
      ),
    );
    // One history step for a whole gesture, even if the pointer pauses.
    debouncedSetHistory.cancel();
  };
  const finish = (cancel = false) => {
    const d = drag.current;
    if (!d) return;
    if (d.frame !== null) cancelAnimationFrame(d.frame);
    if (useEditorStore.getState().selectedImage?.path === d.path) {
      const changed = d.now.x !== d.start.x || d.now.y !== d.start.y;
      if ((cancel || !changed) && d.moved)
        updateAdjustments((prev) => ({
          ...prev,
          relightLights: prev.relightLights?.map((l) =>
            l.id === d.light.id ? d.light : l,
          ),
        }));
      else if (changed) apply();
      debouncedSetHistory.cancel();
      if (!cancel && changed)
        useEditorStore
          .getState()
          .pushHistory(useEditorStore.getState().adjustments);
    }
    drag.current = null;
    useEditorStore.getState().setEditor({ isSliderDragging: false });
  };
  useEffect(
    () => () => {
      const d = drag.current;
      if (!d) return;
      if (d.frame !== null) cancelAnimationFrame(d.frame);
      if (
        useEditorStore.getState().selectedImage?.path === d.path &&
        (d.now.x !== d.start.x || d.now.y !== d.start.y)
      )
        useEditorStore
          .getState()
          .pushHistory(useEditorStore.getState().adjustments);
      useEditorStore.getState().setEditor({ isSliderDragging: false });
    },
    [],
  );
  const point = (e: PointerEvent): RelightPoint => {
    const box = root.current!.getBoundingClientRect();
    return {
      x: ((e.clientX - box.left) / box.width) * 100,
      y: ((e.clientY - box.top) / box.height) * 100,
    };
  };
  const begin = (
    e: PointerEvent<HTMLButtonElement>,
    light: RelightLight,
    handle: RelightHandle,
  ) => {
    if (e.button !== 0 || !useEditorStore.getState().selectedImage?.isReady)
      return;
    e.preventDefault();
    e.stopPropagation();
    e.currentTarget.focus({ preventScroll: true });
    useRelightStore.setState({ selected: light.id });
    debouncedSetHistory.flush();
    drag.current = {
      light,
      handle,
      start: point(e),
      now: point(e),
      path: useEditorStore.getState().selectedImage!.path,
      frame: null,
      moved: false,
    };
    try {
      e.currentTarget.setPointerCapture(e.pointerId);
    } catch {
      /* Synthetic QA pointer. */
    }
    useEditorStore.getState().setEditor({ isSliderDragging: true });
  };
  const key = (
    e: KeyboardEvent<HTMLButtonElement>,
    light: RelightLight,
    handle: RelightHandle,
  ) => {
    if (e.key === "Escape") {
      e.stopPropagation();
      e.preventDefault();
      finish(true);
      return;
    }
    if (e.metaKey || e.ctrlKey || e.altKey) return;
    const dx = e.key === "ArrowRight" ? 1 : e.key === "ArrowLeft" ? -1 : 0;
    const dy = e.key === "ArrowDown" ? 1 : e.key === "ArrowUp" ? -1 : 0;
    if (!dx && !dy) return;
    e.stopPropagation();
    e.preventDefault();
    const amount = e.shiftKey ? 5 : 0.5;
    const values =
      handle === "rotate"
        ? {
            patternRotation:
              (((light.patternRotation ?? light.rotation) +
                (dx || -dy) * amount +
                540) %
                360) -
              180,
          }
        : handle === "scale"
          ? {
              scale: Math.max(
                5,
                Math.min(300, light.scale + (dx || -dy) * amount * 2),
              ),
            }
          : handle === "radius"
            ? {
                size: Math.max(
                  5,
                  Math.min(200, light.size + (dx || -dy) * amount * 2),
                ),
              }
            : relightDragPatch(
                light,
                handle,
                { x: 0, y: 0 },
                { x: dx * amount, y: dy * amount },
                rect.width,
                rect.height,
              );
    patch(light.id, values);
  };
  const button = (
    light: RelightLight,
    handle: RelightHandle,
    x: number,
    y: number,
    icon: React.ReactNode,
  ) => (
    <button
      key={`${light.id}-${handle}`}
      type="button"
      className={`relight-handle relight-handle-${handle}`}
      aria-label={t(`studio.relight.handles.${handle}`, {
        number: lights.indexOf(light) + 1,
      })}
      title={t(`studio.relight.handles.${handle}`, {
        number: lights.indexOf(light) + 1,
      })}
      aria-pressed={light.id === active?.id}
      style={{
        left: x,
        top: y,
        zIndex: handle === "source" ? (light.id === active?.id ? 3 : 2) : 1,
        transform: `translate(-50%, -50%) scale(${1 / zoom})`,
        color: handle === "source" ? light.color : undefined,
        opacity: light.enabled ? 1 : 0.5,
      }}
      onPointerDown={(e) => begin(e, light, handle)}
      onPointerMove={(e) => {
        if (!drag.current) return;
        e.stopPropagation();
        drag.current.now = point(e);
        drag.current.moved ||=
          drag.current.now.x !== drag.current.start.x ||
          drag.current.now.y !== drag.current.start.y;
        if (drag.current.frame === null)
          drag.current.frame = requestAnimationFrame(apply);
      }}
      onPointerUp={(e) => {
        if (!drag.current) return;
        e.stopPropagation();
        drag.current.now = point(e);
        finish();
      }}
      onPointerCancel={() => finish(true)}
      onLostPointerCapture={() => finish()}
      onKeyDown={(e) => key(e, light, handle)}
      onClick={(e) => {
        e.stopPropagation();
        useRelightStore.setState({ selected: light.id });
      }}
    >
      {icon}
    </button>
  );
  if (!active || rect.width <= 0 || rect.height <= 0) return null;
  const source = relightSource(active, rect.width, rect.height);
  const sx = (source.x / 100) * rect.width,
    sy = (source.y / 100) * rect.height;
  const cx = (active.x / 100) * rect.width,
    cy = (active.y / 100) * rect.height;
  const short = Math.min(rect.width, rect.height);
  const a = ((active.patternRotation ?? active.rotation) * Math.PI) / 180;
  const w =
    ((short * active.scale) / 100) * (active.pattern === "strip" ? 2 : 1);
  const h =
    ((short * active.scale) / 100) * (active.pattern === "strip" ? 0.3 : 1);
  const at = (x: number, y: number) => ({
    x: cx + Math.cos(a) * x - Math.sin(a) * y,
    y: cy + Math.sin(a) * x + Math.cos(a) * y,
  });
  const corner = at(w / 2, h / 2),
    rotate = at(0, -h / 2 - 30 / zoom),
    top = at(0, -h / 2);
  return (
    <div
      ref={root}
      className="relight-overlay"
      style={{
        left: rect.left,
        top: rect.top,
        width: rect.width,
        height: rect.height,
      }}
      onMouseDown={(e) => e.stopPropagation()}
      onDoubleClick={(e) => e.stopPropagation()}
    >
      <svg width="100%" height="100%" aria-hidden="true">
        <defs>
          <marker
            id={arrowId}
            viewBox="0 0 8 8"
            refX="7"
            refY="4"
            markerWidth="7"
            markerHeight="7"
            orient="auto"
          >
            <path
              d="M1 1 L7 4 L1 7"
              fill="none"
              stroke="white"
              strokeWidth="1.2"
            />
          </marker>
        </defs>
        <line
          className="relight-direction"
          markerEnd={`url(#${arrowId})`}
          x1={sx}
          y1={sy}
          x2={
            active.type === "sun"
              ? sx + (Math.sin((active.rotation * Math.PI) / 180) * 65) / zoom
              : cx
          }
          y2={
            active.type === "sun"
              ? sy - (Math.cos((active.rotation * Math.PI) / 180) * 65) / zoom
              : cy
          }
        />
        {active.type === "spot" && (
          <circle cx={sx} cy={sy} r={(short * active.size) / 100} />
        )}
        {active.pattern !== "none" && (
          <>
            <rect
              x={cx - w / 2}
              y={cy - h / 2}
              width={w}
              height={h}
              transform={`rotate(${(a * 180) / Math.PI} ${cx} ${cy})`}
            />
            <line x1={top.x} y1={top.y} x2={rotate.x} y2={rotate.y} />
          </>
        )}
      </svg>
      {lights.map((light) => {
        const p = relightSource(light, rect.width, rect.height);
        return button(
          light,
          "source",
          (p.x / 100) * rect.width,
          (p.y / 100) * rect.height,
          light.type === "sun" ? <Sun size={22} /> : <Lightbulb size={22} />,
        );
      })}
      {active.pattern !== "none" &&
        button(active, "move", cx, cy, <Move size={15} />)}
      {active.pattern !== "none" && (
        <>
          {button(active, "rotate", rotate.x, rotate.y, <RotateCw size={15} />)}
          {button(active, "scale", corner.x, corner.y, <Scaling size={15} />)}
        </>
      )}
      {active.type === "spot" &&
        button(
          active,
          "radius",
          sx + (short * active.size) / 100,
          sy,
          <Scaling size={15} />,
        )}
    </div>
  );
}
