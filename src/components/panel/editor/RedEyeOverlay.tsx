import { invoke } from "@tauri-apps/api/core";
import { useEffect, useRef, useState, type PointerEvent } from "react";
import { useTranslation } from "react-i18next";
import { toast } from "react-toastify";
import { updateAdjustments } from "../../../hooks/useEditorActions";
import { useEditorStore } from "../../../store/useEditorStore";
import type { RedEye } from "../../../utils/adjustments";
import { GEOMETRY_KEYS, ORIENTATION_KEYS } from "../../../utils/geometryKeys";
import { Invokes } from "../../ui/AppProperties";

interface Props {
  /** The displayed photograph's box in the viewer's (zoomed) content. */
  rect: { left: number; top: number; width: number; height: number };
  /** Mask-frame pixels at the box's top left, and box pixels per mask pixel. */
  cropX: number;
  cropY: number;
  scale: number;
  /** The viewer's zoom, so handles keep their size on screen. */
  zoom: number;
}

type Point = { x: number; y: number };

/** An ellipse (`red_eye::Pupil`): as the detector finds and corrections
 * keep it, in the photo as decoded (units of its long edge); as
 * `red_eyes_shown` returns it, in mask-frame pixels. */
type Pupil = Pick<RedEye, "x" | "y" | "radiusX" | "radiusY" | "angle">;

/** A correction where the photo shows it. */
type Shown = { eye: RedEye; ellipse: Pupil };

type Drag =
  | { kind: "search"; start: Point; radius: number }
  | { kind: "move"; id: string; start: Point; offset: Point };

/** Screen pixels a press must travel to size the search circle. */
const DRAG_MIN = 4;

/** The render version new corrections get (`red_eye::VERSION`). */
const RED_EYE_VERSION = 1;

/** The catchlight's default place (`red_eye::DEFAULT_CATCHLIGHT`). */
const DEFAULT_CATCHLIGHT = { x: -0.3, y: -0.35 };

/** The adjustments that move where the photo shows a correction. */
const SHOWN_KEYS = [...GEOMETRY_KEYS, ...ORIENTATION_KEYS] as const;

/** How much Pupil Size scales the found ellipse (`red_eye::apply_red_eyes`). */
const pupilScale = (pupilSize: number) => 0.6 + 0.8 * (pupilSize / 100);

/** Whether (x, y) lies inside an ellipse scaled by `size` (grown by `margin`). */
function insideEllipse(
  e: Pupil,
  size: number,
  x: number,
  y: number,
  margin = 0,
) {
  const a = (e.angle * Math.PI) / 180;
  const dx = x - e.x;
  const dy = y - e.y;
  const u = dx * Math.cos(a) + dy * Math.sin(a);
  const v = -dx * Math.sin(a) + dy * Math.cos(a);
  return (
    (u / (e.radiusX * size + margin)) ** 2 +
      (v / (e.radiusY * size + margin)) ** 2 <=
    1
  );
}

/**
 * Red eye correction on the photograph (Lightroom's Red Eye and Pet Eye,
 * Capture One's Remove Red Eye): a click searches the circle around it for
 * the pupil (red, or for pet eyes glowing), a drag sizes that circle first;
 * the found ellipse becomes a correction. Corrections are kept in the photo
 * as decoded; the backend says where the geometry shows them
 * (`red_eyes_shown`). A correction can be selected and dragged; Delete
 * removes the selected one, [ and ] change the search circle, Escape
 * deselects.
 */
export default function RedEyeOverlay({
  rect,
  cropX,
  cropY,
  scale,
  zoom,
}: Props) {
  const { t } = useTranslation();
  const eyes = useEditorStore((s) => s.adjustments.redEyes ?? []);
  const activeId = useEditorStore((s) => s.activeSpotId);
  const settings = useEditorStore((s) => s.redEyeSettings);
  const [shown, setShown] = useState<Shown[]>([]);
  // The selected correction's drag offset (mask-frame pixels).
  const [draft, setDraft] = useState<{ id: string; offset: Point } | null>(
    null,
  );
  const [search, setSearch] = useState<{
    x: number;
    y: number;
    radius: number;
    pending: boolean;
  } | null>(null);
  const [hover, setHover] = useState<Point | null>(null);
  const drag = useRef<Drag | null>(null);
  const ref = useRef<HTMLDivElement>(null);

  const toBox = (x: number, y: number) =>
    [(x - cropX) * scale, (y - cropY) * scale] as const;
  const toImage = (clientX: number, clientY: number) => {
    const b = ref.current!.getBoundingClientRect();
    return {
      x: ((clientX - b.left) / b.width) * (rect.width / scale) + cropX,
      y: ((clientY - b.top) / b.height) * (rect.height / scale) + cropY,
    };
  };
  // One screen pixel in mask pixels.
  const pixel = 1 / (scale * zoom);

  // Where the photo shows its corrections, asked again when they or the
  // geometry change.
  const eyesKey = JSON.stringify(eyes);
  const geometryKey = useEditorStore((s) =>
    JSON.stringify(SHOWN_KEYS.map((key) => s.adjustments[key])),
  );
  useEffect(() => {
    const { adjustments } = useEditorStore.getState();
    const asked = adjustments.redEyes ?? [];
    if (asked.length === 0) {
      setShown([]);
      return;
    }
    let cancelled = false;
    invoke<(Pupil | null)[]>(Invokes.RedEyesShown, {
      jsAdjustments: adjustments,
    })
      .then((ellipses) => {
        if (cancelled) return;
        setShown(
          asked.flatMap((eye, i) => {
            const ellipse = ellipses[i];
            return ellipse ? [{ eye, ellipse }] : [];
          }),
        );
      })
      .catch((error) =>
        console.error("Showing red eye corrections failed:", error),
      );
    return () => {
      cancelled = true;
    };
  }, [eyesKey, geometryKey]);

  /** Searches the circle for a pupil and adds the correction it finds. */
  const place = async (x: number, y: number, radius: number) => {
    const { selectedImage, adjustments, redEyeSettings, setEditor } =
      useEditorStore.getState();
    if (!selectedImage?.isReady) return;
    const path = selectedImage.path;
    setSearch({ x, y, radius, pending: true });
    try {
      const pupil = await invoke<Pupil | null>(Invokes.DetectRedEye, {
        x,
        y,
        radius,
        kind: redEyeSettings.kind,
        jsAdjustments: adjustments,
      });
      if (useEditorStore.getState().selectedImage?.path !== path) return;
      if (!pupil) {
        toast.info(
          t(
            redEyeSettings.kind === "pet"
              ? "spotRemoval.redEye.noPetEye"
              : "spotRemoval.redEye.noRedEye",
          ),
        );
        return;
      }
      const eye: RedEye = {
        id: crypto.randomUUID(),
        version: RED_EYE_VERSION,
        kind: redEyeSettings.kind,
        ...pupil,
        pupilSize: redEyeSettings.pupilSize,
        darken: redEyeSettings.darken,
        catchlight: redEyeSettings.kind === "pet" && redEyeSettings.catchlight,
        catchlightX: DEFAULT_CATCHLIGHT.x,
        catchlightY: DEFAULT_CATCHLIGHT.y,
        visible: true,
      };
      updateAdjustments((prev) => ({
        ...prev,
        redEyes: [...(prev.redEyes ?? []), eye],
      }));
      setEditor({ activeSpotId: eye.id });
    } catch (error) {
      console.error("Red eye detection failed:", error);
    } finally {
      setSearch(null);
    }
  };

  /** Keeps a correction dragged to mask-frame (x, y) at that spot of the
   * photo as decoded. */
  const move = async (id: string, x: number, y: number) => {
    const { selectedImage, adjustments } = useEditorStore.getState();
    const path = selectedImage?.path;
    try {
      const [ix, iy] = await invoke<[number, number]>(Invokes.RedEyePoint, {
        x,
        y,
        jsAdjustments: adjustments,
      });
      if (useEditorStore.getState().selectedImage?.path !== path) return;
      updateAdjustments((prev) => ({
        ...prev,
        redEyes: (prev.redEyes ?? []).map((eye) =>
          eye.id === id ? { ...eye, x: ix, y: iy } : eye,
        ),
      }));
    } catch (error) {
      console.error("Moving the red eye correction failed:", error);
    } finally {
      setDraft(null);
    }
  };

  const onPointerDown = (e: PointerEvent<HTMLDivElement>) => {
    if (e.button !== 0) return;
    e.stopPropagation();
    const p = toImage(e.clientX, e.clientY);
    try {
      e.currentTarget.setPointerCapture(e.pointerId);
    } catch {
      // Synthetic input without a tracked pointer: no capture.
    }
    // The selected correction first, then the others from the top.
    const hit = [...shown]
      .reverse()
      .sort(
        (a, b) => Number(b.eye.id === activeId) - Number(a.eye.id === activeId),
      )
      .find(({ eye, ellipse }) =>
        insideEllipse(ellipse, pupilScale(eye.pupilSize), p.x, p.y, 3 * pixel),
      );
    if (hit) {
      useEditorStore.getState().setEditor({ activeSpotId: hit.eye.id });
      drag.current = {
        kind: "move",
        id: hit.eye.id,
        start: p,
        offset: { x: 0, y: 0 },
      };
      return;
    }
    drag.current = { kind: "search", start: p, radius: settings.size };
    setSearch({ x: p.x, y: p.y, radius: settings.size, pending: false });
  };

  const onPointerMove = (e: PointerEvent<HTMLDivElement>) => {
    const p = toImage(e.clientX, e.clientY);
    const d = drag.current;
    if (!d) {
      setHover(p);
      return;
    }
    if (d.kind === "move") {
      // Kept on the drag too: the release may come before a render.
      d.offset = { x: p.x - d.start.x, y: p.y - d.start.y };
      setDraft({ id: d.id, offset: d.offset });
    } else {
      const travelled = Math.hypot(p.x - d.start.x, p.y - d.start.y);
      // A drag sizes the circle from its centre.
      if (travelled >= DRAG_MIN * pixel) d.radius = Math.max(2, travelled);
      setSearch({
        x: d.start.x,
        y: d.start.y,
        radius: d.radius,
        pending: false,
      });
    }
  };

  const onPointerUp = (e: PointerEvent<HTMLDivElement>) => {
    const d = drag.current;
    drag.current = null;
    try {
      if (e.currentTarget.hasPointerCapture(e.pointerId))
        e.currentTarget.releasePointerCapture(e.pointerId);
    } catch {
      // See setPointerCapture.
    }
    if (d?.kind === "search") {
      const { setEditor, redEyeSettings } = useEditorStore.getState();
      if (d.radius !== redEyeSettings.size) {
        setEditor({
          redEyeSettings: { ...redEyeSettings, size: Math.round(d.radius) },
        });
      }
      void place(d.start.x, d.start.y, d.radius);
      return;
    }
    if (d?.kind === "move") {
      const at = shown.find((item) => item.eye.id === d.id);
      if (at && (d.offset.x !== 0 || d.offset.y !== 0)) {
        void move(d.id, at.ellipse.x + d.offset.x, at.ellipse.y + d.offset.y);
        return;
      }
    }
    setDraft(null);
  };

  // Delete removes the selected correction; [ and ] change the search
  // circle; Escape deselects.
  useEffect(() => {
    const onKeyDown = (e: KeyboardEvent) => {
      const target = e.target as HTMLElement | null;
      if (
        target &&
        (target.tagName === "INPUT" ||
          target.tagName === "TEXTAREA" ||
          target.isContentEditable)
      )
        return;
      const {
        activeSpotId: id,
        redEyeSettings,
        setEditor,
        adjustments,
      } = useEditorStore.getState();
      const selected = adjustments.redEyes?.some((eye) => eye.id === id);
      if ((e.key === "Delete" || e.key === "Backspace") && selected) {
        e.preventDefault();
        e.stopImmediatePropagation();
        updateAdjustments((prev) => ({
          ...prev,
          redEyes: (prev.redEyes ?? []).filter((eye) => eye.id !== id),
        }));
        setEditor({ activeSpotId: null });
      } else if (e.key === "[" || e.key === "]") {
        e.preventDefault();
        e.stopImmediatePropagation();
        // 15 % a press, and at least a pixel.
        const size = redEyeSettings.size;
        const next =
          e.key === "]"
            ? Math.max(size + 1, Math.round(size * 1.15))
            : Math.min(size - 1, Math.round(size / 1.15));
        setEditor({
          redEyeSettings: { ...redEyeSettings, size: Math.max(2, next) },
        });
      } else if (e.key === "Escape" && id) {
        e.preventDefault();
        e.stopImmediatePropagation();
        setEditor({ activeSpotId: null });
      }
    };
    window.addEventListener("keydown", onKeyDown, true);
    return () => window.removeEventListener("keydown", onKeyDown, true);
  }, []);

  const line = 1.5 * pixel * scale;
  const circle = (key: string, at: Point, r: number, dashed: boolean) => {
    const [bx, by] = toBox(at.x, at.y);
    return (
      <g key={key}>
        <circle
          cx={bx}
          cy={by}
          r={r * scale}
          fill="none"
          stroke="rgba(0,0,0,0.6)"
          strokeWidth={line * 2.4}
        />
        <circle
          cx={bx}
          cy={by}
          r={r * scale}
          fill="none"
          stroke="currentColor"
          strokeWidth={line}
          strokeDasharray={dashed ? `${4 * line} ${3 * line}` : undefined}
        />
      </g>
    );
  };

  return (
    <div
      ref={ref}
      aria-label={t("spotRemoval.redEye.overlay")}
      data-testid="red-eye-overlay"
      style={{
        position: "absolute",
        left: rect.left,
        top: rect.top,
        width: rect.width,
        height: rect.height,
        zIndex: 6,
        cursor: "crosshair",
        touchAction: "none",
      }}
      onPointerDown={onPointerDown}
      onPointerMove={onPointerMove}
      onPointerUp={onPointerUp}
      onPointerCancel={onPointerUp}
      onPointerLeave={() => setHover(null)}
    >
      <svg
        width={rect.width}
        height={rect.height}
        style={{
          position: "absolute",
          inset: 0,
          overflow: "visible",
          pointerEvents: "none",
        }}
      >
        {shown.map(({ eye, ellipse }) => {
          const offset = draft?.id === eye.id ? draft.offset : { x: 0, y: 0 };
          const [cx, cy] = toBox(ellipse.x + offset.x, ellipse.y + offset.y);
          const active = eye.id === activeId;
          // The ellipse the correction covers at its Pupil Size.
          const size = pupilScale(eye.pupilSize);
          const rx = ellipse.radiusX * size * scale;
          const ry = ellipse.radiusY * size * scale;
          return (
            <g
              key={eye.id}
              data-testid="red-eye"
              style={{
                color: active ? "var(--studio-accent, #9cc2e0)" : "#fff",
              }}
              transform={`rotate(${ellipse.angle} ${cx} ${cy})`}
            >
              <ellipse
                cx={cx}
                cy={cy}
                rx={rx}
                ry={ry}
                fill="none"
                stroke="rgba(0,0,0,0.6)"
                strokeWidth={line * 2.4}
              />
              <ellipse
                cx={cx}
                cy={cy}
                rx={rx}
                ry={ry}
                fill="none"
                stroke="currentColor"
                strokeWidth={line}
              />
            </g>
          );
        })}
        {search && (
          <g style={{ color: search.pending ? "#f0c060" : "#fff" }}>
            {circle("search", search, search.radius, true)}
          </g>
        )}
        {!search && hover && !drag.current && (
          <g style={{ color: "rgba(255,255,255,0.7)" }}>
            {circle("hover", hover, settings.size, true)}
          </g>
        )}
      </svg>
    </div>
  );
}
