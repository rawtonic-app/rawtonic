import { useEffect, useId, useState } from 'react';
import { createPortal } from 'react-dom';
import { useEditorStore } from '../../store/useEditorStore';
import { useUIStore } from '../../store/useUIStore';
import { SURROUNDINGS_OPACITY, type ScreenRect } from '../../utils/viewerDisplay';
import { viewerShown } from '../../utils/viewerMode';

function clipPhoto(photo: ScreenRect, pane: DOMRect): ScreenRect {
  const left = Math.max(photo.left, pane.left);
  const top = Math.max(photo.top, pane.top);
  return {
    left,
    top,
    width: Math.min(photo.left + photo.width, pane.right) - left,
    height: Math.min(photo.top + photo.height, pane.bottom) - top
  };
}

function photoOpenings(): ScreenRect[] {
  const openings: ScreenRect[] = Array.from(document.querySelectorAll('[data-surroundings-cell]'), (cell) =>
    cell.getBoundingClientRect()
  );
  const photo = document.querySelector('[data-surroundings-photo]');
  const pane = photo?.closest('[data-surroundings-clip]');
  if (!photo || !pane) return openings;
  const viewport = pane.getBoundingClientRect();
  // The renderer shades its own backdrop so a webview update can never
  // cover a native frame that's still moving. Browser crop can rotate
  // outside the fitted box and likewise paints its own background.
  const protectPane = pane.hasAttribute('data-surroundings-protect-pane');
  const image = protectPane ? viewport : photo.getBoundingClientRect();
  openings.push(clipPhoto(image, viewport));
  const before = document.querySelector('[data-surroundings-mirror]')?.getBoundingClientRect();
  if (before) {
    openings.push(
      clipPhoto(
        {
          left: before.left + image.left - viewport.left,
          top: before.top + image.top - viewport.top,
          width: image.width,
          height: image.height
        },
        before
      )
    );
  }
  return openings;
}

function ActiveSurroundingsDimmer({ mode }: { mode: 'dim' | 'black' }) {
  const [openings, setOpenings] = useState<ScreenRect[]>([]);
  const maskId = useId();

  useEffect(() => {
    let frame: number;
    let previous = '';
    // Read the already transformed photo bounds, including imperative pan
    // and animated zoom that do not go through a React render each frame.
    const measure = () => {
      const next = photoOpenings()
        .filter(({ width, height }) => width > 0 && height > 0)
        .map(({ left, top, width, height }) => ({ left, top, width, height }));
      const key = JSON.stringify(next);
      if (key !== previous) {
        previous = key;
        setOpenings(next);
      }
      frame = requestAnimationFrame(measure);
    };
    measure();
    return () => cancelAnimationFrame(frame);
  }, []);

  return createPortal(
    <svg
      aria-hidden="true"
      data-testid="surroundings-dimmer"
      className="fixed inset-0 z-[35] h-full w-full"
      style={{ pointerEvents: 'none' }}
    >
      <defs>
        <mask id={maskId} maskUnits="userSpaceOnUse" x="0" y="0" width="100%" height="100%">
          <rect width="100%" height="100%" fill="white" />
          {openings.map(({ left, top, width, height }, index) => (
            <rect key={index} x={left} y={top} width={width} height={height} fill="black" />
          ))}
        </mask>
      </defs>
      {openings.length > 0 && (
        <rect width="100%" height="100%" fill="black" opacity={SURROUNDINGS_OPACITY[mode]} mask={`url(#${maskId})`} />
      )}
    </svg>,
    document.body
  );
}

/** Above workspace chrome, below popovers and dialogs. The mask has zero
 * alpha over the photograph so the native surface stays visible. */
export function SurroundingsDimmer() {
  const shown = useUIStore(viewerShown);
  const mode = useUIStore((s) => s.surroundings);
  const hasPhoto = useEditorStore((s) => !!s.selectedImage);
  useEffect(() => {
    if ((!shown || !hasPhoto) && mode !== 'normal') useUIStore.getState().setUI({ surroundings: 'normal' });
  }, [shown, hasPhoto, mode]);

  return shown && hasPhoto && mode !== 'normal' ? <ActiveSurroundingsDimmer mode={mode} /> : null;
}
