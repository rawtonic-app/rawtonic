import { invoke } from '@tauri-apps/api/core';
import { listen } from '@tauri-apps/api/event';
import { useTranslation } from 'react-i18next';
import { useEffect, useRef, useState, type PointerEvent } from 'react';
import { updateAdjustments } from '../../../hooks/useEditorActions';
import { useEditorStore } from '../../../store/useEditorStore';
import type { AiPatch, AiPatchData, Spot } from '../../../utils/adjustments';
import { Invokes } from '../../ui/AppProperties';
import { Mask, SubMaskMode, ToolType } from '../right/Masks';
import { useProcessStore } from '../../../store/useProcessStore';
import { isAiTaskCancelled, toastAiError } from '../../../utils/aiErrors';
import { toast } from 'react-toastify';
import { getTransformAdjustments } from '../../../hooks/useAiMasking';
import { createSubMask } from '../../../utils/maskUtils';
import { shapeAsShown, shapeFrame, type SizedFrame } from '../../../utils/maskGeometry';

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

/** What a Detect objects selection does: a new removal, or adding to or
 * subtracting from the selected one (Shift and Alt, as in the masks). */
type SelectOp = 'new' | 'add' | 'subtract';

type Drag =
  | { kind: 'target' | 'source'; id: string; startX: number; startY: number; spot: Spot }
  | { kind: 'radius'; id: string; spot: Spot }
  | { kind: 'paint'; points: Point[] }
  | { kind: 'box'; start: Point; end: Point; op: SelectOp }
  | { kind: 'objectStroke'; points: Point[]; op: SelectOp };

/** Screen pixels within which a press on a circle's rim resizes it. */
const RIM = 6;

/** Screen radius of the pin marking an object removed by a click. */
const PIN = 8;

/** Screen pixels a press must travel to draw a box rather than click. */
const BOX_MIN = 5;

/** A removal made by this tool (an AI patch named Remove): a painted area
 * (brush stroke or circle), or an object selected by a click or box. */
type Removal =
  | { patch: AiPatch; kind: 'area'; points: Point[]; radius: number }
  | { patch: AiPatch; kind: 'object'; start: Point; end: Point };

/** The removals among the recipe's AI patches; painted areas where the
 * photo shows them `now` (a stroke painted before a turn stays on its spot). */
function removalsOf(aiPatches: AiPatch[] | undefined, now?: SizedFrame): Removal[] {
  return (aiPatches ?? []).flatMap((patch): Removal[] => {
    if (patch.name !== 'Remove') return [];
    const sub = patch.subMasks[0];
    if (sub?.type === Mask.QuickEraser) {
      const p = sub.parameters as { startX?: number; startY?: number; endX?: number; endY?: number };
      if (p.startX === undefined || p.startY === undefined || p.endX === undefined || p.endY === undefined) return [];
      return [{ patch, kind: 'object', start: { x: p.startX, y: p.startY }, end: { x: p.endX, y: p.endY } }];
    }
    const shown = sub?.parameters && shapeAsShown(sub.type, sub.parameters, now);
    const line = (shown?.lines as { points: Point[]; brushSize: number }[] | undefined)?.[0];
    return line ? [{ patch, kind: 'area', points: line.points, radius: line.brushSize / 2 }] : [];
  });
}

/** Whether an object removal was made by a click (a point, not a box). */
const isClick = (start: Point, end: Point) => Math.abs(start.x - end.x) < 1e-6 && Math.abs(start.y - end.y) < 1e-6;

/** Where an object removal's pin sits: the click, or the box's centre. */
const pinOf = (start: Point, end: Point) =>
  isClick(start, end) ? start : { x: (start.x + end.x) / 2, y: (start.y + end.y) / 2 };

/** The prompt for the object mask model from a loose stroke over an
 * object: the box around the painted area (the stroke grown by the brush
 * radius), or a click where the stroke barely moved. */
export function strokePrompt(points: Point[], radius: number): { start: Point; end: Point } {
  const travelled = points.reduce(
    (sum, p, i) => (i ? sum + Math.hypot(p.x - points[i - 1].x, p.y - points[i - 1].y) : 0),
    0,
  );
  if (travelled < radius * 0.5) return { start: points[0], end: points[0] };
  const xs = points.map((p) => p.x);
  const ys = points.map((p) => p.y);
  return {
    start: { x: Math.max(0, Math.min(...xs) - radius), y: Math.max(0, Math.min(...ys) - radius) },
    end: { x: Math.max(...xs) + radius, y: Math.max(...ys) + radius },
  };
}

/** Colour of a selection being made: new, added (green) or subtracted (red). */
const OP_COLOUR: Record<SelectOp, string> = { new: '#f0c060', add: '#7fd18b', subtract: '#ef7f7f' };

/** Whether a spot is a brush stroke rather than a circle. */
const isStroke = (spot: Spot): spot is Spot & { points: Point[] } => (spot.points?.length ?? 0) >= 2;

/** Distance from (x, y) to the polyline `points`. */
function distanceToPath(x: number, y: number, points: Point[]) {
  let best = Infinity;
  for (let i = 1; i < points.length; i++) {
    const a = points[i - 1];
    const b = points[i];
    const vx = b.x - a.x;
    const vy = b.y - a.y;
    const length2 = vx * vx + vy * vy;
    const t = length2 > 0 ? Math.min(1, Math.max(0, ((x - a.x) * vx + (y - a.y) * vy) / length2)) : 0;
    best = Math.min(best, Math.hypot(x - a.x - t * vx, y - a.y - t * vy));
  }
  return best;
}

/** Distance from (x, y) to a spot's area edge-in: negative inside. */
function distanceToSpot(x: number, y: number, spot: Spot, dx = 0, dy = 0) {
  if (isStroke(spot)) return distanceToPath(x - dx, y - dy, spot.points) - spot.radius;
  return Math.hypot(x - dx - spot.x, y - dy - spot.y) - spot.radius;
}

/**
 * Spot removal on the photograph (Lightroom's Healing, Capture One's Heal
 * and Clone): a click places a spot with an automatically chosen source,
 * a drag paints a brush stroke (Lightroom's heal strokes) that copies from
 * one automatically chosen offset; the spot or its source can be dragged, a
 * circle's rim resizes it, Delete removes the selected spot and [ ] change
 * its size. Changes reach the recipe when a drag ends, because the backend
 * heals the full-resolution base image.
 *
 * Remove fills the area with the local inpainting model; with Detect
 * objects (Lightroom's object-aware Remove) a click on an object or a box
 * drawn around it selects it with the object mask model, and the selection,
 * grown a little to take its edge along, is filled. Removals are AI patches,
 * so exports carry the generated-content marking. A large hole on macOS is
 * filled with Moebius, which takes a few seconds (and downloads its model
 * the first time): the overlay shows its progress with Cancel (or Escape).
 */
/** A running removal's progress: its model's download, then the network's runs. */
type FillProgress = { taskId: string; download?: number; done?: number; total?: number };

export default function SpotOverlay({ rect, cropX, cropY, scale, zoom }: Props) {
  const { t } = useTranslation();
  const spots = useEditorStore((s) => s.adjustments.spots ?? []);
  const aiPatches = useEditorStore((s) => s.adjustments.aiPatches);
  const rotation = useEditorStore((s) => s.adjustments.rotation);
  const flipHorizontal = useEditorStore((s) => s.adjustments.flipHorizontal);
  const flipVertical = useEditorStore((s) => s.adjustments.flipVertical);
  const orientationSteps = useEditorStore((s) => s.adjustments.orientationSteps);
  const photoWidth = useEditorStore((s) => s.selectedImage?.width);
  const photoHeight = useEditorStore((s) => s.selectedImage?.height);
  const now = shapeFrame({ rotation, flipHorizontal, flipVertical, orientationSteps }, photoWidth, photoHeight);
  // Remove areas and objects: the AI patches this tool made.
  const removals = removalsOf(aiPatches, now);
  const activeSpotId = useEditorStore((s) => s.activeSpotId);
  const settings = useEditorStore((s) => s.spotSettings);
  const visualize = useEditorStore((s) => s.spotVisualize);
  const [view, setView] = useState<string | null>(null);
  const [draft, setDraft] = useState<Spot | null>(null);
  const [stroke, setStroke] = useState<Point[] | null>(null);
  const [pending, setPending] = useState<{ x: number; y: number; radius: number; points?: Point[] } | null>(null);
  // An object being drawn around (box) or being selected and removed.
  const [box, setBox] = useState<{ start: Point; end: Point } | null>(null);
  const [pendingObject, setPendingObject] = useState<{ start: Point; end: Point; op: SelectOp } | null>(null);
  // A loose stroke over an object (Detect objects with the brush).
  const [objectStroke, setObjectStroke] = useState<{ points: Point[]; op: SelectOp } | null>(null);
  const detectObjects = settings.mode === 'remove' && Boolean(settings.detectObjects);
  const objectBrush = detectObjects && (settings.objectSelect ?? 'brush') === 'brush';
  const drag = useRef<Drag | null>(null);
  const ref = useRef<HTMLDivElement>(null);
  // The removals being filled (task ids, newest last) and what the backend
  // reported of each; the newest one with a report is shown.
  const filling = useRef<string[]>([]);
  const [reports, setReports] = useState<FillProgress[]>([]);
  const report = (next: FillProgress) =>
    setReports((all) => [...all.filter((r) => r.taskId !== next.taskId), next]);
  const progress =
    filling.current
      .toReversed()
      .map((id) => reports.find((r) => r.taskId === id))
      .find((r) => r !== undefined) ?? null;
  const cancelFill = (taskId: string) => {
    invoke(Invokes.CancelAiTask, { taskId }).catch(() => {});
  };

  const toBox = (x: number, y: number) => [(x - cropX) * scale, (y - cropY) * scale] as const;
  const toImage = (clientX: number, clientY: number) => {
    const b = ref.current!.getBoundingClientRect();
    return [
      ((clientX - b.left) / b.width) * (rect.width / scale) + cropX,
      ((clientY - b.top) / b.height) * (rect.height / scale) + cropY,
    ] as const;
  };
  const shown = spots.map((s) => (draft && s.id === draft.id ? draft : s));
  // One screen pixel in mask pixels.
  const pixel = 1 / (scale * zoom);

  const commit = (spot: Spot) =>
    updateAdjustments((prev) => ({
      ...prev,
      spots: (prev.spots ?? []).map((s) => (s.id === spot.id ? spot : s)),
    }));

  /** Fills `patch` (an AI patch named Remove) with the local inpainting
   * model and adds it to the recipe once filled (in place of the patch
   * `replaces`: a refined selection), so a removal is one undo step and a
   * failed one leaves the recipe as it was (exports with it are marked as
   * containing generated areas). `select` first runs the object mask model
   * and returns the patch with its selection. A refined removal gets a new
   * id: the preview sends a patch's pixels once per id, and undo brings the
   * old one back under its own. */
  const fillAndAdd = async (
    patch: AiPatch,
    select?: (patch: AiPatch) => Promise<AiPatch>,
    replaces?: string,
  ) => {
    const { selectedImage, adjustments: current } = useEditorStore.getState();
    // The fill starts from the photo without the removal it replaces.
    const adjustments = replaces
      ? { ...current, aiPatches: (current.aiPatches ?? []).filter((p) => p.id !== replaces) }
      : current;
    if (!selectedImage?.isReady) return;
    const path = selectedImage.path;
    const { registerAiTask, unregisterAiTask } = useProcessStore.getState();
    registerAiTask(patch.id, 'inpaint');
    filling.current = [...filling.current, patch.id];
    try {
      const definition = select ? await select(patch) : patch;
      if (useEditorStore.getState().selectedImage?.path !== path) return;
      const json = await invoke<string>(Invokes.InvokeGenerativeReplaseWithMaskDef, {
        currentAdjustments: adjustments,
        patchDefinition: definition,
        path,
        useFastInpaint: true,
        taskId: patch.id,
      });
      if (useEditorStore.getState().selectedImage?.path !== path) return;
      const done: AiPatch = { ...definition, patchData: JSON.parse(json) as AiPatchData, isLoading: false };
      updateAdjustments((prev) => {
        const patches = prev.aiPatches ?? [];
        return {
          ...prev,
          aiPatches:
            replaces && patches.some((p) => p.id === replaces)
              ? patches.map((p) => (p.id === replaces ? done : p))
              : [...patches, done],
        };
      });
      useEditorStore.getState().setEditor({ activeSpotId: patch.id });
    } catch (error) {
      // A cancel is the user's choice, not a failure.
      if (isAiTaskCancelled(error)) console.info('Remove cancelled');
      else console.error('Remove failed:', error);
      toastAiError(error, `Remove failed: ${error}`);
    } finally {
      unregisterAiTask(patch.id);
      filling.current = filling.current.filter((id) => id !== patch.id);
      setReports((all) => all.filter((r) => r.taskId !== patch.id));
    }
  };

  /** Lightroom's Remove: the circle or stroke is filled by the local
   * inpainting model. */
  const removeArea = async (x: number, y: number, points?: Point[]) => {
    const radius = settings.size;
    const { selectedImage, adjustments } = useEditorStore.getState();
    // The geometry the area is painted in, so it stays on its spot.
    const frame = shapeFrame(adjustments, selectedImage?.width, selectedImage?.height);
    setPending({ x, y, radius, points });
    try {
      await fillAndAdd({
        id: crypto.randomUUID(),
        name: 'Remove',
        isLoading: false,
        invert: false,
        patchData: null,
        prompt: '',
        visible: true,
        subMasks: [
          {
            id: crypto.randomUUID(),
            type: Mask.Brush,
            mode: SubMaskMode.Additive,
            invert: false,
            opacity: 100,
            visible: true,
            parameters: {
              lines: [
                {
                  points: points ?? [{ x, y }],
                  brushSize: radius * 2,
                  feather: 0.2,
                  tool: ToolType.Brush,
                  ...(frame && { frame }),
                },
              ],
            },
          },
        ],
      });
    } finally {
      setPending(null);
    }
  };

  /** Lightroom's object-aware Remove: a click on an object (start = end), a
   * box around it or a loose stroke over it (its box) selects it with the
   * object mask model (the selection grown and feathered so the fill takes
   * its edge along), then the local inpainting model fills it. With `op`
   * add or subtract, the selection is added to or taken from the selected
   * object removal (a component of its own, as in the masks), which is
   * filled again from the original pixels. */
  const removeObject = async (start: Point, end: Point, op: SelectOp = 'new') => {
    const { selectedImage, adjustments, activeSpotId: activeId } = useEditorStore.getState();
    if (!selectedImage?.isReady) return;
    const target = op === 'new' ? undefined : adjustments.aiPatches?.find((p) => p.id === activeId);
    if (op !== 'new' && !target) return;
    const created = createSubMask(Mask.QuickEraser, { width: selectedImage.width, height: selectedImage.height });
    const selection = {
      ...created,
      name: 'Object',
      mode: op === 'subtract' ? SubMaskMode.Subtractive : SubMaskMode.Additive,
      parameters: { ...created.parameters, startX: start.x, startY: start.y, endX: end.x, endY: end.y },
    };
    setPendingObject({ start, end, op });
    try {
      await fillAndAdd(
        target
          ? { ...target, id: crypto.randomUUID(), isLoading: false }
          : {
              id: crypto.randomUUID(),
              name: 'Remove',
              isLoading: false,
              invert: false,
              patchData: null,
              prompt: '',
              visible: true,
              subMasks: [],
            },
        async (patch) => {
          const { adjustments } = useEditorStore.getState();
          const mask = await invoke<Record<string, unknown>>(Invokes.GenerateAiSubjectMask, {
            jsAdjustments: getTransformAdjustments(adjustments),
            path: selectedImage.path,
            startPoint: [start.x, start.y],
            endPoint: [end.x, end.y],
            rotation: adjustments.rotation,
            flipHorizontal: adjustments.flipHorizontal,
            flipVertical: adjustments.flipVertical,
            orientationSteps: adjustments.orientationSteps,
            taskId: patch.id,
          });
          return {
            ...patch,
            subMasks: [...patch.subMasks, { ...selection, parameters: { ...selection.parameters, ...mask } }],
          };
        },
        target?.id,
      );
    } finally {
      setPendingObject(null);
    }
  };

  /** Places a circle at (x, y), or a brush stroke along `points`. */
  const place = async (x: number, y: number, points?: Point[]) => {
    if (settings.mode === 'remove') return removeArea(x, y, points);
    const { selectedImage, adjustments, setEditor } = useEditorStore.getState();
    if (!selectedImage?.isReady) return;
    const radius = settings.size;
    setPending({ x, y, radius, points });
    const path = selectedImage.path;
    try {
      const [sourceX, sourceY] = await invoke<[number, number]>(Invokes.FindSpotSource, {
        x,
        y,
        radius,
        points: points ?? null,
        jsAdjustments: adjustments,
      });
      if (useEditorStore.getState().selectedImage?.path !== path) return;
      const spot: Spot = {
        id: crypto.randomUUID(),
        mode: settings.mode === 'clone' ? 'clone' : 'heal',
        x,
        y,
        radius,
        sourceX,
        sourceY,
        feather: settings.feather,
        opacity: settings.opacity,
        visible: true,
        ...(points && { points }),
      };
      updateAdjustments((prev) => ({ ...prev, spots: [...(prev.spots ?? []), spot] }));
      setEditor({ activeSpotId: spot.id });
    } catch (error) {
      console.error('Spot removal: no source found', error);
    } finally {
      setPending(null);
    }
  };

  const onPointerDown = (e: PointerEvent<HTMLDivElement>) => {
    if (e.button !== 0) return;
    e.stopPropagation();
    const [x, y] = toImage(e.clientX, e.clientY);
    const { setEditor } = useEditorStore.getState();
    // The selected spot first, then the others from the top.
    const ordered = [...shown].sort((a, b) => Number(b.id === activeSpotId) - Number(a.id === activeSpotId));
    // Shift or Alt with an object removal selected: the press refines its
    // selection (anywhere, also on pins).
    const refining =
      detectObjects &&
      (e.shiftKey || e.altKey) &&
      removals.some((r) => r.kind === 'object' && r.patch.id === activeSpotId);
    const capture = () => {
      try {
        e.currentTarget.setPointerCapture(e.pointerId);
      } catch {
        // Synthetic input without a tracked pointer: no capture.
      }
    };
    for (const spot of ordered) {
      const toTarget = distanceToSpot(x, y, spot);
      const toSource = distanceToSpot(x, y, spot, spot.sourceX - spot.x, spot.sourceY - spot.y);
      const hit = (kind: 'target' | 'source' | 'radius') => {
        capture();
        drag.current =
          kind === 'radius' ? { kind, id: spot.id, spot } : { kind, id: spot.id, startX: x, startY: y, spot };
        setEditor({ activeSpotId: spot.id });
        setDraft(spot);
      };
      if (!isStroke(spot) && Math.abs(toTarget) <= RIM * pixel) return hit('radius');
      if (toTarget < 0) return hit('target');
      if (spot.id === activeSpotId && toSource < 0) return hit('source');
    }
    // A removal is selected (Delete removes it); it is not moved, since its
    // fill was made for that place. An object removal is hit at its pin.
    for (const removal of refining ? [] : removals) {
      if (removal.kind === 'object') {
        const pin = pinOf(removal.start, removal.end);
        if (Math.hypot(x - pin.x, y - pin.y) <= (PIN + 2) * pixel) {
          setEditor({ activeSpotId: removal.patch.id });
          return;
        }
        continue;
      }
      const distance =
        removal.points.length > 1
          ? distanceToPath(x, y, removal.points)
          : Math.hypot(x - removal.points[0].x, y - removal.points[0].y);
      if (distance < removal.radius) {
        setEditor({ activeSpotId: removal.patch.id });
        return;
      }
    }
    capture();
    // Detect objects: a click selects the object under it; a drag paints a
    // loose stroke over it or draws a box around it. Shift adds to the
    // selected object removal, Alt subtracts from it.
    if (detectObjects) {
      const op: SelectOp = refining ? (e.altKey ? 'subtract' : 'add') : 'new';
      if (objectBrush) {
        drag.current = { kind: 'objectStroke', points: [{ x, y }], op };
        setObjectStroke({ points: [{ x, y }], op });
      } else {
        drag.current = { kind: 'box', start: { x, y }, end: { x, y }, op };
        setBox({ start: { x, y }, end: { x, y } });
      }
      return;
    }
    // Empty photo: a click places a circle, a drag paints a stroke.
    drag.current = { kind: 'paint', points: [{ x, y }] };
    setStroke([{ x, y }]);
  };

  const onPointerMove = (e: PointerEvent<HTMLDivElement>) => {
    const d = drag.current;
    if (!d) return;
    const [x, y] = toImage(e.clientX, e.clientY);
    if (d.kind === 'box') {
      d.end = { x, y };
      setBox({ start: d.start, end: d.end });
    } else if (d.kind === 'objectStroke') {
      const last = d.points[d.points.length - 1];
      if (Math.hypot(x - last.x, y - last.y) >= Math.max(settings.size * 0.2, pixel)) {
        d.points = [...d.points, { x, y }];
        setObjectStroke({ points: d.points, op: d.op });
      }
    } else if (d.kind === 'paint') {
      const last = d.points[d.points.length - 1];
      // A point every fifth of the brush radius (and at least a screen pixel).
      if (Math.hypot(x - last.x, y - last.y) >= Math.max(settings.size * 0.2, pixel)) {
        d.points = [...d.points, { x, y }];
        setStroke(d.points);
      }
    } else if (d.kind === 'radius') {
      setDraft({ ...d.spot, radius: Math.max(2, Math.hypot(x - d.spot.x, y - d.spot.y)) });
    } else if (d.kind === 'target') {
      const dx = x - d.startX;
      const dy = y - d.startY;
      setDraft({
        ...d.spot,
        x: d.spot.x + dx,
        y: d.spot.y + dy,
        sourceX: d.spot.sourceX + (isStroke(d.spot) ? dx : 0),
        sourceY: d.spot.sourceY + (isStroke(d.spot) ? dy : 0),
        ...(d.spot.points && { points: d.spot.points.map((p) => ({ x: p.x + dx, y: p.y + dy })) }),
      });
    } else {
      setDraft({
        ...d.spot,
        sourceX: d.spot.sourceX + x - d.startX,
        sourceY: d.spot.sourceY + y - d.startY,
      });
    }
  };

  const onPointerUp = (e: PointerEvent<HTMLDivElement>) => {
    const d = drag.current;
    drag.current = null;
    try {
      if (e.currentTarget.hasPointerCapture(e.pointerId)) e.currentTarget.releasePointerCapture(e.pointerId);
    } catch {
      // See setPointerCapture.
    }
    if (d?.kind === 'box') {
      setBox(null);
      const { start, end } = d;
      // Travelled less than a few screen pixels: a click on the object.
      if (Math.hypot(end.x - start.x, end.y - start.y) < BOX_MIN * pixel) void removeObject(start, start, d.op);
      else
        void removeObject(
          { x: Math.min(start.x, end.x), y: Math.min(start.y, end.y) },
          { x: Math.max(start.x, end.x), y: Math.max(start.y, end.y) },
          d.op,
        );
      return;
    }
    if (d?.kind === 'objectStroke') {
      setObjectStroke(null);
      const prompt = strokePrompt(d.points, settings.size);
      void removeObject(prompt.start, prompt.end, d.op);
      return;
    }
    if (d?.kind === 'paint') {
      setStroke(null);
      const first = d.points[0];
      const travelled = d.points.reduce(
        (sum, p, i) => (i ? sum + Math.hypot(p.x - d.points[i - 1].x, p.y - d.points[i - 1].y) : 0),
        0,
      );
      // Shorter than half the brush radius: a click.
      if (travelled < settings.size * 0.5) void place(first.x, first.y);
      else void place(first.x, first.y, d.points);
      return;
    }
    if (d && draft && JSON.stringify(draft) !== JSON.stringify(d.spot)) commit(draft);
    setDraft(null);
  };

  // Visualize Spots: the backend's band-pass view of the displayed photo
  // (with its spots), refreshed when the spots or the sensitivity change.
  const spotsKey = JSON.stringify(spots);
  useEffect(() => {
    if (!visualize?.on) {
      setView(null);
      return;
    }
    let url: string | null = null;
    let cancelled = false;
    const timer = window.setTimeout(() => {
      const { adjustments } = useEditorStore.getState();
      invoke<ArrayBuffer>(Invokes.VisualizeSpots, {
        sensitivity: visualize.sensitivity,
        maxEdge: 2400,
        jsAdjustments: adjustments,
      })
        .then((bytes) => {
          if (cancelled) return;
          url = URL.createObjectURL(new Blob([bytes], { type: 'image/jpeg' }));
          setView(url);
        })
        .catch((error) => console.error('Visualize spots failed:', error));
    }, 120);
    return () => {
      cancelled = true;
      window.clearTimeout(timer);
      if (url) URL.revokeObjectURL(url);
    };
  }, [visualize?.on, visualize?.sensitivity, spotsKey]);

  // Progress of the running removal: the model's download (any file of it
  // while the removal waits) and the network's runs.
  useEffect(() => {
    const unlisten = [
      listen<{ taskId: string | null; done: number; total: number }>('ai-fill-progress', ({ payload }) => {
        if (payload.taskId && filling.current.includes(payload.taskId))
          report({ taskId: payload.taskId, done: payload.done, total: payload.total });
      }),
      listen<{ received: number; total: number; batchReceived?: number; batchTotal?: number }>(
        'ai-model-download-progress',
        ({ payload }) => {
          // A download serves the newest removal waiting for it; a model of
          // several files reports its progress over all of them.
          const id = filling.current.at(-1);
          const received = payload.batchReceived ?? payload.received;
          const total = payload.batchTotal ?? payload.total;
          if (id && total > 0) report({ taskId: id, download: Math.min(100, Math.floor((received / total) * 100)) });
        },
      ),
      // Moebius could not be used for a large hole: MI-GAN filled it, and
      // the user is told why the fill is weaker.
      // The GPU took too long per network run: large holes use the fast
      // method for the rest of the session (inpainting::MOEBIUS_TOO_SLOW).
      listen<{ taskId: string | null; reason: 'download' | 'unavailable' | 'tooSlow' }>('ai-fill-notice', ({ payload }) => {
        if (payload.reason === 'tooSlow') {
          toast.info(t('spotRemoval.largeHoleTooSlow'));
          return;
        }
        if (!payload.taskId || !filling.current.includes(payload.taskId)) return;
        toast.warn(
          t(payload.reason === 'download' ? 'spotRemoval.fallbackDownload' : 'spotRemoval.fallbackUnavailable'),
          { toastId: `ai-fill-notice-${payload.reason}` },
        );
      }),
    ].map((p) => p.catch(() => null));
    return () => unlisten.forEach((p) => p.then((stop) => stop?.()));
  }, [t]);

  // Delete removes the selected spot; [ and ] resize it (or the next one);
  // Escape cancels the removals being filled, else deselects.
  useEffect(() => {
    const onKeyDown = (e: KeyboardEvent) => {
      const target = e.target as HTMLElement | null;
      if (target && (target.tagName === 'INPUT' || target.tagName === 'TEXTAREA' || target.isContentEditable)) return;
      const { activeSpotId: id, spotSettings, setEditor, adjustments } = useEditorStore.getState();
      if ((e.key === 'Delete' || e.key === 'Backspace') && id) {
        e.preventDefault();
        e.stopImmediatePropagation();
        updateAdjustments((prev) => ({
          ...prev,
          spots: (prev.spots ?? []).filter((s) => s.id !== id),
          aiPatches: (prev.aiPatches ?? []).filter((p) => p.id !== id),
        }));
        setEditor({ activeSpotId: null });
      } else if (e.key === '[' || e.key === ']') {
        e.preventDefault();
        e.stopImmediatePropagation();
        const factor = e.key === ']' ? 1.15 : 1 / 1.15;
        const spot = adjustments.spots?.find((s) => s.id === id);
        if (spot) commit({ ...spot, radius: Math.max(2, spot.radius * factor) });
        else setEditor({ spotSettings: { ...spotSettings, size: Math.max(2, Math.round(spotSettings.size * factor)) } });
      } else if (e.key === 'Escape' && filling.current.length) {
        e.preventDefault();
        e.stopImmediatePropagation();
        filling.current.forEach(cancelFill);
      } else if (e.key === 'Escape' && id) {
        e.preventDefault();
        e.stopImmediatePropagation();
        setEditor({ activeSpotId: null });
      }
    };
    window.addEventListener('keydown', onKeyDown, true);
    return () => window.removeEventListener('keydown', onKeyDown, true);
  }, []);

  const line = 1.5 * pixel * scale;
  const circle = (key: string, x: number, y: number, r: number, cls: string, dashed = false) => {
    const [bx, by] = toBox(x, y);
    return (
      <g key={key} className={cls}>
        <circle cx={bx} cy={by} r={r * scale} fill="none" stroke="rgba(0,0,0,0.6)" strokeWidth={line * 2.4} />
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

  // A box drawn around an object, in mask pixels.
  const rectangle = (key: string, a: Point, b: Point, dashed: boolean) => {
    const [ax, ay] = toBox(Math.min(a.x, b.x), Math.min(a.y, b.y));
    const [bx, by] = toBox(Math.max(a.x, b.x), Math.max(a.y, b.y));
    return (
      <g key={key} className="spot-object-box" fill="none">
        <rect x={ax} y={ay} width={bx - ax} height={by - ay} stroke="rgba(0,0,0,0.6)" strokeWidth={line * 2.4} />
        <rect
          x={ax}
          y={ay}
          width={bx - ax}
          height={by - ay}
          stroke="currentColor"
          strokeWidth={line}
          strokeDasharray={dashed ? `${4 * line} ${3 * line}` : undefined}
        />
      </g>
    );
  };

  // The pin of an object removal (Lightroom marks removals with pins).
  const pin = (key: string, at: Point, active: boolean) => {
    const [px, py] = toBox(at.x, at.y);
    const r = PIN * pixel * scale;
    return (
      <g key={key} className="spot-object-pin">
        <circle cx={px} cy={py} r={r} fill="rgba(0,0,0,0.45)" stroke="rgba(0,0,0,0.6)" strokeWidth={line * 2.4} />
        <circle cx={px} cy={py} r={r} fill="none" stroke="currentColor" strokeWidth={line} />
        <circle cx={px} cy={py} r={r * (active ? 0.45 : 0.3)} fill="currentColor" />
      </g>
    );
  };

  // A brush stroke: its area as a translucent band with an outline.
  const strokeArea = (key: string, points: Point[], r: number, cls: string, dx = 0, dy = 0, dashed = false) => {
    const d = points
      .map((p, i) => {
        const [bx, by] = toBox(p.x + dx, p.y + dy);
        return `${i ? 'L' : 'M'}${bx} ${by}`;
      })
      .join(' ');
    const width = 2 * r * scale;
    return (
      <g key={key} className={cls} strokeLinecap="round" strokeLinejoin="round" fill="none">
        <path d={d} stroke="rgba(0,0,0,0.6)" strokeWidth={width + line * 2.4} strokeOpacity={dashed ? 0.25 : 0.5} />
        <path d={d} stroke="currentColor" strokeWidth={width} strokeOpacity={dashed ? 0.12 : 0.28} />
        <path
          d={d}
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
      aria-label={
        detectObjects
          ? objectBrush
            ? 'Remove objects: click an object or brush over it'
            : 'Remove objects: click an object or drag a box around it'
          : 'Spot removal: click a spot or drag a stroke to heal it'
      }
      data-testid="spot-overlay"
      style={{
        position: 'absolute',
        left: rect.left,
        top: rect.top,
        width: rect.width,
        height: rect.height,
        zIndex: 6,
        cursor: 'crosshair',
        touchAction: 'none',
      }}
      onPointerDown={onPointerDown}
      onPointerMove={onPointerMove}
      onPointerUp={onPointerUp}
      onPointerCancel={onPointerUp}
    >
      {view && (
        <img
          src={view}
          alt=""
          aria-hidden
          draggable={false}
          style={{ position: 'absolute', inset: 0, width: '100%', height: '100%', pointerEvents: 'none' }}
        />
      )}
      <svg
        width={rect.width}
        height={rect.height}
        style={{ position: 'absolute', inset: 0, overflow: 'visible', pointerEvents: 'none' }}
      >
        {shown.map((spot) => {
          const active = spot.id === activeSpotId;
          if (isStroke(spot)) {
            const dx = spot.sourceX - spot.x;
            const dy = spot.sourceY - spot.y;
            return (
              <g key={spot.id} style={{ color: active ? 'var(--studio-accent, #9cc2e0)' : '#fff' }}>
                {strokeArea(`${spot.id}-t`, spot.points, spot.radius, 'spot-target')}
                {active && strokeArea(`${spot.id}-s`, spot.points, spot.radius, 'spot-source', dx, dy, true)}
              </g>
            );
          }
          const [tx, ty] = toBox(spot.x, spot.y);
          const [sx, sy] = toBox(spot.sourceX, spot.sourceY);
          const length = Math.hypot(sx - tx, sy - ty);
          const r = spot.radius * scale;
          // The connecting line runs between the two rims.
          const ux = length > 0 ? (sx - tx) / length : 0;
          const uy = length > 0 ? (sy - ty) / length : 0;
          return (
            <g key={spot.id} style={{ color: active ? 'var(--studio-accent, #9cc2e0)' : '#fff' }}>
              {active && length > 2 * r && (
                <line
                  x1={tx + ux * r}
                  y1={ty + uy * r}
                  x2={sx - ux * r}
                  y2={sy - uy * r}
                  stroke="currentColor"
                  strokeWidth={line}
                />
              )}
              {circle(`${spot.id}-t`, spot.x, spot.y, spot.radius, 'spot-target')}
              {active && circle(`${spot.id}-s`, spot.sourceX, spot.sourceY, spot.radius, 'spot-source', true)}
            </g>
          );
        })}
        {removals.map((removal) => {
          const { patch } = removal;
          const active = patch.id === activeSpotId;
          const color = patch.isLoading ? '#f0c060' : active ? 'var(--studio-accent, #9cc2e0)' : '#fff';
          if (removal.kind === 'object') {
            const { start, end } = removal;
            return (
              <g key={patch.id} style={{ color }} data-testid="remove-object">
                {active && !isClick(start, end) && rectangle(`${patch.id}-b`, start, end, true)}
                {pin(`${patch.id}-p`, pinOf(start, end), active)}
              </g>
            );
          }
          const { points, radius } = removal;
          return (
            <g key={patch.id} style={{ color }} data-testid="remove-area">
              {points.length > 1
                ? strokeArea(`${patch.id}-r`, points, radius, 'spot-remove', 0, 0, true)
                : circle(`${patch.id}-r`, points[0].x, points[0].y, radius, 'spot-remove', true)}
            </g>
          );
        })}
        {box && <g style={{ color: '#fff' }}>{rectangle('drawing-box', box.start, box.end, false)}</g>}
        {objectStroke && objectStroke.points.length > 1 && (
          <g style={{ color: OP_COLOUR[objectStroke.op] }} data-testid="remove-object-stroke">
            {strokeArea('object-stroke', objectStroke.points, settings.size, 'spot-painting')}
          </g>
        )}
        {pendingObject && (
          <g style={{ color: OP_COLOUR[pendingObject.op] }} data-testid="remove-object-pending">
            {isClick(pendingObject.start, pendingObject.end)
              ? pin('pending-object', pendingObject.start, false)
              : rectangle('pending-object', pendingObject.start, pendingObject.end, true)}
          </g>
        )}
        {stroke && stroke.length > 1 && (
          <g style={{ color: '#fff' }}>{strokeArea('painting', stroke, settings.size, 'spot-painting')}</g>
        )}
        {pending && (
          <g style={{ color: '#fff' }}>
            {pending.points
              ? strokeArea('pending', pending.points, pending.radius, 'spot-pending', 0, 0, true)
              : circle('pending', pending.x, pending.y, pending.radius, 'spot-pending', true)}
          </g>
        )}
      </svg>
      {progress && (
        <div
          data-testid="remove-progress"
          role="status"
          onPointerDown={(e) => e.stopPropagation()}
          style={{
            position: 'absolute',
            left: '50%',
            bottom: 16,
            transform: 'translateX(-50%)',
            display: 'flex',
            alignItems: 'center',
            gap: 10,
            padding: '6px 8px 6px 12px',
            borderRadius: 6,
            background: 'rgba(20, 20, 20, 0.85)',
            color: '#eee',
            fontSize: 12,
            cursor: 'default',
            whiteSpace: 'nowrap',
          }}
        >
          <span>
            {progress.download !== undefined
              ? t('spotRemoval.downloadingModel', { percent: progress.download })
              : t('spotRemoval.filling', { done: progress.done ?? 0, total: progress.total ?? 0 })}
          </span>
          <button
            type="button"
            onClick={() => cancelFill(progress.taskId)}
            style={{
              padding: '2px 8px',
              borderRadius: 4,
              border: '1px solid rgba(255,255,255,0.3)',
              background: 'transparent',
              color: 'inherit',
              cursor: 'pointer',
            }}
          >
            {t('spotRemoval.cancel')}
          </button>
        </div>
      )}
    </div>
  );
}
