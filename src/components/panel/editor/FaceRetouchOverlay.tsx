import { useEffect, useId, useRef, useState, type PointerEvent } from 'react';
import { updateAdjustments } from '../../../hooks/useEditorActions';
import { useEditorStore } from '../../../store/useEditorStore';
import type { Adjustments, FaceProtectStroke, RetouchFace } from '../../../utils/adjustments';
import { faceRetouchGeometry, healedBlemishes } from '../../studio/StudioFaceRetouch';

interface Props {
  /** The displayed photograph's box in the viewer's (zoomed) content. */
  rect: { left: number; top: number; width: number; height: number };
  /** Mask-frame pixels at the box's top left, and box pixels per mask pixel. */
  cropX: number;
  cropY: number;
  scale: number;
  /** The viewer's zoom, so lines keep their width on screen. */
  zoom: number;
}

type Point = { x: number; y: number };

/** Tint of the protected areas. */
const PROTECT_TINT = '#5fc4a8';

/**
 * The face retouch on the photograph. Its blemishes (Show blemishes):
 * healed ones as circles, kept ones dashed, the finder's other candidates
 * faint; a click keeps a healed blemish, heals a kept one again, or heals a
 * candidate the slider does not reach. While its protection brush is on,
 * a drag paints an area the
 * retouch leaves as it was (Alt or Erase takes it away again; [ and ]
 * change the size), and the protected areas are tinted.
 */
export default function FaceRetouchOverlay({ rect, cropX, cropY, scale, zoom }: Props) {
  const retouch = useEditorStore((s) => s.adjustments.faceRetouch);
  const geometry = useEditorStore((s) => JSON.stringify(faceRetouchGeometry(s.adjustments)));
  const showBlemishes = useEditorStore((s) => s.showFaceBlemishes);
  const brush = useEditorStore((s) => s.faceProtect);
  const showProtection = useEditorStore((s) => s.showFaceProtection) || brush.painting;
  const [stroke, setStroke] = useState<FaceProtectStroke | null>(null);
  const [cursor, setCursor] = useState<(Point & { erase: boolean }) | null>(null);
  const drag = useRef<FaceProtectStroke | null>(null);
  const ref = useRef<HTMLDivElement>(null);
  const maskId = useId();

  // [ and ] change the brush size while painting, as in the spot tool.
  useEffect(() => {
    if (!brush.painting) return;
    const onKey = (e: KeyboardEvent) => {
      const target = e.target as HTMLElement | null;
      if (target && (target.tagName === 'INPUT' || target.tagName === 'TEXTAREA' || target.isContentEditable)) return;
      if (e.key !== '[' && e.key !== ']') return;
      // Not the rotate shortcut on the same keys.
      e.preventDefault();
      e.stopImmediatePropagation();
      const { faceProtect, setEditor } = useEditorStore.getState();
      const size = Math.round(Math.min(500, Math.max(2, faceProtect.size * (e.key === ']' ? 1.15 : 1 / 1.15))));
      setEditor({ faceProtect: { ...faceProtect, size } });
    };
    window.addEventListener('keydown', onKey, true);
    return () => window.removeEventListener('keydown', onKey, true);
  }, [brush.painting]);

  if (!retouch || retouch.geometry !== geometry) return null;
  const line = 1.5 / zoom;
  const toggle = (face: RetouchFace, id: number, healed: boolean) =>
    updateAdjustments((prev: Adjustments) => {
      const r = prev.faceRetouch;
      if (!r) return prev;
      const faces = r.faces.map((f) => {
        if (f.id !== face.id) return f;
        const kept = f.kept.filter((k) => k !== id);
        const added = (f.added ?? []).filter((a) => a !== id);
        if (healed) return { ...f, kept: [...kept, id], added };
        // Kept or not reached: heal it (an added one only if the slider
        // does not reach it anyway).
        const reached = healedBlemishes({ ...f, kept, added }, r.settings).has(id);
        return { ...f, kept, added: reached ? added : [...added, id] };
      });
      return { ...prev, faceRetouch: { ...r, faces } };
    });

  const toImage = (clientX: number, clientY: number): Point => {
    const b = ref.current!.getBoundingClientRect();
    return {
      x: ((clientX - b.left) / b.width) * (rect.width / scale) + cropX,
      y: ((clientY - b.top) / b.height) * (rect.height / scale) + cropY,
    };
  };
  const onPointerDown = (e: PointerEvent<HTMLDivElement>) => {
    if (e.button !== 0) return;
    e.stopPropagation();
    try {
      e.currentTarget.setPointerCapture(e.pointerId);
    } catch {
      // Synthetic input without a tracked pointer: no capture.
    }
    const erase = brush.erase !== e.altKey;
    drag.current = {
      points: [toImage(e.clientX, e.clientY)],
      radius: brush.size,
      feather: brush.feather,
      ...(erase && { erase: true }),
    };
    setStroke(drag.current);
  };
  const onPointerMove = (e: PointerEvent<HTMLDivElement>) => {
    const p = toImage(e.clientX, e.clientY);
    setCursor({ ...p, erase: brush.erase !== e.altKey });
    const d = drag.current;
    if (!d) return;
    const last = d.points[d.points.length - 1];
    // A point every fifth of the brush radius (and at least a screen pixel).
    if (Math.hypot(p.x - last.x, p.y - last.y) >= Math.max(d.radius * 0.2, 1 / (scale * zoom))) {
      d.points = [...d.points, p];
      setStroke({ ...d });
    }
  };
  const onPointerUp = () => {
    const d = drag.current;
    drag.current = null;
    setStroke(null);
    if (!d) return;
    updateAdjustments((prev: Adjustments) => {
      const r = prev.faceRetouch;
      if (!r) return prev;
      return { ...prev, faceRetouch: { ...r, protect: [...(r.protect ?? []), d] } };
    });
  };

  const strokes = [...(retouch.protect ?? []), ...(stroke ? [stroke] : [])];
  // A stroke in box pixels: a round-capped line of the brush's width (a dot
  // for a dab), its soft edge as a blur of the feathered share.
  const strokePath = (s: FaceProtectStroke, i: number) => {
    if (s.points.length === 0) return null;
    const points = s.points.map((p) => ({ x: (p.x - cropX) * scale, y: (p.y - cropY) * scale }));
    const pts = points.map((p) => `${p.x},${p.y}`);
    const d = pts.length === 1 ? `M${pts[0]} l0.01,0` : `M${pts.join(' L')}`;
    const r = s.radius * scale;
    const soft = (s.feather / 100) * r;
    const blur = soft > 0.5 ? `${maskId}-soft-${i}` : undefined;
    // Object-bounding-box filters disappear on horizontal/vertical paths
    // and clip the cap of short strokes. Include the brush and blur reach.
    const bounds = points.reduce((b, p) => ({
      x0: Math.min(b.x0, p.x), y0: Math.min(b.y0, p.y),
      x1: Math.max(b.x1, p.x), y1: Math.max(b.y1, p.y),
    }), { x0: Infinity, y0: Infinity, x1: -Infinity, y1: -Infinity });
    return (
      <g key={i}>
        {blur && (
          <filter
            id={blur}
            filterUnits="userSpaceOnUse"
            x={bounds.x0 - 2 * r}
            y={bounds.y0 - 2 * r}
            width={bounds.x1 - bounds.x0 + 4 * r}
            height={bounds.y1 - bounds.y0 + 4 * r}
          >
            <feGaussianBlur stdDeviation={soft / 3} />
          </filter>
        )}
        <path
          d={d}
          fill="none"
          stroke={s.erase ? 'black' : 'white'}
          strokeWidth={2 * (r - soft / 2)}
          strokeLinecap="round"
          strokeLinejoin="round"
          filter={blur && `url(#${blur})`}
        />
      </g>
    );
  };

  return (
    <div
      ref={ref}
      style={{
        position: 'absolute',
        left: rect.left,
        top: rect.top,
        width: rect.width,
        height: rect.height,
        pointerEvents: brush.painting ? 'all' : 'none',
        cursor: brush.painting ? 'none' : undefined,
      }}
      aria-label="Face retouch"
      onPointerDown={brush.painting ? onPointerDown : undefined}
      onPointerMove={brush.painting ? onPointerMove : undefined}
      onPointerUp={brush.painting ? onPointerUp : undefined}
      onPointerCancel={brush.painting ? onPointerUp : undefined}
      onPointerLeave={() => setCursor(null)}
    >
      <svg
        width={rect.width}
        height={rect.height}
        style={{ position: 'absolute', inset: 0, overflow: 'visible', pointerEvents: 'none' }}
      >
        {showProtection && strokes.length > 0 && (
          <g aria-label="Protected areas">
            <mask id={maskId} maskUnits="userSpaceOnUse" x={0} y={0} width={rect.width} height={rect.height}>
              {strokes.map(strokePath)}
            </mask>
            <rect
              width={rect.width}
              height={rect.height}
              fill={PROTECT_TINT}
              fillOpacity={0.45}
              mask={`url(#${maskId})`}
            />
          </g>
        )}
        {showBlemishes &&
          retouch.faces.flatMap((face) => {
            if (!face.enabled) return [];
            const healed = healedBlemishes(face, retouch.settings);
            return face.blemishes.map((b) => {
              const isHealed = healed.has(b.id);
              const isKept = face.kept.includes(b.id);
              const state = isHealed ? 'healed' : isKept ? 'kept' : 'candidate';
              return (
                <circle
                  key={`${face.id}-${b.id}`}
                  data-state={state}
                  aria-label={`${state === 'healed' ? 'Keep' : 'Heal'} blemish ${b.id} of ${face.id}`}
                  cx={(b.x - cropX) * scale}
                  cy={(b.y - cropY) * scale}
                  r={Math.max(b.r * scale, 3 / zoom)}
                  fill="transparent"
                  stroke={isHealed ? 'var(--studio-accent, #9cc2e0)' : '#fff'}
                  strokeOpacity={state === 'candidate' ? 0.35 : 0.9}
                  strokeWidth={line}
                  strokeDasharray={isKept ? `${4 * line} ${3 * line}` : undefined}
                  // While painting, the brush takes the clicks.
                  style={{ cursor: 'pointer', pointerEvents: brush.painting ? 'none' : 'all' }}
                  onClick={(e) => {
                    e.stopPropagation();
                    toggle(face, b.id, isHealed);
                  }}
                />
              );
            });
          })}
        {brush.painting && cursor && (
          <g aria-hidden="true">
            <circle
              cx={(cursor.x - cropX) * scale}
              cy={(cursor.y - cropY) * scale}
              r={brush.size * scale}
              fill="none"
              stroke="#fff"
              strokeWidth={line}
              strokeDasharray={cursor.erase ? `${4 * line} ${3 * line}` : undefined}
            />
            {brush.feather > 0 && (
              <circle
                cx={(cursor.x - cropX) * scale}
                cy={(cursor.y - cropY) * scale}
                r={brush.size * scale * (1 - brush.feather / 100)}
                fill="none"
                stroke="#fff"
                strokeOpacity={0.5}
                strokeWidth={line}
              />
            )}
          </g>
        )}
      </svg>
    </div>
  );
}
