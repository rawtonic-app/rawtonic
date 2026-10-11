import { updateAdjustments } from '../../../hooks/useEditorActions';
import { useEditorStore } from '../../../store/useEditorStore';
import type { Adjustments, RetouchFace } from '../../../utils/adjustments';
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

/**
 * The face retouch's blemishes on the photograph: healed ones as circles,
 * kept ones dashed, the finder's other candidates faint. A click keeps a
 * healed blemish, heals a kept one again, or heals a candidate the slider
 * does not reach (Capture One has only a protection brush).
 */
export default function FaceRetouchOverlay({ rect, cropX, cropY, scale, zoom }: Props) {
  const retouch = useEditorStore((s) => s.adjustments.faceRetouch);
  const geometry = useEditorStore((s) => JSON.stringify(faceRetouchGeometry(s.adjustments)));
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
  return (
    <div
      style={{
        position: 'absolute',
        left: rect.left,
        top: rect.top,
        width: rect.width,
        height: rect.height,
        pointerEvents: 'none',
      }}
      aria-label="Face retouch blemishes"
    >
      <svg
        width={rect.width}
        height={rect.height}
        style={{ position: 'absolute', inset: 0, overflow: 'visible', pointerEvents: 'none' }}
      >
        {retouch.faces.flatMap((face) => {
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
                style={{ cursor: 'pointer', pointerEvents: 'all' }}
                onClick={(e) => {
                  e.stopPropagation();
                  toggle(face, b.id, isHealed);
                }}
              />
            );
          });
        })}
      </svg>
    </div>
  );
}
