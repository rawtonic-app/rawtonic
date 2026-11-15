// Linux: the second window's photograph, drawn by its page from the JPEG
// frames second_window/browser.rs keeps (the main window's own frames, or
// ones developed for this window), laid out as the native display lays out
// its cell (secondViewZoom.ts `fitRect`, `zoomedRect`).
import { useEffect, useRef } from 'react';
import { convertFileSrc, invoke } from '@tauri-apps/api/core';
import { Invokes } from '../ui/AppProperties';
import type { SecondWindowFrame } from '../../store/useSecondWindowStore';
import { fitRect, zoomedRect, type Zoom } from './secondViewZoom';

type Rect = [number, number, number, number];

/** Profiling builds log each frame the page shows (`[PERF]` in the app log). */
function reportShown(layer: string) {
  if (import.meta.env.VITE_RAWTONIC_PERF !== '1') return;
  const record = { stage: 'second_page_frame', layer, epoch_ms: Date.now() };
  void invoke(Invokes.FrontendLog, { level: 'info', message: `[PERF] ${JSON.stringify(record)}` }).catch(() => {});
}

/** Where `frame` sits in a window `area` large (device px), or null when it
 * is not shown (a region while the window fits the photograph). */
export function frameRect(area: [number, number], frame: SecondWindowFrame, zoom: Zoom | null): Rect | null {
  if (!zoom) return frame.region ? null : fitRect(area, frame.shown);
  return zoomedRect(area, frame.shown, frame.region, zoom);
}

/** A frame drawn one of its pixels per device pixel (a 100 % region): its
 * place on whole device pixels at its own size, so WebKit copies its pixels
 * instead of filtering them at a fractional offset (LNX1, as the main
 * window's utils/pixelSnap.ts). Null for any other frame. */
export function pixelExactRect(rect: Rect, frame: SecondWindowFrame): Rect | null {
  if (Math.abs(rect[2] - frame.width) > 0.5 || Math.abs(rect[3] - frame.height) > 0.5) return null;
  return [Math.round(rect[0]), Math.round(rect[1]), frame.width, frame.height];
}

/** One layer: the newest frame that finished decoding (a frame decoded
 * after a newer one is dropped; the shown one stays until its replacement
 * is ready, so a drag's frames never blank the window). The decoded image
 * element itself is shown: nothing is loaded twice. */
function FrameLayer({
  name,
  frame,
  area,
  zoom,
  dpr
}: {
  name: string;
  frame: SecondWindowFrame | null;
  area: [number, number];
  zoom: Zoom | null;
  dpr: number;
}) {
  const box = useRef<HTMLDivElement>(null);
  const asked = useRef(0);
  const shownSerial = useRef(0);
  const shownFrame = useRef<SecondWindowFrame | null>(null);
  const layout = useRef({ area, zoom, dpr });
  layout.current = { area, zoom, dpr };

  const place = () => {
    const el = box.current;
    const current = shownFrame.current;
    if (!el) return;
    const rect = current && frameRect(layout.current.area, current, layout.current.zoom);
    if (!rect) {
      el.style.display = 'none';
      return;
    }
    const d = layout.current.dpr;
    const exact = pixelExactRect(rect, current);
    const placed = exact ?? rect;
    el.style.display = 'block';
    el.style.left = `${placed[0] / d}px`;
    el.style.top = `${placed[1] / d}px`;
    el.style.width = `${placed[2] / d}px`;
    el.style.height = `${placed[3] / d}px`;
    el.style.imageRendering = exact ? 'pixelated' : '';
  };

  useEffect(() => {
    const serial = ++asked.current;
    if (!frame) {
      shownSerial.current = serial;
      shownFrame.current = null;
      box.current?.replaceChildren();
      place();
      return;
    }
    const image = new Image();
    image.draggable = false;
    image.alt = '';
    image.className = 'block h-full w-full';
    const commit = () => {
      if (serial < shownSerial.current || !box.current) return;
      shownSerial.current = serial;
      shownFrame.current = frame;
      box.current.replaceChildren(image);
      place();
      reportShown(name);
    };
    image.src = convertFileSrc(frame.id, 'rawtonic-preview');
    if (typeof image.decode === 'function') void image.decode().then(commit, () => {});
    else image.onload = commit;
    // eslint-disable-next-line react-hooks/exhaustive-deps -- a frame is its id
  }, [frame?.id]);

  // eslint-disable-next-line react-hooks/exhaustive-deps -- placed from the refs
  useEffect(place, [area[0], area[1], zoom?.scale, zoom?.center[0], zoom?.center[1], dpr]);

  return <div ref={box} className="pointer-events-none absolute" style={{ display: 'none' }} />;
}

export default function SecondViewFrames({
  whole,
  region,
  area,
  zoom,
  dpr
}: {
  whole: SecondWindowFrame | null;
  region: SecondWindowFrame | null;
  area: [number, number];
  zoom: Zoom | null;
  dpr: number;
}) {
  return (
    <>
      <FrameLayer name="whole" frame={whole} area={area} zoom={zoom} dpr={dpr} />
      <FrameLayer name="region" frame={region} area={area} zoom={zoom} dpr={dpr} />
    </>
  );
}
