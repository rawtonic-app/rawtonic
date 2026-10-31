import { listen } from '@tauri-apps/api/event';
import { useEffect, useLayoutEffect, useRef, type RefObject } from 'react';
import { createPortal } from 'react-dom';
import { useTranslation } from 'react-i18next';
import { performanceDiagnosticsEnabled, reportPerformance } from '../../../hooks/usePerformanceDiagnostics';
import {
  PATCH_SIZE,
  createImagePatchFetch,
  createPatchSampler,
  loupePlacement,
  patchCell,
  pointerToNormalized,
  readoutColor,
  readoutValues,
  usePointerSampleStore,
  type PreviewPatch,
  type ReadoutUnit,
} from '../../../utils/pointerSample';

/** Samples the shown photograph under the pointer while `enabled` (a picker
 * is on, or the viewer bar's readout): pointer moves over `rootRef`, mapped
 * through the photograph's on-screen rect (`imageRef`), one sample in
 * flight. The native frame, or with `browserPreview` (no native display,
 * e.g. Linux) that rendered image. A press hides the loupe and stops
 * sampling until the next move after its release (a drag on the photograph
 * is a stroke, a crop or a pan, and must not wait behind readbacks); a new
 * frame under a resting pointer is sampled again. */
export function usePointerSampling(
  rootRef: RefObject<HTMLElement | null>,
  imageRef: RefObject<Element | null>,
  path: string,
  enabled: boolean,
  browserPreview: string | null = null,
) {
  const previewRef = useRef(browserPreview);
  previewRef.current = browserPreview;
  const askRef = useRef<(() => void) | null>(null);
  const browser = browserPreview !== null;
  useEffect(() => {
    const root = rootRef.current;
    const setSample = usePointerSampleStore.getState().set;
    if (!enabled || !root) {
      setSample(null);
      return;
    }
    let last: { clientX: number; clientY: number } | null = null;
    let held = false;
    const sampler = createPatchSampler(
      (sample, inputAt) => {
        if (!last || held) return;
        setSample(sample);
        if (performanceDiagnosticsEnabled && inputAt !== undefined) {
          // Profiling builds: pointer event → sample answered, and → the next
          // animation frame after the readout's commit (not photon time).
          const answered = performance.now();
          requestAnimationFrame(() =>
            reportPerformance('pointer_readout', {
              input_to_sample_ms: answered - inputAt,
              input_to_frame_ms: performance.now() - inputAt,
              sampled: sample.patch ? 1 : 0,
            }),
          );
        }
      },
      browser ? createImagePatchFetch(() => previewRef.current) : undefined,
    );
    const stop = () => {
      last = null;
      sampler.cancel();
      setSample(null);
    };
    const ask = (inputAt = performance.now()) => {
      const image = imageRef.current;
      if (!last || held || !image) return;
      const point = pointerToNormalized(last.clientX, last.clientY, image.getBoundingClientRect());
      if (!point) {
        stop();
        return;
      }
      sampler.request({ path, x: point.x, y: point.y, clientX: last.clientX, clientY: last.clientY, inputAt });
    };
    askRef.current = () => ask();
    const onMove = (event: PointerEvent) => {
      if (event.buttons !== 0) return;
      held = false;
      last = { clientX: event.clientX, clientY: event.clientY };
      // Synthetic input can carry a zero time stamp.
      ask(event.timeStamp > 0 ? event.timeStamp : performance.now());
    };
    const onDown = () => {
      held = true;
      stop();
    };
    const onUp = () => {
      held = false;
    };
    root.addEventListener('pointermove', onMove, { passive: true });
    root.addEventListener('pointerleave', stop);
    root.addEventListener('pointerdown', onDown, true);
    window.addEventListener('pointerup', onUp, true);
    window.addEventListener('pointercancel', onUp, true);
    let disposed = false;
    let unlisten: (() => void) | undefined;
    if (!browser)
      void listen<{ path: string }>('wgpu-frame-ready', (event) => {
        if (event.payload?.path === path) ask();
      }).then((unlistenFrames) => {
        if (disposed) unlistenFrames();
        else unlisten = unlistenFrames;
      }).catch(() => {});
    return () => {
      disposed = true;
      askRef.current = null;
      unlisten?.();
      root.removeEventListener('pointermove', onMove);
      root.removeEventListener('pointerleave', stop);
      root.removeEventListener('pointerdown', onDown, true);
      window.removeEventListener('pointerup', onUp, true);
      window.removeEventListener('pointercancel', onUp, true);
      sampler.cancel();
      setSample(null);
    };
  }, [rootRef, imageRef, path, enabled, browser]);
  // A new browser-rendered preview under a resting pointer.
  useEffect(() => {
    if (browserPreview) askRef.current?.();
  }, [browserPreview]);
}

const CELL = 12;
const GRID = PATCH_SIZE * CELL + 1;
export const LOUPE_WIDTH = GRID + 2;
export const LOUPE_HEIGHT = GRID + 26;

function drawPatch(canvas: HTMLCanvasElement, patch: PreviewPatch | null) {
  const ratio = window.devicePixelRatio || 1;
  canvas.width = GRID * ratio;
  canvas.height = GRID * ratio;
  const context = canvas.getContext('2d');
  if (!context) return;
  context.setTransform(ratio, 0, 0, ratio, 0, 0);
  context.fillStyle = '#26282b';
  context.fillRect(0, 0, GRID, GRID);
  if (!patch) return;
  for (let row = 0; row < PATCH_SIZE; row += 1) {
    for (let column = 0; column < PATCH_SIZE; column += 1) {
      const cell = patchCell(patch, column, row);
      if (!cell) continue;
      context.fillStyle = `rgb(${cell[0]} ${cell[1]} ${cell[2]})`;
      context.fillRect(column * CELL + 1, row * CELL + 1, CELL - 1, CELL - 1);
    }
  }
  // The sampled pixel: a dark and a light ring, readable on any colour.
  const x = (patch.column + patch.center[0]) * CELL;
  const y = (patch.row + patch.center[1]) * CELL;
  context.lineWidth = 1;
  context.strokeStyle = '#000';
  context.strokeRect(x + 0.5, y + 0.5, CELL, CELL);
  context.strokeStyle = '#fff';
  context.strokeRect(x - 0.5, y - 0.5, CELL + 2, CELL + 2);
}

/** Lightroom's picker loupe: the 11 × 11 display pixels the pickers average,
 * magnified, the sampled one marked, and their R, G, B below. Beside the
 * pointer, never over the sampled pixel, inside the viewer (`.studio-viewer`
 * around `anchorRef`); hidden off the photograph. Portalled to the body: the
 * canvas around the photograph is scaled with the zoom. */
export function PickerLoupe({ unit, anchorRef }: { unit: ReadoutUnit; anchorRef: RefObject<HTMLElement | null> }) {
  const { t } = useTranslation();
  const sample = usePointerSampleStore((s) => s.sample);
  const canvasRef = useRef<HTMLCanvasElement>(null);
  const patch = sample?.patch ?? null;
  const shown = !!sample;
  useLayoutEffect(() => {
    if (canvasRef.current) drawPatch(canvasRef.current, patch);
  }, [patch, shown]);
  if (!sample) return null;
  const anchor = anchorRef.current;
  const box = (anchor?.closest('.studio-viewer') ?? anchor)?.getBoundingClientRect() ??
    { left: 0, top: 0, width: window.innerWidth, height: window.innerHeight };
  const place = loupePlacement(
    { x: sample.clientX - box.left, y: sample.clientY - box.top },
    { width: LOUPE_WIDTH, height: LOUPE_HEIGHT },
    { width: box.width, height: box.height },
  );
  const color = patch && readoutColor(patch);
  const values = color ? readoutValues(color, unit) : ['–', '–', '–'];
  return createPortal(
    <div
      className="picker-loupe"
      data-testid="picker-loupe"
      aria-label={t('editor.pickerLoupe.label')}
      style={{ left: box.left + place.left, top: box.top + place.top, width: LOUPE_WIDTH }}
    >
      <canvas ref={canvasRef} style={{ width: GRID, height: GRID }} />
      <div className="picker-loupe-values">
        {(['R', 'G', 'B'] as const).map((channel, index) => (
          <span key={channel}>
            <span>{channel}</span> {values[index]}
          </span>
        ))}
        {unit === 'percent' && <span>%</span>}
      </div>
    </div>,
    document.body,
  );
}
