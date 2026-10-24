import { useState, useEffect, useRef, useCallback, memo, useMemo, type MutableRefObject } from 'react';
import { useCropGestures, useToolGesture } from '../../../utils/cropGesture';
import ReactCrop from 'react-image-crop';
import 'react-image-crop/dist/ReactCrop.css';
import { Stage, Layer, Ellipse, Line, Group, Circle, Rect } from 'react-konva';
import type Konva from 'konva';
import type { KonvaEventObject } from 'konva/lib/Node';
import { PercentCrop, Crop } from 'react-image-crop';
import { invoke } from '@tauri-apps/api/core';
import { Adjustments, Coord, MaskContainer, GuideLine, GuideOrientation } from '../../../utils/adjustments';
import type { LiveStroke } from '../../../utils/liveMaskRender';
import { AUTO_MASK_VERSION } from '../../../utils/autoMaskUpgrade';
import { autoMaskPrewarmPath } from '../../../utils/autoMaskPrewarm';
import {
  pointPressure,
  pressureChanged,
  pressureKeys,
  pressureMapping,
  withPressure,
  type StrokePoint,
} from '../../../utils/penPressure';
import { Mask, SubMask, SubMaskMode, ToolType } from '../right/Masks';
import { AppSettings, BrushSettings, SelectedImage } from '../../ui/AppProperties';
import { RenderSize } from '../../../hooks/useImageRenderSize';
import { useTranslation } from 'react-i18next';
import { useEditorStore } from '../../../store/useEditorStore';
import type { OverlayMode } from './overlays/CompositionOverlays';
import CompositionOverlays from './overlays/CompositionOverlays';
import { calculateStraightenAngle } from '../../../utils/cropUtils';
import { pickColorRange } from '../../../utils/colorRanges';
import { sampleLumaRange } from '../../../utils/lumaSample';
import TargetedAdjustmentOverlay from './TargetedAdjustmentOverlay';
import RelightOverlay from './RelightOverlay';
import { useRelightStore } from '../../../store/useRelightStore';
import SpotOverlay from './SpotOverlay';
import RedEyeOverlay from './RedEyeOverlay';
import FaceRetouchOverlay from './FaceRetouchOverlay';
import BrowserSplitImage from './BrowserSplitImage';
import { clearDraftPatch, registerDraftPatchHost } from '../../../utils/browserDraftPatch';
import { useDecodedPreview } from '../../../hooks/useDecodedPreview';
import { toast } from 'react-toastify';
import {
  notePointerSessionStroke,
  reportImagePlaceholderLoad,
  reportMaskOverlayLoad,
  reportBrowserPreviewLoad,
} from '../../../hooks/usePerformanceDiagnostics';
import { whiteBalanceForCurrentPreview } from '../../../utils/whiteBalance';
import { useBrushGesture } from './brushGesture';
import { isCropEdge, resizeCropFromEdge, type CropRect } from './cropEdges';
import { DEFAULT_BRUSH_SIZE } from '../../../utils/brushSize';
import { GUIDE_COLORS, guideOrientation, guideRejection } from '../../../utils/guideLines';
import { useContextMenu } from '../../../context/ContextMenuContext';
import { Invokes, OPTION_SEPARATOR, canRenderSource } from '../../ui/AppProperties';
import { DEFAULT_COLOR_RANGE } from '../../../utils/maskKinds';
import {
  FrameGeometry,
  ShapeFrame, SizedFrame,
  frameGeometry,
  reframePoint,
  sameFrameGeometry,
  shapeAsShown,
  shapeFrame,
} from '../../../utils/maskGeometry';
import {
  drawsMaskOverlayNatively,
  maskDisplayOf,
  maskOverlayOpacity,
  nativeMaskOverlay,
  sameNativeMaskOverlay,
} from '../../../utils/maskDisplay';
import {
  dragRadialHandle,
  dragRadialLine,
  radialHandlePositions,
  radialLines,
  toLocal,
  type RadialHandle,
  type RadialLine,
  type RadialParams,
} from '../../../utils/radialMask';
import {
  dragLinearLine,
  linearFrame,
  linearLineOffsets,
  rotateLinear,
  type LinearLine,
  type LinearParams,
} from '../../../utils/linearMask';
import {
  draftToCommit,
  restoreAfterShapeDrag,
  shapeFromDrag,
  type ShapeDraft,
  type ShapeDragState,
} from '../../../utils/shapeDraw';
import { requestLiveMaskRender } from '../../../utils/liveMaskRender';
import {
  nextPrompts,
  promptBounds,
  promptGesture,
  promptOf,
  storedPrompts,
  type ObjectPrompt,
  type PromptGesture,
} from '../../../utils/objectPrompts';
import { pendingObjectPrompts } from '../../../hooks/useAiMasking';

/** Screen pixels outside the crop frame where a drag rotates the photo. */
const CROP_ROTATE_BAND = 36;

interface CursorPreview {
  visible: boolean;
  x: number;
  y: number;
}

interface DrawnLine {
  brushSize: number;
  feather?: number;
  /** Percent. A brush stroke's opacity and flow (Capture One); a Flow mask
   * line's flow is its per-stroke strength. */
  opacity?: number;
  flow?: number;
  /** Percent; Capture One's tip, used instead of `feather` when present. */
  hardness?: number;
  /** Auto Mask: keeps to colours like the one under the brush centre. */
  autoMask?: boolean;
  /** Auto Mask method; absent = 1 (old strokes, Lightroom imports), 2 = edge-aware. */
  autoMaskVersion?: number;
  /** Dab placement; absent = 1 (old strokes), 2 = a dab at the last point and,
   * for Capture One tips without Auto Mask, denser dabs (crates/engine
   * `DabTip::dab_version_2`). */
  dabVersion?: number;
  /** Pen pressure (points' `p`) scales the dab size / flow; only pen strokes
   * carry them (crates/engine `PenPressure`). */
  pressureSize?: true;
  pressureFlow?: true;
  /** The photo's geometry the stroke was painted in (see `ShapeFrame`). */
  frame?: ShapeFrame;
  points: Array<StrokePoint>;
  tool: ToolType;
}

/** The native event Konva passes to pointer handlers: a mouse event (drags too)
 * or a touch event, each without the other's `button` / `touches`. */
type NativePointerEvent = (MouseEvent & { touches?: undefined }) | (TouchEvent & { button?: undefined });
type CanvasPointerEvent = KonvaEventObject<NativePointerEvent>;

/** handleMove also continues a stroke outside the stage with the window's own
 * mouse/touch events (onGlobalMove) and tells them from Konva's at runtime; the
 * `?: undefined` fields mark what each kind of event lacks. */
type DomEventTarget = (EventTarget & { getStage?: undefined }) | null;
type CanvasMoveEvent =
  | (CanvasPointerEvent & { clientX?: undefined; touches?: undefined; shiftKey?: undefined })
  | (MouseEvent & { evt?: undefined; touches?: undefined; target: DomEventTarget })
  | (TouchEvent & { evt?: undefined; clientX?: undefined; target: DomEventTarget });

/** What the canvas previews live: the active mask layer. */
export type LiveMaskDefinition = MaskContainer;

/** Result of `calculate_guided_perspective` (`GuidedResultJson` in guided_perspective.rs). */
interface GuidedPerspectiveResult {
  forwardH: number[];
  /** The snake_case spelling, accepted as well. */
  forward_h?: number[];
  crop: [number, number, number, number];
  valid: boolean;
  debugVp: [[number, number] | null, [number, number] | null];
}

/** Modifier state the key listeners below mirror on `window`. */
type ModifierKeyWindow = Window & { altKeyDown?: boolean };

interface ImageCanvasProps {
  appSettings: AppSettings | null;
  activeMaskContainerId: string | null;
  activeMaskId: string | null;
  adjustments: Adjustments;
  brushSettings: BrushSettings | null;
  crop: Crop | null;
  finalPreviewUrl: string | null;
  handleCropComplete(c: Crop, cp: PercentCrop): void;
  imageRenderSize: RenderSize;
  isCropping: boolean;
  isMaskControlHovered: boolean;
  isMasking: boolean;
  isSliderDragging: boolean;
  isStraightenActive: boolean;
  isRotationActive?: boolean;
  maskOverlayUrl: string | null;
  /** An Object selection from its prompts (`start`/`end`: the first
   * included one's box or click); settles when its mask is in. */
  onGenerateAiMask(id: string | null, start: Coord, end: Coord, prompts?: ObjectPrompt[]): unknown;
  onLiveMaskPreview?: (
    previewMaskDef: LiveMaskDefinition,
    options?: { renderLive?: boolean; stroke?: Omit<LiveStroke, 'maskId'> },
  ) => void;
  onSelectMask(id: string | null): void;
  onStraighten(val: number): void;
  selectedImage: SelectedImage;
  setCrop(crop: Crop, perfentCrop: PercentCrop): void;
  setIsMaskHovered(isHovered: boolean): void;
  setIsMaskTouchInteracting(isInteracting: boolean): void;
  showOriginal: boolean;
  uncroppedAdjustedPreviewUrl: string | null;
  updateSubMask(id: string | null, subMask: Partial<SubMask>): void;
  interactivePatch?: { url: string; normX: number; normY: number; normW: number; normH: number } | null;
  isWbPickerActive?: boolean;
  onWbPicked?: () => void;
  setAdjustments(fn: (prev: Adjustments) => Adjustments): void;
  overlayMode?: OverlayMode;
  overlayRotation?: number;
  cursorStyle: string;
  isMaxZoom?: boolean;
  liveRotation?: number | null;
  transformState: { scale: number; positionX: number; positionY: number };
  hasRenderedFirstFrame: boolean;
  /** Filled with a test for whether a screen point presses a mask handle. */
  maskHitTestRef?: MutableRefObject<((clientX: number, clientY: number) => boolean) | null>;
}

interface MaskOverlayProps {
  adjustments: Adjustments;
  imageHeight: number;
  imageWidth: number;
  onMaskInteractionEnd(): void;
  onMaskInteractionStart(event?: KonvaEventObject<Event>): void;
  isToolActive: boolean;
  isSelected: boolean;
  /** A new gradient waiting for (or following) its first drag. */
  isBeingDrawn?: boolean;
  onMaskMouseEnter(): void;
  onMaskMouseLeave(): void;
  onPreviewUpdate?(id: string, subMask: Partial<SubMask>): void;
  /** False when moves render live via onPreviewUpdate and only the release commits. */
  commitWhileDragging?: boolean;
  onSelect(): void;
  onUpdate(id: string, subMask: Partial<SubMask>): void;
  scale: number;
  subMask: SubMask;
  offsetX: number;
  offsetY: number;
  stageScale: number;
  /** The colour-range area being dragged, in image coordinates. */
  sampleDraft?: { x0: number; y0: number; x1: number; y1: number } | null;
  /** The photo's geometry now: drawn masks made in another are shown, and
   * edited, where their spot of the photo is now. */
  drawingFrame?: SizedFrame;
}

/** A colour-range sample: a point, or an area with a size, on the photo as
 * shown when it was taken (full-resolution pixels before the crop), with that
 * geometry, so it keeps its spot when the photo is turned later. Older samples
 * have no geometry of their own: the component's applies. */
interface ColorSample extends Partial<FrameGeometry> {
  x: number;
  y: number;
  width?: number;
  height?: number;
}

/** Lightroom's colour range takes up to five samples. */
const MAX_COLOR_SAMPLES = 5;

/** Dab placement of new strokes: 2 ends a stroke at its last point (and the
 * live stroke at the pointer) and lays plain Capture One tips' dabs closer;
 * strokes without it keep version 1. */
const DAB_VERSION = 2;

const TRANSPARENT_PIXEL = 'data:image/gif;base64,R0lGODlhAQABAIAAAAAAAP///yH5BAEAAAAALAAAAAABAAEAAAIBRAA7';
const IDENTITY_3X3 = [1, 0, 0, 0, 1, 0, 0, 0, 1];

function multiply3x3(a: number[], b: number[]): number[] {
  if (!a || !b) return IDENTITY_3X3;
  const out = [0, 0, 0, 0, 0, 0, 0, 0, 0];
  for (let i = 0; i < 3; i++) {
    for (let j = 0; j < 3; j++) {
      out[i * 3 + j] = a[i * 3 + 0] * b[0 * 3 + j] + a[i * 3 + 1] * b[1 * 3 + j] + a[i * 3 + 2] * b[2 * 3 + j];
    }
  }
  return out;
}

function invert3x3(h: number[]): number[] {
  if (!h) return IDENTITY_3X3;
  const a = h[0],
    b = h[1],
    c = h[2],
    d = h[3],
    e = h[4],
    f = h[5],
    g = h[6],
    hh = h[7],
    i = h[8];
  const A = e * i - f * hh,
    B = f * g - d * i,
    C = d * hh - e * g;
  const D = c * hh - b * i,
    E = a * i - c * g,
    F = b * g - a * hh;
  const G = b * f - c * e,
    H = c * d - a * f,
    I = a * e - b * d;
  const det = a * A + b * B + c * C;
  if (Math.abs(det) < 1e-15) return IDENTITY_3X3;
  const inv = 1.0 / det;
  return [A * inv, D * inv, G * inv, B * inv, E * inv, H * inv, C * inv, F * inv, I * inv];
}

function project3x3(h: number[], x: number, y: number): { x: number; y: number } {
  if (!h) return { x, y };
  const W = h[6] * x + h[7] * y + h[8];
  if (Math.abs(W) < 1e-12) return { x, y };
  return {
    x: (h[0] * x + h[1] * y + h[2]) / W,
    y: (h[3] * x + h[4] * y + h[5]) / W,
  };
}

function orientPoint(x: number, y: number, w: number, h: number, steps: number) {
  const s = ((steps % 4) + 4) % 4;
  if (s === 0) return { x, y };
  if (s === 1) return { x: h - y, y: x };
  if (s === 2) return { x: w - x, y: h - y };
  return { x: y, y: w - x };
}

function unorientPoint(x: number, y: number, w: number, h: number, steps: number) {
  const s = ((steps % 4) + 4) % 4;
  const inv = (4 - s) % 4;
  return orientPoint(x, y, w, h, inv);
}

const getEdgeFadeStyle = (fadeDistancePx: number = 128): React.CSSProperties => ({
  WebkitMaskImage: `
    linear-gradient(to right, transparent, black ${fadeDistancePx}px, black calc(100% - ${fadeDistancePx}px), transparent),
    linear-gradient(to bottom, transparent, black ${fadeDistancePx}px, black calc(100% - ${fadeDistancePx}px), transparent)
  `,
  WebkitMaskComposite: 'source-in',
  maskImage: `
    linear-gradient(to right, transparent, black ${fadeDistancePx}px, black calc(100% - ${fadeDistancePx}px), transparent),
    linear-gradient(to bottom, transparent, black ${fadeDistancePx}px, black calc(100% - ${fadeDistancePx}px), transparent)
  `,
  maskComposite: 'intersect',
});

const OptimizedBrushLine = memo(
  ({ line, scale, cropX, cropY }: { line: DrawnLine; scale: number; cropX: number; cropY: number }) => {
    const flattenedPoints = useMemo(() => {
      const pts = new Float32Array(line.points.length * 2);
      for (let i = 0; i < line.points.length; i++) {
        pts[i * 2] = (line.points[i].x - cropX) * scale;
        pts[i * 2 + 1] = (line.points[i].y - cropY) * scale;
      }
      return Array.from(pts);
    }, [line.points, scale, cropX, cropY]);

    return (
      <Line
        hitStrokeWidth={line.brushSize * scale}
        lineCap="round"
        lineJoin="round"
        points={flattenedPoints}
        stroke="transparent"
        strokeScaleEnabled={false}
        perfectDrawEnabled={false}
        shadowForStrokeEnabled={false}
      />
    );
  },
);

// Painted masks have no handles; pressing their strokes may still pan, as
// before. Shape and point masks are moved by their handles.
const HANDLELESS_MASKS = new Set<string>([Mask.Brush, Mask.Flow]);

const MaskOverlay = memo(
  ({
    adjustments,
    imageHeight,
    imageWidth,
    onMaskInteractionEnd,
    onMaskInteractionStart,
    isToolActive,
    isSelected,
    isBeingDrawn = false,
    onMaskMouseEnter,
    onMaskMouseLeave,
    onPreviewUpdate,
    commitWhileDragging = true,
    onSelect,
    onUpdate,
    scale,
    subMask,
    offsetX,
    offsetY,
    stageScale,
    sampleDraft = null,
    drawingFrame,
  }: MaskOverlayProps) => {
    const rotateStartRef = useRef<{ angle: number; rotation: number } | null>(null);

    const crop = adjustments.crop;
    const isPercent = crop?.unit === '%';
    const cropX = crop ? (isPercent ? (crop.x / 100) * imageWidth : crop.x) : 0;
    const cropY = crop ? (isPercent ? (crop.y / 100) * imageHeight : crop.y) : 0;
    const cropW = crop ? (isPercent ? (crop.width / 100) * imageWidth : crop.width) : imageWidth;
    const cropH = crop ? (isPercent ? (crop.height / 100) * imageHeight : crop.height) : imageHeight;

    // A radial or linear drawn while the photo was turned otherwise is shown
    // where its spot of the photo is now, and edited there: the first drag
    // stores it in the geometry now (with that frame). Strokes are shown
    // there too and stay stored as painted.
    const shownParameters = useMemo(
      () => shapeAsShown(subMask.type, subMask.parameters, drawingFrame),
      [subMask.type, subMask.parameters, drawingFrame],
    );
    const [p, setP] = useState(shownParameters);
    const pRef = useRef(p);
    const isDragging = useRef(false);

    const dragStartPointer = useRef<Coord | null>(null);
    const dragStartParams = useRef<SubMask['parameters'] | null>(null);

    const getPointer = useCallback(
      // Shape events always come from a node on a stage.
      (stage: Konva.Stage | null) => {
        const pos = stage!.getPointerPosition();
        if (!pos) return null;
        return { x: pos.x / stageScale - offsetX, y: pos.y / stageScale - offsetY };
      },
      [offsetX, offsetY, stageScale],
    );

    useEffect(() => {
      if (!isDragging.current) {
        setP(shownParameters);
        pRef.current = shownParameters;
      }
    }, [shownParameters]);

    const updateP = useCallback((newP: SubMask['parameters']) => {
      setP(newP);
      pRef.current = newP;
    }, []);

    const handleMaskTouchStart = useCallback(
      (e: CanvasPointerEvent) => {
        if (e.evt && typeof e.evt.button === 'number' && e.evt.button !== 0) return;

        onMaskInteractionStart(e);
        if (e.evt.cancelable) e.evt.preventDefault();
        e.evt.stopPropagation?.();
      },
      [onMaskInteractionStart],
    );

    const handleMaskTouchEnd = useCallback(() => {
      onMaskInteractionEnd();
    }, [onMaskInteractionEnd]);

    const handleSelect = isToolActive ? undefined : onSelect;

    const lockDragBoundFunc = useCallback(function (this: Konva.Node) {
      return this.getAbsolutePosition();
    }, []);

    const handleRadialDragStart = useCallback(
      (e: KonvaEventObject<DragEvent>) => {
        if (e.evt && typeof e.evt.button === 'number' && e.evt.button !== 0) return;
        isDragging.current = true;
        onMaskInteractionStart(e);
        dragStartPointer.current = getPointer(e.target.getStage());
        dragStartParams.current = { ...pRef.current };
      },
      [onMaskInteractionStart, getPointer],
    );

    const handleRadialDragMove = useCallback(
      (e: KonvaEventObject<DragEvent>) => {
        const pointerPos = getPointer(e.target.getStage());
        if (!pointerPos || !dragStartPointer.current || !dragStartParams.current) return;

        const dx = (pointerPos.x - dragStartPointer.current.x) / scale;
        const dy = (pointerPos.y - dragStartPointer.current.y) / scale;

        const newP = {
          ...dragStartParams.current,
          centerX: (dragStartParams.current as RadialParams).centerX + dx,
          centerY: (dragStartParams.current as RadialParams).centerY + dy,
        };

        updateP(newP);
        if (onPreviewUpdate) onPreviewUpdate(subMask.id, { parameters: newP });

        if (commitWhileDragging) onUpdate(subMask.id, { parameters: newP });
      },
      [scale, updateP, onPreviewUpdate, subMask.id, getPointer, onUpdate, commitWhileDragging],
    );

    const handleRadialDragEnd = useCallback(() => {
      isDragging.current = false;
      onMaskInteractionEnd();
      onUpdate(subMask.id, { parameters: pRef.current });
    }, [subMask.id, onMaskInteractionEnd, onUpdate]);

    // Capture One-style radial editing. The inner (100%) and outer (0%) lines
    // set the feathering, with Shift around the fixed central line; the four
    // handles on the central (50%) line set the shape, Alt keeps the opposite
    // handle and Shift scales the whole mask. Each move is computed from the
    // state at the start of the drag, so modifiers can change mid-drag.
    const radialEditStart = useRef<RadialParams | null>(null);
    const handleRadialEditStart = useCallback(
      (e: KonvaEventObject<DragEvent>) => {
        if (e.evt && typeof e.evt.button === 'number' && e.evt.button !== 0) return;
        isDragging.current = true;
        radialEditStart.current = { ...pRef.current } as RadialParams;
        onMaskInteractionStart(e);
        e.cancelBubble = true;
      },
      [onMaskInteractionStart],
    );

    const handleRadialEditMove = useCallback(
      (e: KonvaEventObject<DragEvent>, target: RadialLine | RadialHandle) => {
        const start = radialEditStart.current;
        const pointer = getPointer(e.target.getStage());
        if (!start || !pointer) return;
        const local = toLocal(start, pointer.x / scale + cropX, pointer.y / scale + cropY);
        const shift = !!e.evt?.shiftKey;
        const newP =
          target === 'inner' || target === 'outer'
            ? dragRadialLine(start, target, local, shift)
            : dragRadialHandle(start, target, local, { uniform: shift, lockOpposite: !!e.evt?.altKey });
        updateP(newP);
        if (onPreviewUpdate) onPreviewUpdate(subMask.id, { parameters: newP });
        if (commitWhileDragging) onUpdate(subMask.id, { parameters: newP });
      },
      [getPointer, scale, cropX, cropY, updateP, onPreviewUpdate, subMask.id, commitWhileDragging, onUpdate],
    );

    // Capture One-style linear editing: the start (100%) and end (0%) lines
    // move with the opposite line fixed, Alt changes only the dragged zone and
    // Shift resizes both around the central line; the central line rotates
    // (Shift: 45° steps). Moves are computed from the state at drag start.
    const linearEditStart = useRef<{ params: LinearParams; pointer: Coord; pivot: Coord } | null>(null);
    const imagePointer = useCallback(
      (stage: Konva.Stage | null) => {
        const pointer = stage ? getPointer(stage) : null;
        return pointer ? { x: pointer.x / scale + cropX, y: pointer.y / scale + cropY } : null;
      },
      [getPointer, scale, cropX, cropY],
    );

    const handleLinearEditStart = useCallback(
      (e: KonvaEventObject<DragEvent>) => {
        if (e.evt && typeof e.evt.button === 'number' && e.evt.button !== 0) return;
        const pointer = imagePointer(e.target.getStage());
        if (!pointer) return;
        const params = { ...pRef.current } as LinearParams;
        isDragging.current = true;
        linearEditStart.current = {
          params,
          pointer,
          pivot: { x: (params.startX + params.endX) / 2, y: (params.startY + params.endY) / 2 },
        };
        onMaskInteractionStart(e);
        e.cancelBubble = true;
      },
      [imagePointer, onMaskInteractionStart],
    );

    const handleLinearEditMove = useCallback(
      (e: KonvaEventObject<DragEvent>, target: LinearLine | 'central') => {
        const start = linearEditStart.current;
        const pointer = imagePointer(e.target.getStage());
        if (!start || !pointer) return;
        const newP =
          target === 'central'
            ? rotateLinear(start.params, start.pivot, start.pointer, pointer, !!e.evt?.shiftKey)
            : dragLinearLine(
                start.params,
                target,
                pointer,
                e.evt?.altKey ? 'independent' : e.evt?.shiftKey ? 'symmetric' : 'opposite',
              );
        updateP(newP);
        if (onPreviewUpdate) onPreviewUpdate(subMask.id, { parameters: newP });
        if (commitWhileDragging) onUpdate(subMask.id, { parameters: newP });
      },
      [imagePointer, updateP, onPreviewUpdate, subMask.id, commitWhileDragging, onUpdate],
    );

    const handleShapeEditEnd = useCallback(
      (e?: KonvaEventObject<DragEvent>) => {
        isDragging.current = false;
        radialEditStart.current = null;
        linearEditStart.current = null;
        if (e) e.cancelBubble = true;
        onMaskInteractionEnd();
        onUpdate(subMask.id, { parameters: pRef.current });
      },
      [subMask.id, onMaskInteractionEnd, onUpdate],
    );

    const setRotateCursor = useCallback(
      (stage: Konva.Stage | null, pointerPos: Coord, pivot?: { x: number; y: number }) => {
        const cx = pivot ? pivot.x : ((pRef.current as RadialParams).centerX - cropX) * scale;
        const cy = pivot ? pivot.y : ((pRef.current as RadialParams).centerY - cropY) * scale;
        const angle = Math.atan2(pointerPos.y - cy, pointerPos.x - cx) * (180 / Math.PI);

        const svgStr = `<svg xmlns="http://www.w3.org/2000/svg" width="32" height="32" viewBox="0 0 32 32" fill="none" stroke="white" stroke-width="1.5" stroke-linecap="round" stroke-linejoin="round" style="filter: drop-shadow(0px 1px 2px rgba(0,0,0,0.8));">
          <g transform="rotate(${Math.round(angle)} 16 16)">
            <path d="M 23 9 A 10 10 0 0 1 23 23" />
            <path d="M 28 9 L 23 9 L 23 14" />
            <path d="M 28 23 L 23 23 L 23 18" />
          </g>
        </svg>`;
        const encodedSvg = encodeURIComponent(svgStr);
        stage!.container().style.cursor = `url('data:image/svg+xml;utf8,${encodedSvg}') 16 16, crosshair`;
      },
      [cropX, cropY, scale],
    );

    const handleRotateStart = useCallback(
      (e: CanvasPointerEvent) => {
        if (e.evt && typeof e.evt.button === 'number' && e.evt.button !== 0) return;

        isDragging.current = true;
        onMaskInteractionStart(e);
        e.cancelBubble = true;
        if (e.evt && e.evt.cancelable) e.evt.preventDefault();

        const stage = e.target.getStage();
        const pointer = getPointer(stage);
        if (!pointer) return;

        const cx = ((pRef.current as RadialParams).centerX - cropX) * scale;
        const cy = ((pRef.current as RadialParams).centerY - cropY) * scale;

        const startAngle = Math.atan2(pointer.y - cy, pointer.x - cx);
        rotateStartRef.current = {
          angle: startAngle,
          rotation: pRef.current.rotation || 0,
        };
      },
      [onMaskInteractionStart, cropX, cropY, scale, getPointer],
    );

    const handleRotateMove = useCallback(
      (e: CanvasPointerEvent) => {
        if (!rotateStartRef.current) return;
        const stage = e.target.getStage();
        const pointer = getPointer(stage);
        if (!pointer) return;

        setRotateCursor(stage, pointer);

        const cx = ((pRef.current as RadialParams).centerX - cropX) * scale;
        const cy = ((pRef.current as RadialParams).centerY - cropY) * scale;

        const currentAngle = Math.atan2(pointer.y - cy, pointer.x - cx);
        const angleDiff = currentAngle - rotateStartRef.current.angle;
        const angleDiffDeg = (angleDiff * 180) / Math.PI;

        const newRotation = rotateStartRef.current.rotation + angleDiffDeg;

        const newP = {
          ...pRef.current,
          rotation: newRotation,
        };

        updateP(newP);
        if (onPreviewUpdate) onPreviewUpdate(subMask.id, { parameters: newP });
        if (commitWhileDragging) onUpdate(subMask.id, { parameters: newP });
      },
      [
        cropX,
        cropY,
        scale,
        updateP,
        onPreviewUpdate,
        subMask.id,
        setRotateCursor,
        getPointer,
        onUpdate,
        commitWhileDragging,
      ],
    );

    const handleRotateEnd = useCallback(
      (e: CanvasPointerEvent) => {
        isDragging.current = false;
        rotateStartRef.current = null;
        onMaskInteractionEnd();
        onUpdate(subMask.id, { parameters: pRef.current });

        if (e?.target?.getStage) {
          e.target.getStage()!.container().style.cursor = '';
        }
      },
      [subMask.id, onMaskInteractionEnd, onUpdate],
    );

    const handleRotateHoverMove = useCallback(
      (e: KonvaEventObject<MouseEvent>) => {
        if (isToolActive || isDragging.current) return;
        const stage = e.target.getStage();
        const pointer = getPointer(stage);
        if (pointer) setRotateCursor(stage, pointer);
      },
      [isToolActive, setRotateCursor, getPointer],
    );

    const handleRotateMouseEnter = useCallback(
      (e: KonvaEventObject<MouseEvent>) => {
        onMaskMouseEnter();
        if (!isToolActive && !isDragging.current) {
          const stage = e.target.getStage();
          const pointer = getPointer(stage);
          if (pointer) setRotateCursor(stage, pointer);
        }
      },
      [onMaskMouseEnter, isToolActive, setRotateCursor, getPointer],
    );

    const handleRotateMouseLeave = useCallback(
      (e: KonvaEventObject<MouseEvent>) => {
        onMaskMouseLeave();
        if (!isDragging.current) {
          const stage = e.target.getStage();
          stage!.container().style.cursor = '';
        }
      },
      [onMaskMouseLeave],
    );

    const handleLinearGroupDragStart = useCallback(
      (e: KonvaEventObject<DragEvent>) => {
        if (e.evt && typeof e.evt.button === 'number' && e.evt.button !== 0) return;
        isDragging.current = true;
        onMaskInteractionStart(e);
        dragStartPointer.current = getPointer(e.target.getStage());
        dragStartParams.current = { ...pRef.current };
        e.cancelBubble = true;
      },
      [onMaskInteractionStart, getPointer],
    );

    const handleLinearGroupDragMove = useCallback(
      (e: KonvaEventObject<DragEvent>) => {
        const pointerPos = getPointer(e.target.getStage());
        if (!pointerPos || !dragStartPointer.current || !dragStartParams.current) return;

        const dx = (pointerPos.x - dragStartPointer.current.x) / scale;
        const dy = (pointerPos.y - dragStartPointer.current.y) / scale;

        const newP = {
          ...dragStartParams.current,
          startX: (dragStartParams.current as LinearParams).startX + dx,
          startY: (dragStartParams.current as LinearParams).startY + dy,
          endX: (dragStartParams.current as LinearParams).endX + dx,
          endY: (dragStartParams.current as LinearParams).endY + dy,
        };

        updateP(newP);
        if (onPreviewUpdate) onPreviewUpdate(subMask.id, { parameters: newP });
        if (commitWhileDragging) onUpdate(subMask.id, { parameters: newP });
      },
      [scale, updateP, onPreviewUpdate, subMask.id, getPointer, onUpdate, commitWhileDragging],
    );

    const handleLinearGroupDragEnd = useCallback(
      (e: KonvaEventObject<DragEvent>) => {
        isDragging.current = false;
        e.cancelBubble = true;
        onMaskInteractionEnd();
        onUpdate(subMask.id, { parameters: pRef.current });
      },
      [subMask.id, onMaskInteractionEnd, onUpdate],
    );

    if (!subMask.visible) {
      return null;
    }

    const commonProps = {
      dash: [4, 4],
      onClick: handleSelect,
      onTap: handleSelect,
      opacity: isSelected ? 1 : 0.7,
      stroke: isSelected
        ? '#0ea5e9'
        : subMask.mode === SubMaskMode.Subtractive
          ? '#f43f5e'
          : subMask.mode === SubMaskMode.Intersect
            ? '#a855f7'
            : 'white',
      strokeScaleEnabled: false,
      strokeWidth: isSelected ? 3 : 2,
    };

    if (subMask.type === Mask.AiSubject) {
      const { startX, startY, endX, endY } = p;
      if (startX !== undefined && startY !== undefined && endX !== undefined && endY !== undefined) {
        const isPoint = Math.abs(startX - endX) < 1e-6 && Math.abs(startY - endY) < 1e-6;
        if (isPoint) {
          return (
            <Circle
              x={(startX - cropX) * scale}
              y={(startY - cropY) * scale}
              radius={5}
              stroke={isSelected ? '#0ea5e9' : 'white'}
              strokeWidth={2}
              listening={!isToolActive}
              onClick={handleSelect}
              onTap={handleSelect}
              onTouchEnd={handleMaskTouchEnd}
              onTouchStart={handleMaskTouchStart}
              onMouseEnter={onMaskMouseEnter}
              onMouseLeave={onMaskMouseLeave}
              shadowColor="black"
              shadowBlur={2}
              shadowOpacity={0.8}
            />
          );
        } else {
          return (
            <Rect
              height={Math.max(0.1, Math.abs(endY - startY) * scale)}
              onMouseEnter={onMaskMouseEnter}
              onMouseLeave={onMaskMouseLeave}
              onTouchEnd={handleMaskTouchEnd}
              onTouchStart={handleMaskTouchStart}
              width={Math.max(0.1, Math.abs(endX - startX) * scale)}
              x={(Math.min(startX, endX) - cropX) * scale}
              y={(Math.min(startY, endY) - cropY) * scale}
              {...commonProps}
            />
          );
        }
      }
      return null;
    }

    if (subMask.type === Mask.Brush || subMask.type === Mask.Flow) {
      const { lines = [] } = p;

      return (
        <Group
          onClick={handleSelect}
          onTap={handleSelect}
          onTouchEnd={handleMaskTouchEnd}
          onTouchStart={handleMaskTouchStart}
        >
          {lines.map((line: DrawnLine, i: number) => (
            <OptimizedBrushLine key={i} line={line} scale={scale} cropX={cropX} cropY={cropY} />
          ))}
        </Group>
      );
    }

    if (subMask.type === Mask.Radial) {
      const { centerX, centerY, radiusX, radiusY, rotation } = p as RadialParams;

      return (
        <Group>
          {isSelected && !isToolActive && (
            <Ellipse
              x={(centerX - cropX) * scale}
              y={(centerY - cropY) * scale}
              radiusX={Math.max(0.1, radiusX * scale) + 35}
              radiusY={Math.max(0.1, radiusY * scale) + 35}
              rotation={rotation}
              fill="transparent"
              draggable
              dragBoundFunc={lockDragBoundFunc}
              onDragStart={handleRotateStart}
              onDragMove={handleRotateMove}
              onDragEnd={handleRotateEnd}
              onMouseEnter={handleRotateMouseEnter}
              onMouseMove={handleRotateHoverMove}
              onMouseLeave={handleRotateMouseLeave}
              onTouchStart={handleRotateStart}
              onTouchMove={handleRotateMove}
              onTouchEnd={handleRotateEnd}
            />
          )}

          <Ellipse
            {...commonProps}
            // A selected mask draws its outer line with the feather lines below.
            strokeEnabled={!isSelected || isToolActive}
            fill="transparent"
            draggable={!isToolActive}
            dragBoundFunc={lockDragBoundFunc}
            onDragStart={handleRadialDragStart}
            onDragMove={handleRadialDragMove}
            onDragEnd={handleRadialDragEnd}
            onMouseEnter={(e: KonvaEventObject<MouseEvent>) => {
              onMaskMouseEnter();
              if (!isToolActive && !isDragging.current) {
                e.target.getStage()!.container().style.cursor = 'move';
              }
            }}
            onMouseLeave={(e: KonvaEventObject<MouseEvent>) => {
              onMaskMouseLeave();
              if (!isDragging.current && e?.target?.getStage) {
                e.target.getStage()!.container().style.cursor = '';
              }
            }}
            onTouchEnd={handleMaskTouchEnd}
            onTouchStart={handleMaskTouchStart}
            radiusX={Math.max(0.1, radiusX * scale)}
            radiusY={Math.max(0.1, radiusY * scale)}
            rotation={rotation}
            x={(centerX - cropX) * scale}
            y={(centerY - cropY) * scale}
          />
          {isSelected &&
            !isToolActive &&
            (() => {
              const lines = radialLines(p as RadialParams);
              const handles = radialHandlePositions(p as RadialParams);
              const cursorFor = (cursor: string) => (e: KonvaEventObject<MouseEvent>) => {
                const stage = e.target.getStage();
                if (stage && !isDragging.current) stage.container().style.cursor = cursor;
              };
              const lineAt = (which: RadialLine | 'central', radii: { x: number; y: number }) => (
                <Ellipse
                  key={which}
                  x={(centerX - cropX) * scale}
                  y={(centerY - cropY) * scale}
                  radiusX={Math.max(0.1, radii.x * scale)}
                  radiusY={Math.max(0.1, radii.y * scale)}
                  rotation={rotation}
                  stroke={commonProps.stroke}
                  strokeWidth={which === 'central' ? 2.5 : 2}
                  strokeScaleEnabled={false}
                  dash={which === 'central' ? undefined : [6, 6]}
                  fillEnabled={false}
                  listening={which !== 'central'}
                  hitStrokeWidth={24}
                  draggable={which !== 'central'}
                  dragBoundFunc={lockDragBoundFunc}
                  onDragStart={handleRadialEditStart}
                  onDragMove={(e) => which !== 'central' && handleRadialEditMove(e, which)}
                  onDragEnd={handleShapeEditEnd}
                  onTouchStart={handleMaskTouchStart}
                  onTouchEnd={handleMaskTouchEnd}
                  onMouseEnter={(e) => {
                    onMaskMouseEnter();
                    cursorFor('col-resize')(e);
                  }}
                  onMouseLeave={(e) => {
                    onMaskMouseLeave();
                    cursorFor('')(e);
                  }}
                />
              );
              return (
                <>
                  {lineAt('outer', lines.outer)}
                  {lineAt('inner', lines.inner)}
                  {lineAt('central', lines.central)}
                  {(Object.keys(handles) as RadialHandle[]).map((handle) => (
                    <Circle
                      key={handle}
                      x={(handles[handle].x - cropX) * scale}
                      y={(handles[handle].y - cropY) * scale}
                      radius={7 / stageScale}
                      fill="#0ea5e9"
                      stroke="white"
                      strokeWidth={2 / stageScale}
                      draggable
                      dragBoundFunc={lockDragBoundFunc}
                      onDragStart={handleRadialEditStart}
                      onDragMove={(e) => handleRadialEditMove(e, handle)}
                      onDragEnd={handleShapeEditEnd}
                      onTouchStart={handleMaskTouchStart}
                      onTouchEnd={handleMaskTouchEnd}
                      onMouseEnter={(e) => {
                        onMaskMouseEnter();
                        cursorFor(handle === 'left' || handle === 'right' ? 'ew-resize' : 'ns-resize')(e);
                      }}
                      onMouseLeave={(e) => {
                        onMaskMouseLeave();
                        cursorFor('')(e);
                      }}
                    />
                  ))}
                </>
              );
            })()}
        </Group>
      );
    }

    if (subMask.type === Mask.Linear) {
      const defaultRange = Math.min(cropW, cropH) * 0.1;
      const params = { ...p, range: p.range ?? defaultRange } as LinearParams;
      const { startX, startY, endX, endY } = params;

      const { along, normal } = linearFrame(params);
      const offsets = linearLineOffsets(params);
      const midX = ((startX + endX) / 2 - cropX) * scale;
      const midY = ((startY + endY) / 2 - cropY) * scale;
      const EXT = 5000;
      // A line parallel to the central line, `offset` image pixels along the
      // normal (negative: start / 100% side).
      const parallel = (offset: number) => {
        const x = midX + normal.x * offset * scale;
        const y = midY + normal.y * offset * scale;
        return [x - along.x * EXT, y - along.y * EXT, x + along.x * EXT, y + along.y * EXT];
      };
      const startLine = parallel(offsets.start);
      const endLine = parallel(offsets.end);
      const editable = isSelected && !isToolActive;
      const showFeatherLines = isSelected && (!isToolActive || isBeingDrawn);
      const lineProps = {
        stroke: commonProps.stroke,
        opacity: commonProps.opacity,
        strokeWidth: isSelected ? 2.5 : 2,
        strokeScaleEnabled: false,
        hitStrokeWidth: 24,
      };
      const setCursor = (e: KonvaEventObject<MouseEvent>, cursor: string) => {
        const stage = e.target.getStage();
        if (stage && !isDragging.current) stage.container().style.cursor = cursor;
      };
      const featherLine = (line: LinearLine, points: number[]) => (
        <Line
          key={line}
          points={points}
          {...lineProps}
          dash={[6, 6]}
          listening={editable}
          draggable={editable}
          dragBoundFunc={lockDragBoundFunc}
          onDragStart={handleLinearEditStart}
          onDragMove={(e) => handleLinearEditMove(e, line)}
          onDragEnd={handleShapeEditEnd}
          onTouchEnd={handleMaskTouchEnd}
          onTouchStart={handleMaskTouchStart}
          onMouseEnter={(e) => {
            onMaskMouseEnter();
            setCursor(e, 'row-resize');
          }}
          onMouseLeave={(e) => {
            onMaskMouseLeave();
            setCursor(e, '');
          }}
        />
      );

      return (
        <Group>
          {editable && (
            // The transition zone: dragging inside it moves the whole mask.
            <Line
              points={[...startLine.slice(0, 2), ...startLine.slice(2), ...endLine.slice(2), ...endLine.slice(0, 2)]}
              closed
              fill="transparent"
              draggable
              dragBoundFunc={lockDragBoundFunc}
              onDragStart={handleLinearGroupDragStart}
              onDragMove={handleLinearGroupDragMove}
              onDragEnd={handleLinearGroupDragEnd}
              onTouchEnd={handleMaskTouchEnd}
              onTouchStart={handleMaskTouchStart}
              onMouseEnter={(e) => {
                onMaskMouseEnter();
                setCursor(e, 'move');
              }}
              onMouseLeave={(e) => {
                onMaskMouseLeave();
                setCursor(e, '');
              }}
            />
          )}
          <Line
            points={parallel(0)}
            {...lineProps}
            dash={[2, 3]}
            onClick={handleSelect}
            onTap={handleSelect}
            draggable={editable}
            dragBoundFunc={lockDragBoundFunc}
            onDragStart={handleLinearEditStart}
            onDragMove={(e) => handleLinearEditMove(e, 'central')}
            onDragEnd={handleShapeEditEnd}
            onTouchEnd={handleMaskTouchEnd}
            onTouchStart={handleMaskTouchStart}
            onMouseEnter={(e) => {
              onMaskMouseEnter();
              const stage = e.target.getStage();
              const pointer = stage ? getPointer(stage) : null;
              if (editable && stage && pointer && !isDragging.current) {
                setRotateCursor(stage, pointer, { x: midX, y: midY });
              }
            }}
            onMouseMove={(e) => {
              const stage = e.target.getStage();
              const pointer = stage ? getPointer(stage) : null;
              if (editable && stage && pointer && !isDragging.current) {
                setRotateCursor(stage, pointer, { x: midX, y: midY });
              }
            }}
            onMouseLeave={(e) => {
              onMaskMouseLeave();
              setCursor(e, '');
            }}
          />
          {showFeatherLines && (
            <>
              {featherLine('start', startLine)}
              {featherLine('end', endLine)}
            </>
          )}
          {isSelected && (
            // Capture One's centre point: the axis the central line rotates
            // around; dragging it moves the whole mask.
            <Circle
              x={midX}
              y={midY}
              radius={6 / stageScale}
              fill="#0ea5e9"
              stroke="white"
              strokeWidth={2 / stageScale}
              shadowColor="black"
              shadowBlur={2 / stageScale}
              shadowOpacity={0.6}
              listening={editable}
              draggable={editable}
              dragBoundFunc={lockDragBoundFunc}
              onDragStart={handleLinearGroupDragStart}
              onDragMove={handleLinearGroupDragMove}
              onDragEnd={handleLinearGroupDragEnd}
              onTouchEnd={handleMaskTouchEnd}
              onTouchStart={handleMaskTouchStart}
              onMouseEnter={(e) => {
                onMaskMouseEnter();
                setCursor(e, 'move');
              }}
              onMouseLeave={(e) => {
                onMaskMouseLeave();
                setCursor(e, '');
              }}
            />
          )}
        </Group>
      );
    }

    if (subMask.type === Mask.Color && (p.samples?.length || sampleDraft)) {
      const stroke = isSelected ? '#0ea5e9' : 'white';
      // Samples taken while the photo was turned otherwise: where their spot
      // of the photo is shown now.
      const shown = frameGeometry(adjustments);
      const component = frameGeometry(p);
      const turned = shown.orientationSteps % 2 === 1;
      const photoW = turned ? imageHeight : imageWidth;
      const photoH = turned ? imageWidth : imageHeight;
      const toScreen = (point: { x: number; y: number }, from: FrameGeometry) => {
        const at = reframePoint(point, from, shown, photoW, photoH);
        return [(at.x - cropX) * scale, (at.y - cropY) * scale];
      };
      return (
        <Group listening={false}>
          {(p.samples as ColorSample[] | undefined)?.map((sample, i) => {
            const from = sample.orientationSteps === undefined ? component : frameGeometry(sample);
            if (sample.width && sample.height) {
              if (sameFrameGeometry(from, shown)) {
                return (
                  <Rect
                    key={i}
                    x={(sample.x - cropX) * scale}
                    y={(sample.y - cropY) * scale}
                    width={sample.width * scale}
                    height={sample.height * scale}
                    stroke={stroke}
                    strokeWidth={1.5}
                    shadowColor="black"
                    shadowBlur={2}
                    shadowOpacity={0.8}
                  />
                );
              }
              const corners = [
                { x: sample.x, y: sample.y },
                { x: sample.x + sample.width, y: sample.y },
                { x: sample.x + sample.width, y: sample.y + sample.height },
                { x: sample.x, y: sample.y + sample.height },
              ];
              return (
                <Line
                  key={i}
                  points={corners.flatMap((corner) => toScreen(corner, from))}
                  closed
                  stroke={stroke}
                  strokeWidth={1.5}
                  shadowColor="black"
                  shadowBlur={2}
                  shadowOpacity={0.8}
                />
              );
            }
            const [x, y] = toScreen(sample, from);
            return (
              <Circle
                key={i}
                x={x}
                y={y}
                radius={5}
                stroke={stroke}
                strokeWidth={2}
                shadowColor="black"
                shadowBlur={2}
                shadowOpacity={0.8}
              />
            );
          })}
          {sampleDraft && (
            <Rect
              x={(Math.min(sampleDraft.x0, sampleDraft.x1) - cropX) * scale}
              y={(Math.min(sampleDraft.y0, sampleDraft.y1) - cropY) * scale}
              width={Math.abs(sampleDraft.x1 - sampleDraft.x0) * scale}
              height={Math.abs(sampleDraft.y1 - sampleDraft.y0) * scale}
              stroke="#0ea5e9"
              dash={[4, 3]}
              strokeWidth={1.5}
            />
          )}
        </Group>
      );
    }

    if (subMask.type === Mask.Color || subMask.type === Mask.Luminance) {
      const { targetX, targetY } = p;
      if (targetX !== undefined && targetX >= 0 && targetY !== undefined && targetY >= 0) {
        // Set on the photo as shown then: where that spot is shown now.
        const shown = frameGeometry(adjustments);
        const turned = shown.orientationSteps % 2 === 1;
        const target = reframePoint(
          { x: targetX, y: targetY },
          frameGeometry(p),
          shown,
          turned ? imageHeight : imageWidth,
          turned ? imageWidth : imageHeight,
        );
        return (
          <Circle
            x={(target.x - cropX) * scale}
            y={(target.y - cropY) * scale}
            radius={5}
            stroke={isSelected ? '#0ea5e9' : 'white'}
            strokeWidth={2}
            listening={false}
            onTouchEnd={handleMaskTouchEnd}
            onTouchStart={handleMaskTouchStart}
            shadowColor="black"
            shadowBlur={2}
            shadowOpacity={0.8}
          />
        );
      }
      return null;
    }
    return null;
  },
);

const ImageCanvas = memo(
  ({
    appSettings,
    activeMaskContainerId,
    activeMaskId,
    adjustments,
    brushSettings,
    crop,
    finalPreviewUrl,
    handleCropComplete,
    imageRenderSize,
    interactivePatch,
    isCropping,
    isMaskControlHovered,
    isMasking,
    isSliderDragging,
    isStraightenActive,
    isRotationActive,
    maskOverlayUrl,
    onGenerateAiMask,
    onLiveMaskPreview,
    onSelectMask,
    onStraighten,
    selectedImage,
    setCrop,
    setIsMaskHovered,
    setIsMaskTouchInteracting,
    showOriginal,
    uncroppedAdjustedPreviewUrl,
    updateSubMask,
    isWbPickerActive = false,
    onWbPicked,
    setAdjustments,
    overlayRotation,
    overlayMode,
    cursorStyle,
    isMaxZoom,
    liveRotation,
    transformState,
    hasRenderedFirstFrame,
    maskHitTestRef,
  }: ImageCanvasProps) => {
    const isGuidedPerspectiveActive = useEditorStore((state) => state.isGuidedPerspectiveActive);
    // The previous photograph's native frame stands in; no placeholder over it.
    const holdingNativeFrame = useEditorStore((state) => state.holdingNativeFrame);
    const showMaskWithSelectTool = useEditorStore((state) => state.showMaskWithSelectTool);
    const isMaskRangeDragging = useEditorStore((state) => state.isMaskRangeDragging);
    const maskOverlayAutoHidden = useEditorStore((state) => state.maskOverlayAuto === 'hidden');
    const lumaPickerLayer = useEditorStore((state) => state.lumaPickerLayer);
    const shapeDrawTarget = useEditorStore((state) => state.shapeDrawTarget);
    const tatTarget = useEditorStore((state) => state.tatTarget);
    const relightHandles = useRelightStore((s) => s.panelOpen && s.showHandles);
    const isSpotToolActive = useEditorStore((state) => state.isSpotToolActive);
    const redEyeMode = useEditorStore((state) => state.spotSettings.mode === 'redEye');
    const showFaceBlemishes = useEditorStore((state) => state.showFaceBlemishes);
    const [draftGuideLine, setDraftGuideLine] = useState<{ p1: Coord; p2: Coord } | null>(null);
    const [localDragLines, setLocalDragLines] = useState<GuideLine[] | null>(null);
    const [selectedGuideId, setSelectedGuideId] = useState<string | null>(null);
    const { showContextMenu } = useContextMenu();
    // Removes the given guides (all when null); under two guides the
    // correction pauses until another is drawn.
    const removeGuides = useCallback(
      (ids: string[] | null) => {
        setSelectedGuideId(null);
        setAdjustments((prev: Adjustments) => {
          const lines = ids ? (prev.guidedPerspective?.lines || []).filter((l) => !ids.includes(l.id)) : [];
          return {
            ...prev,
            guidedPerspective: { ...prev.guidedPerspective, lines, enabled: lines.length >= 2 },
          };
        });
      },
      [setAdjustments],
    );
    useEffect(() => {
      if (!isGuidedPerspectiveActive) {
        setSelectedGuideId(null);
        return;
      }
      // Capture phase, before the app shortcuts (Delete would remove the
      // photograph, Escape leave the crop view).
      const onKey = (e: KeyboardEvent) => {
        const target = e.target as HTMLElement | null;
        if (target?.closest('input, textarea, select, [contenteditable="true"]')) return;
        if ((e.key === 'Backspace' || e.key === 'Delete') && selectedGuideId) {
          e.preventDefault();
          e.stopPropagation();
          removeGuides([selectedGuideId]);
        } else if (e.key === 'Escape') {
          e.preventDefault();
          e.stopPropagation();
          if (selectedGuideId) setSelectedGuideId(null);
          else useEditorStore.getState().setEditor({ isGuidedPerspectiveActive: false });
        }
      };
      document.addEventListener('keydown', onKey, true);
      return () => document.removeEventListener('keydown', onKey, true);
    }, [isGuidedPerspectiveActive, selectedGuideId, removeGuides]);

    const [forwardH, setForwardH] = useState<number[]>(IDENTITY_3X3);
    const [invH, setInvH] = useState<number[]>(IDENTITY_3X3);

    const [isCropViewVisible, setIsCropViewVisible] = useState(false);
    const cropImageRef = useRef<HTMLImageElement>(null);
    // The gradient a drag is drawing: its component and its shape so far.
    const [shapeDraft, setShapeDraft] = useState<ShapeDraft | null>(null);
    const [isMaskInteractionActive, setIsMaskInteractionActive] = useState(false);
    // Pressing a mask handle must never pan or click-zoom the view, even when
    // no hover preceded the press (pens, Sidecar, synthetic input). The editor
    // asks before it starts a pan, synchronously, instead of relying on hover.
    const maskStageRef = useRef<Konva.Stage>(null);
    useEffect(() => {
      if (!maskHitTestRef) return;
      maskHitTestRef.current = (clientX, clientY) => {
        const stage = maskStageRef.current;
        if (!stage) return false;
        const rect = stage.container().getBoundingClientRect();
        if (!rect.width || !rect.height) return false;
        const shape = stage.getIntersection({
          x: ((clientX - rect.left) * stage.width()) / rect.width,
          y: ((clientY - rect.top) * stage.height()) / rect.height,
        });
        return !!shape?.findAncestor('.mask-handles');
      };
      return () => {
        maskHitTestRef.current = null;
      };
    }, [maskHitTestRef]);
    const isDrawing = useRef(false);
    const colorSampleRef = useRef<{
      x0: number;
      y0: number;
      x1: number;
      y1: number;
      add: boolean;
      /** For the luma range eyedropper instead of a colour range. */
      luma?: boolean;
    } | null>(null);
    const [colorSampleDraft, setColorSampleDraft] = useState<{ x0: number; y0: number; x1: number; y1: number } | null>(
      null,
    );
    // Mirrors isDrawing for rendering: "While drawing" shows the mask only then.
    const [isStrokeActive, setIsStrokeActive] = useState(false);
    const setDrawing = useCallback((drawing: boolean) => {
      isDrawing.current = drawing;
      setIsStrokeActive(drawing);
    }, []);
    const drawingStageRef = useRef<Konva.Stage | null>(null);
    const dragStartPointer = useRef<(Coord & ShapeDragState) | null>(null);
    const lastBrushPoint = useRef<Coord | null>(null);
    const currentLine = useRef<DrawnLine | null>(null);
    // The positions the pointer passed since the last delivered move: the
    // webview delivers about one move per display frame, the mouse or pen
    // may report more (coalesced events), so a fast stroke keeps its curve.
    const coalescedMoves = useRef<PointerEvent[]>([]);
    // The last pointer event's pressure to store (a pen's while a mapping is
    // on, else none): pointer events precede the mouse events strokes use.
    const pointerPressure = useRef<number | undefined>(undefined);
    // The stroke being drawn in image space, grown by each move's new points.
    const liveStroke = useRef<Omit<LiveStroke, 'maskId'> | null>(null);
    const nextStrokeId = useRef(0);
    const previewBoxRef = useRef<{ start: Coord; end: Coord } | null>(null);
    const [previewBox, setPreviewBox] = useState<{ start: Coord; end: Coord } | null>(null);
    // The Object selection's press: what it does to the prompts (Shift adds,
    // Alt takes out), and the prompts shown while their mask is computed.
    const objectGestureRef = useRef<PromptGesture>('replace');
    const [objectPromptsDraft, setObjectPromptsDraft] = useState<{ id: string; prompts: ObjectPrompt[] } | null>(null);
    const activeStrokeIndex = useRef<number | null>(null);

    const [cursorPreview, setCursorPreview] = useState<CursorPreview>({ x: 0, y: 0, visible: false });
    // While the brush rings show, they follow the pointer by moving their
    // Konva node on its own layer: a render of the canvas per move held up
    // the pointer events after it (the state keeps visibility and the last
    // position React drew).
    const cursorPosition = useRef<Coord>({ x: 0, y: 0 });
    const brushCursorRef = useRef<Konva.Group>(null);
    const cropGesture = useCropGestures();
    // Closed or another photograph mid-drag: the crop component reports no
    // end then.
    const cropGestureEndAll = cropGesture.endAll;
    useEffect(() => {
      if (!isCropping || !crop) cropGestureEndAll();
    }, [isCropping, crop, cropGestureEndAll]);
    useEffect(() => cropGestureEndAll, [selectedImage.path, cropGestureEndAll]);
    const [straightenLine, setStraightenLine] = useState<{ start: Coord; end: Coord } | null>(null);
    const isStraightening = useRef(false);
    // The line's gesture; switched off mid-line (Escape removes the drawing
    // layer before its release), the line and its gesture end.
    const lineGesture = useToolGesture(isStraightenActive);
    useEffect(() => {
      if (isStraightenActive || !isStraightening.current) return;
      isStraightening.current = false;
      setStraightenLine(null);
    }, [isStraightenActive]);

    const decodedBase = useDecodedPreview(
      finalPreviewUrl || selectedImage.thumbnailUrl ? { url: (finalPreviewUrl || selectedImage.thumbnailUrl)! } : null,
      selectedImage.path,
    );
    const displayBase = decodedBase?.url;

    const [baseTool, setBaseTool] = useState<ToolType>(brushSettings?.tool ?? ToolType.Brush);
    const [isAltPressed, setIsAltPressed] = useState(false);
    const retainedPatchRef = useRef<typeof interactivePatch>(null);

    const isWgpuActive =
      appSettings?.useWgpuRenderer !== false && canRenderSource(selectedImage) && hasRenderedFirstFrame;
    // Like brush strokes, drawing, moving or reshaping a radial/linear mask
    // renders the developed image live and commits (save, history, thumbnail)
    // on release. The JPEG fallback keeps committing every move.
    const renderShapesLive = isMasking && !!isWgpuActive;
    const { t } = useTranslation();

    const paddingX = imageRenderSize.width * 0.5;
    const paddingY = imageRenderSize.height * 0.5;

    const stageLeft = imageRenderSize.offsetX - paddingX;
    const stageTop = imageRenderSize.offsetY - paddingY;
    const stageWidth = imageRenderSize.width > 0 ? imageRenderSize.width + paddingX * 2 : 0;
    const stageHeight = imageRenderSize.height > 0 ? imageRenderSize.height + paddingY * 2 : 0;

    const groupOffsetX = paddingX;
    const groupOffsetY = paddingY;

    const [settledScale, setSettledScale] = useState(transformState.scale);
    useEffect(() => {
      const timer = setTimeout(() => {
        setSettledScale(transformState.scale);
      }, 150);
      return () => clearTimeout(timer);
    }, [transformState.scale]);

    const maxDimension = Math.max(stageWidth, stageHeight, 1);
    const maxSafeScale = Math.max(1, Math.min(settledScale, 4092 / maxDimension));

    const getCanvasPointer = useCallback(
      // Stage events always come from a node on a stage.
      (stage: Konva.Stage | null) => {
        const pos = stage!.getPointerPosition();
        if (!pos) return null;
        return {
          x: pos.x / maxSafeScale - groupOffsetX,
          y: pos.y / maxSafeScale - groupOffsetY,
        };
      },
      [groupOffsetX, groupOffsetY, maxSafeScale],
    );

    useEffect(() => {
      if (interactivePatch) {
        retainedPatchRef.current = interactivePatch;
      }
    }, [interactivePatch]);

    useEffect(() => {
      setBaseTool(brushSettings?.tool ?? ToolType.Brush);
    }, [brushSettings?.tool]);

    useEffect(() => {
      const handleKeyDown = (e: KeyboardEvent) => {
        if (e.key === 'Alt') {
          e.preventDefault();
          (window as ModifierKeyWindow).altKeyDown = true;
          setIsAltPressed(true);
        }
      };
      const handleKeyUp = (e: KeyboardEvent) => {
        if (e.key === 'Alt') {
          e.preventDefault();
          (window as ModifierKeyWindow).altKeyDown = false;
          setIsAltPressed(false);
        }
      };
      const handleBlur = () => {
        (window as ModifierKeyWindow).altKeyDown = false;
        setIsAltPressed(false);
      };

      window.addEventListener('keydown', handleKeyDown);
      window.addEventListener('keyup', handleKeyUp);
      window.addEventListener('blur', handleBlur);

      return () => {
        window.removeEventListener('keydown', handleKeyDown);
        window.removeEventListener('keyup', handleKeyUp);
        window.removeEventListener('blur', handleBlur);
        (window as ModifierKeyWindow).altKeyDown = false;
      };
    }, []);

    const activeContainer = useMemo(() => {
      if (isMasking) {
        return adjustments.masks.find((c: MaskContainer) => c.id === activeMaskContainerId);
      }
      return null;
    }, [adjustments.masks, activeMaskContainerId, isMasking]);

    const activeSubMask = useMemo(() => {
      if (!activeContainer) {
        return null;
      }
      return activeContainer.subMasks.find((m: SubMask) => m.id === activeMaskId);
    }, [activeContainer, activeMaskId]);

    // The geometry new shapes and strokes are drawn in, recorded with them.
    const drawingFrame = useMemo(
      () =>
        shapeFrame(
          {
            rotation: adjustments.rotation,
            flipHorizontal: adjustments.flipHorizontal,
            flipVertical: adjustments.flipVertical,
            orientationSteps: adjustments.orientationSteps,
          },
          selectedImage.width,
          selectedImage.height,
        ),
      [
        adjustments.rotation,
        adjustments.flipHorizontal,
        adjustments.flipVertical,
        adjustments.orientationSteps,
        selectedImage.width,
        selectedImage.height,
      ],
    );

    const effectiveImageDimensions = useMemo(() => {
      const steps = adjustments.orientationSteps || 0;
      const w = selectedImage.width || 0;
      const h = selectedImage.height || 0;
      if (steps === 1 || steps === 3) {
        return { width: h, height: w };
      }
      return { width: w, height: h };
    }, [selectedImage.width, selectedImage.height, adjustments.orientationSteps]);

    const activeCrop = adjustments.crop;
    const isPercentCrop = activeCrop?.unit === '%';
    const cropX = activeCrop
      ? isPercentCrop
        ? (activeCrop.x / 100) * effectiveImageDimensions.width
        : activeCrop.x
      : 0;
    const cropY = activeCrop
      ? isPercentCrop
        ? (activeCrop.y / 100) * effectiveImageDimensions.height
        : activeCrop.y
      : 0;

    const effectiveZoomScale = transformState.scale > 0 ? transformState.scale : 1;
    // Brush sizes are image pixels (Capture One); stage units are image pixels
    // times the render scale, and the zoom scales the stage on screen.
    const brushImageSpaceSize = brushSettings?.size ?? 0;
    const brushStageSize = brushImageSpaceSize * (imageRenderSize.scale || 1);
    const screenPixelsPerImagePixel = (imageRenderSize.scale || 1) * effectiveZoomScale;

    const isBrushActive = isMasking && (activeSubMask?.type === Mask.Brush || activeSubMask?.type === Mask.Flow);

    const activeLineFlow = activeSubMask?.type === Mask.Flow ? (activeSubMask?.parameters?.flow ?? 10) : undefined;
    // Brush strokes carry Capture One's opacity and flow; at 100% they are left
    // out, and the stroke renders exactly as before.
    const isFlowMask = activeSubMask?.type === Mask.Flow;
    const brushOpacity = brushSettings?.opacity ?? 100;
    const brushFlow = brushSettings?.flow ?? 100;
    const strokeOpacity = !isFlowMask && brushOpacity < 100 ? brushOpacity : undefined;
    const strokeFlow = isFlowMask ? activeLineFlow : brushFlow < 100 ? brushFlow : undefined;
    const strokeHardness = isFlowMask ? undefined : brushSettings?.hardness;
    const strokeAutoMask = !isFlowMask && brushSettings?.autoMask ? true : undefined;
    const strokeAutoMaskVersion = strokeAutoMask ? AUTO_MASK_VERSION : undefined;
    const strokePressure = useMemo(
      () => (isFlowMask ? { size: false, flow: false } : pressureMapping(brushSettings)),
      [isFlowMask, brushSettings],
    );

    // An Auto Mask stroke reads the full-resolution photo as it looks before
    // develop. Building it (~0.3 s for 40 MP) delayed the first stroke's first
    // dab; while the layers panel is open with Auto Mask on, the backend builds
    // it in the background (same pixels, only earlier) from the recipe of its
    // last preview, and keeps it built when the source or geometry changes.
    const warmPath = autoMaskPrewarmPath(isMasking, brushSettings?.autoMask, selectedImage);
    useEffect(() => {
      invoke(Invokes.PrewarmMaskSource, { path: warmPath }).catch(() => {});
    }, [warmPath]);

    // Capture One's brush cursor: the inner ring is the brush size (50%
    // coverage), the outer one where the soft edge ends, r(1 + s) for softness
    // s. Outlines stay visible at any brush opacity; opacity and flow show in
    // the gesture readout instead.
    const brushCursorRings = useMemo(() => {
      const radius = Math.max(0.1, brushStageSize / 2);
      const softness =
        strokeHardness === undefined ? undefined : 1 - Math.max(0, Math.min(1, strokeHardness / 100));
      const feather = Math.max(0, Math.min(1, (brushSettings?.feather ?? 0) / 100));
      const isEraser = isAltPressed ? baseTool !== ToolType.Eraser : baseTool === ToolType.Eraser;
      return softness === undefined
        ? { inner: Math.max(0.1, radius * (1 - feather)), outer: radius, isEraser }
        : { inner: radius, outer: radius * (1 + softness), isEraser };
    }, [brushSettings?.feather, strokeHardness, brushStageSize, baseTool, isAltPressed]);

    const isAiSubjectActive = isMasking && activeSubMask?.type === Mask.AiSubject;
    // The luma range eyedropper (Lightroom's Luminance Range picker): a click
    // or a drag on the photo sets the active layer's range.
    const isLumaPickerActive =
      isMasking &&
      !!activeContainer &&
      lumaPickerLayer === activeContainer.id &&
      activeContainer.subMasks.some((m: SubMask) => m.type === Mask.LumaRange);
    const isParametricActive =
      isLumaPickerActive ||
      (isMasking && (activeSubMask?.type === Mask.Color || activeSubMask?.type === Mask.Luminance));
    // A new gradient: the next drag on the photo draws it.
    const isInitialDrawing =
      isMasking &&
      !!activeSubMask &&
      shapeDrawTarget === activeSubMask.id &&
      (activeSubMask.type === Mask.Linear || activeSubMask.type === Mask.Radial);
    // Another component or photograph ends a gradient drag: its draft is
    // dropped, the drawing session ends and the preview shows the recipe's
    // shape again.
    const activeSubMaskId = activeSubMask?.id;
    const livePreviewRef = useRef(onLiveMaskPreview);
    livePreviewRef.current = onLiveMaskPreview;
    const shownImagePath = useRef(selectedImage.path);
    useEffect(() => {
      const samePhoto = shownImagePath.current === selectedImage.path;
      shownImagePath.current = selectedImage.path;
      const start = dragStartPointer.current;
      setShapeDraft(null);
      if (!start || start.id === activeSubMaskId) return;
      dragStartPointer.current = null;
      setDrawing(false);
      drawingStageRef.current = null;
      if (samePhoto)
        restoreAfterShapeDrag(start, useEditorStore.getState().adjustments, {
          renderLive: requestLiveMaskRender,
          showLayer: (layer) => livePreviewRef.current?.(layer),
        });
    }, [activeSubMaskId, selectedImage.path, setDrawing]);

    // Control+Option drags change the brush instead of painting (Capture One).
    const canvasRootRef = useRef<HTMLDivElement>(null);
    const brushGesture = useBrushGesture(
      canvasRootRef,
      Boolean(isBrushActive),
      brushSettings,
      screenPixelsPerImagePixel,
      (values) => {
        const { brushSettings: current, setEditor } = useEditorStore.getState();
        setEditor({
          brushSettings: { ...(current ?? { size: DEFAULT_BRUSH_SIZE, feather: 50, tool: ToolType.Brush }), ...values },
        });
      },
    );
    // The rings stay where the drag began while the pointer adjusts them.
    const [gestureAnchor, setGestureAnchor] = useState<CursorPreview | null>(null);
    const gestureActive = brushGesture !== null;
    useEffect(() => {
      setGestureAnchor(gestureActive ? { ...cursorPosition.current, visible: true } : null);
      if (!gestureActive) setCursorPreview((p: CursorPreview) => ({ ...p, ...cursorPosition.current }));
      // eslint-disable-next-line react-hooks/exhaustive-deps -- the anchor is the cursor when the gesture starts, not its later moves
    }, [gestureActive]);

    const isToolActive = isBrushActive || isAiSubjectActive || isInitialDrawing || isParametricActive;

    // Mask layers are drawn by the native display with each preview; the PNG
    // overlay remains for the JPEG fallback.
    const drawsOverlayNatively = drawsMaskOverlayNatively({
      nativeDisplay: !!isWgpuActive,
      masking: isMasking,
      livePreviews: appSettings?.enableLivePreviews !== false,
    });
    const displayedMaskUrl = isMasking && !drawsOverlayNatively ? maskOverlayUrl : null;

    useEffect(() => {
      if (isToolActive) {
        return;
      }
      setDrawing(false);
      drawingStageRef.current = null;
      // A gradient drag ended by leaving the tool or masking.
      if (dragStartPointer.current)
        restoreAfterShapeDrag(dragStartPointer.current, useEditorStore.getState().adjustments, {
          renderLive: requestLiveMaskRender,
          showLayer: (layer) => livePreviewRef.current?.(layer),
        });
      dragStartPointer.current = null;
      currentLine.current = null;
      lastBrushPoint.current = null;
      setPreviewBox(null);
      previewBoxRef.current = null;
      setShapeDraft(null);
    }, [isToolActive, setDrawing]);

    useEffect(() => {
      if (!isMasking) {
        setIsMaskInteractionActive(false);
      }
    }, [isMasking]);

    useEffect(() => {
      const clearTouchInteraction = () => {
        setIsMaskTouchInteracting(false);
      };

      window.addEventListener('touchend', clearTouchInteraction);
      window.addEventListener('touchcancel', clearTouchInteraction);

      return () => {
        window.removeEventListener('touchend', clearTouchInteraction);
        window.removeEventListener('touchcancel', clearTouchInteraction);
      };
    }, [setIsMaskTouchInteracting]);

    const sortedSubMasks = useMemo(() => {
      if (!activeContainer) {
        return [];
      }
      const selectedMask = activeContainer.subMasks.find((m: SubMask) => m.id === activeMaskId);
      const otherMasks = activeContainer.subMasks.filter((m: SubMask) => m.id !== activeMaskId);
      return selectedMask ? [...otherMasks, selectedMask] : activeContainer.subMasks;
    }, [activeContainer, activeMaskId]);

    // With the native display the crop tool appears once the display shows
    // the uncropped frame; until then the cropped photo stays on screen.
    const nativeCropFrame = useEditorStore((s) => s.nativeCropFrame);
    const cropViewReady = isWgpuActive ? nativeCropFrame : !!uncroppedAdjustedPreviewUrl;
    const cropLayerVisible = isCropViewVisible && isCropping && cropViewReady;
    // Capture One shows the grid and the frame's size only while the frame
    // is dragged; Lightroom can show the grid always (crop grid setting).
    const [isCropDragging, setIsCropDragging] = useState(false);
    const cropGrid = appSettings?.cropGrid ?? 'drag';
    const cropShade = (appSettings?.cropShade ?? 50) / 100;
    useEffect(() => {
      if (isCropping && cropViewReady) {
        const timer = setTimeout(() => setIsCropViewVisible(true), 10);
        return () => clearTimeout(timer);
      } else {
        setIsCropViewVisible(false);
      }
    }, [isCropping, cropViewReady]);

    const uncroppedImageRenderSize = useMemo<Partial<RenderSize> | null>(() => {
      if (!selectedImage?.width || !selectedImage?.height || !imageRenderSize?.width || !imageRenderSize?.height) {
        return null;
      }

      const viewportWidth = imageRenderSize.width + 2 * imageRenderSize.offsetX;
      const viewportHeight = imageRenderSize.height + 2 * imageRenderSize.offsetY;

      let uncroppedEffectiveWidth = selectedImage.width;
      let uncroppedEffectiveHeight = selectedImage.height;
      const orientationSteps = adjustments.orientationSteps || 0;
      if (orientationSteps === 1 || orientationSteps === 3) {
        [uncroppedEffectiveWidth, uncroppedEffectiveHeight] = [uncroppedEffectiveHeight, uncroppedEffectiveWidth];
      }

      if (uncroppedEffectiveWidth <= 0 || uncroppedEffectiveHeight <= 0 || viewportWidth <= 0 || viewportHeight <= 0) {
        return null;
      }

      const scale = Math.min(viewportWidth / uncroppedEffectiveWidth, viewportHeight / uncroppedEffectiveHeight);

      const renderWidth = uncroppedEffectiveWidth * scale;
      const renderHeight = uncroppedEffectiveHeight * scale;

      return { width: renderWidth, height: renderHeight };
    }, [selectedImage?.width, selectedImage?.height, imageRenderSize, adjustments.orientationSteps]);

    useEffect(() => {
      const calcMatrix = async () => {
        if (!selectedImage?.width || !selectedImage?.height) return;
        const Ow = selectedImage.width;
        const Oh = selectedImage.height;
        let guidedH = IDENTITY_3X3;
        const lines = adjustments.guidedPerspective?.lines || [];
        if (lines.length >= 2) {
          try {
            // Upright corrections zoom to fill the frame (Constrain crop,
            // stored as autoCrop), as the engine does.
            const fill = Boolean(
              adjustments.guidedPerspective?.upright &&
                adjustments.guidedPerspective.upright !== 'off' &&
                adjustments.guidedPerspective.autoCrop !== false,
            );
            const res: GuidedPerspectiveResult = await invoke(Invokes.CalculateGuidedPerspective, { lines, width: Ow, height: Oh, fill });
            if (res?.valid && (res?.forwardH || res?.forward_h)) {
              guidedH = res.forwardH || res.forward_h;
            }
          } catch (e) {
            console.error('Matrix calculation failed', e);
          }
        }
        // The manual Transform sliders as the engine applies them (measured in
        // Camera Raw; calculate_manual_transform), on centred coordinates.
        let manual = IDENTITY_3X3;
        try {
          manual = await invoke<number[]>(Invokes.CalculateManualTransform, {
            adjustments: {
              transformVertical: adjustments.transformVertical ?? 0,
              transformHorizontal: adjustments.transformHorizontal ?? 0,
              transformRotate: adjustments.transformRotate ?? 0,
              transformAspect: adjustments.transformAspect ?? 0,
              transformScale: adjustments.transformScale ?? 100,
              transformXOffset: adjustments.transformXOffset ?? 0,
              transformYOffset: adjustments.transformYOffset ?? 0,
            },
            width: Ow,
            height: Oh,
          });
        } catch (e) {
          console.error('Transform matrix failed', e);
        }

        const cx = Ow / 2.0;
        const cy = Oh / 2.0;
        const t_center = [1, 0, cx, 0, 1, cy, 0, 0, 1];
        const t_uncenter = [1, 0, -cx, 0, 1, -cy, 0, 0, 1];

        let f = multiply3x3(t_center, manual);
        f = multiply3x3(f, guidedH);
        f = multiply3x3(f, t_uncenter);

        setForwardH(f);
        setInvH(invert3x3(f));
      };
      calcMatrix();
    }, [
      selectedImage?.width,
      selectedImage?.height,
      adjustments.guidedPerspective?.lines,
      adjustments.guidedPerspective?.upright,
      adjustments.guidedPerspective?.autoCrop,
      adjustments.transformVertical,
      adjustments.transformHorizontal,
      adjustments.transformRotate,
      adjustments.transformAspect,
      adjustments.transformScale,
      adjustments.transformXOffset,
      adjustments.transformYOffset,
    ]);

    const mapUvToScreen = useCallback(
      (uv: Coord) => {
        if (!uncroppedImageRenderSize?.width || !uncroppedImageRenderSize?.height) return { x: 0, y: 0 };
        const Ow = selectedImage?.width || 1920;
        const Oh = selectedImage?.height || 1080;
        const orientationSteps = adjustments.orientationSteps || 0;
        const Dw = orientationSteps % 2 !== 0 ? Oh : Ow;
        const Dh = orientationSteps % 2 !== 0 ? Ow : Oh;

        const ox = uv.x * Ow;
        const oy = uv.y * Oh;
        const warped = project3x3(forwardH, ox, oy);

        let { x: px, y: py } = orientPoint(warped.x, warped.y, Ow, Oh, orientationSteps);
        if (adjustments.flipHorizontal) px = Dw - px;
        if (adjustments.flipVertical) py = Dh - py;

        const sx = (px / Dw) * uncroppedImageRenderSize.width;
        const sy = (py / Dh) * uncroppedImageRenderSize.height;

        const activeRotation =
          liveRotation !== null && liveRotation !== undefined ? liveRotation : adjustments.rotation || 0;
        if (Math.abs(activeRotation) > 1e-4) {
          const rad = (activeRotation * Math.PI) / 180;
          const cos = Math.cos(rad);
          const sin = Math.sin(rad);
          const cx = uncroppedImageRenderSize.width / 2;
          const cy = uncroppedImageRenderSize.height / 2;
          const dx = sx - cx;
          const dy = sy - cy;
          return {
            x: cx + dx * cos - dy * sin,
            y: cy + dx * sin + dy * cos,
          };
        }

        return { x: sx, y: sy };
      },
      [
        forwardH,
        uncroppedImageRenderSize,
        selectedImage,
        adjustments.orientationSteps,
        adjustments.flipHorizontal,
        adjustments.flipVertical,
        adjustments.rotation,
        liveRotation,
      ],
    );

    const mapScreenToUv = useCallback(
      (stageX: number, stageY: number): Coord => {
        if (!uncroppedImageRenderSize?.width || !uncroppedImageRenderSize?.height) return { x: 0, y: 0 };
        const Ow = selectedImage?.width || 1920;
        const Oh = selectedImage?.height || 1080;
        const orientationSteps = adjustments.orientationSteps || 0;
        const Dw = orientationSteps % 2 !== 0 ? Oh : Ow;
        const Dh = orientationSteps % 2 !== 0 ? Ow : Oh;

        const activeRotation =
          liveRotation !== null && liveRotation !== undefined ? liveRotation : adjustments.rotation || 0;
        let sx = stageX;
        let sy = stageY;

        if (Math.abs(activeRotation) > 1e-4) {
          const rad = (activeRotation * Math.PI) / 180;
          const cos = Math.cos(rad);
          const sin = Math.sin(rad);
          const cx = uncroppedImageRenderSize.width / 2;
          const cy = uncroppedImageRenderSize.height / 2;
          const dx = stageX - cx;
          const dy = stageY - cy;
          sx = cx + dx * cos + dy * sin;
          sy = cy - dx * sin + dy * cos;
        }

        let px = (sx / uncroppedImageRenderSize.width) * Dw;
        let py = (sy / uncroppedImageRenderSize.height) * Dh;

        if (adjustments.flipHorizontal) px = Dw - px;
        if (adjustments.flipVertical) py = Dh - py;

        const unoriented = unorientPoint(px, py, Dw, Dh, orientationSteps);
        const orig = project3x3(invH, unoriented.x, unoriented.y);

        return {
          x: Math.max(0, Math.min(1, orig.x / Ow)),
          y: Math.max(0, Math.min(1, orig.y / Oh)),
        };
      },
      [invH, uncroppedImageRenderSize, selectedImage, adjustments, liveRotation],
    );

    const whiteBalanceRequestRef = useRef(0);
    const handleWbClick = useCallback(
      (e: CanvasPointerEvent) => {
        if (!isWbPickerActive || !selectedImage.isReady || !onWbPicked) return;

        const stage = e.target.getStage();
        const pointerPos = getCanvasPointer(stage);
        if (!pointerPos) return;

        const x = pointerPos.x / imageRenderSize.scale;
        const y = pointerPos.y / imageRenderSize.scale;

        const imgLogicalWidth = imageRenderSize.width / imageRenderSize.scale;
        const imgLogicalHeight = imageRenderSize.height / imageRenderSize.scale;

        if (x < 0 || x > imgLogicalWidth || y < 0 || y > imgLogicalHeight) return;

        const requestId = ++whiteBalanceRequestRef.current;
        const colorTarget = useEditorStore.getState().colorPickerTarget;
        if (colorTarget !== null) {
          void pickColorRange(selectedImage.path, colorTarget, x / imgLogicalWidth, y / imgLogicalHeight)
            .then((picked) => {
              if (picked && requestId === whiteBalanceRequestRef.current) onWbPicked();
            })
            .catch((error) => toast.error(`Could not sample the colour: ${error}`));
          return;
        }
        void whiteBalanceForCurrentPreview({
          image: selectedImage, adjustments, finalPreviewUrl,
          normalizedX: x / imgLogicalWidth, normalizedY: y / imgLogicalHeight,
          useWgpu: isWgpuActive,
        }).then((balance) => {
          if (!balance || requestId !== whiteBalanceRequestRef.current) return;
          setAdjustments((prev: Adjustments) => ({
            ...prev,
            ...balance,
          }));
          onWbPicked();
        }).catch((error) => {
          if (useEditorStore.getState().selectedImage === selectedImage && requestId === whiteBalanceRequestRef.current) {
            toast.error(`Could not sample white balance: ${error}`);
          }
        });
      },
      [
        isWbPickerActive,
        selectedImage,
        adjustments,
        isWgpuActive,
        finalPreviewUrl,
        imageRenderSize,
        onWbPicked,
        setAdjustments,
        getCanvasPointer,
      ],
    );

    // Shows the stroke being drawn (the mask overlay and the developed photo
    // beneath it) without committing it: the line so far, grown by `added`
    // (stage coordinates; null starts the stroke).
    const previewLiveStroke = useCallback(
      (line: DrawnLine, added: Coord[] | null) => {
        if (!onLiveMaskPreview || !activeContainer || !activeSubMask || !isBrushActive) return;
        const { scale } = imageRenderSize;
        const toImage = (p: StrokePoint) => withPressure({ x: p.x / scale + cropX, y: p.y / scale + cropY }, p.p);
        let stroke = liveStroke.current;
        if (added && stroke && stroke.subMaskId === activeSubMask.id) {
          stroke.points.push(...added.map(toImage));
        } else {
          stroke = { id: ++nextStrokeId.current, subMaskId: activeSubMask.id, points: line.points.map(toImage) };
          liveStroke.current = stroke;
        }

        const imageSpaceLine: DrawnLine = {
          brushSize: brushImageSpaceSize,
          feather: brushSettings?.feather ? brushSettings?.feather / 100 : 0,
          opacity: strokeOpacity,
          flow: strokeFlow,
          hardness: strokeHardness,
          autoMask: strokeAutoMask,
          autoMaskVersion: strokeAutoMaskVersion,
          dabVersion: DAB_VERSION,
          ...pressureKeys(stroke.points, strokePressure),
          frame: drawingFrame,
          points: stroke.points.slice(),
          tool: line.tool,
        };

        const existingLines = activeSubMask.parameters?.lines || [];
        const previewSubMask = {
          ...activeSubMask,
          parameters: {
            ...activeSubMask.parameters,
            lines: [...existingLines, imageSpaceLine],
          },
        };

        const previewContainer = {
          ...activeContainer,
          subMasks: activeContainer.subMasks.map((sm: SubMask) => (sm.id === activeSubMask.id ? previewSubMask : sm)),
        };

        onLiveMaskPreview(previewContainer, { renderLive: true, stroke: { ...stroke, points: imageSpaceLine.points } });
        notePointerSessionStroke(imageSpaceLine.points.length);
      },
      [
        strokePressure,
        onLiveMaskPreview,
        activeContainer,
        activeSubMask,
        isBrushActive,
        imageRenderSize,
        cropX,
        cropY,
        brushImageSpaceSize,
        brushSettings?.feather,
        strokeOpacity,
        strokeFlow,
        strokeHardness,
        strokeAutoMask,
        strokeAutoMaskVersion,
        drawingFrame,
      ],
    );

    const handleStart = useCallback(
      (e: CanvasPointerEvent) => {
        if (e.evt && typeof e.evt.button === 'number' && e.evt.button !== 0) {
          return;
        }

        if (e.evt && e.evt.cancelable) e.evt.preventDefault();

        if (isGuidedPerspectiveActive && isCropping) {
          if (e.target === e.target.getStage()) {
            setSelectedGuideId(null);
            const stage = e.target.getStage();
            const pos = stage?.getPointerPosition();
            if (!pos || !uncroppedImageRenderSize?.width || !uncroppedImageRenderSize?.height) return;
            const uv = mapScreenToUv(pos.x, pos.y);
            setDraftGuideLine({ p1: uv, p2: uv });
            setDrawing(true);
          }
          return;
        }

        if (isWbPickerActive) {
          handleWbClick(e);
          return;
        }

        if (isLumaPickerActive) {
          const pos = getCanvasPointer(e.target.getStage());
          if (!pos) return;
          const x = pos.x / imageRenderSize.scale + cropX;
          const y = pos.y / imageRenderSize.scale + cropY;
          // As the colour range: a click samples a point, a drag an area;
          // Shift widens the range to take the sample in.
          colorSampleRef.current = { x0: x, y0: y, x1: x, y1: y, add: !!e.evt?.shiftKey, luma: true };
          setColorSampleDraft(null);
          setDrawing(true);
          return;
        }

        if (isParametricActive && activeSubMask) {
          const pos = getCanvasPointer(e.target.getStage());
          if (!pos) return;

          const { scale } = imageRenderSize;
          const x = pos.x / scale + cropX;
          const y = pos.y / scale + cropY;

          // Colour range (Lightroom's): a click samples a point, a drag an
          // area; Shift adds the sample to the others.
          if (activeSubMask.type === Mask.Color) {
            colorSampleRef.current = { x0: x, y0: y, x1: x, y1: y, add: !!e.evt?.shiftKey };
            setColorSampleDraft(null);
            setDrawing(true);
            return;
          }

          const newParams = { ...activeSubMask.parameters };
          newParams.targetX = x;
          newParams.targetY = y;
          newParams.rotation = adjustments.rotation || 0;
          newParams.flipHorizontal = adjustments.flipHorizontal || false;
          newParams.flipVertical = adjustments.flipVertical || false;
          newParams.orientationSteps = adjustments.orientationSteps || 0;

          const activeId = isMasking ? activeMaskId : null;
          updateSubMask(activeId, { parameters: newParams });
          return;
        }

        if (isInitialDrawing && activeSubMask) {
          setDrawing(true);
          drawingStageRef.current = e.target.getStage();
          const pos = getCanvasPointer(e.target.getStage());
          if (!pos) return;

          const { scale } = imageRenderSize;
          dragStartPointer.current = { id: activeSubMask.id, x: pos.x / scale + cropX, y: pos.y / scale + cropY };
          // The shape changes once the press has moved far enough to draw.
          setShapeDraft(null);
          return;
        }

        if (isToolActive) {
          const stage = e.target.getStage();
          const pos = getCanvasPointer(stage);
          if (!pos) {
            setDrawing(false);
            currentLine.current = null;
            setPreviewBox(null);
            previewBoxRef.current = null;
            setIsMaskInteractionActive(false);
            return;
          }

          if (isAiSubjectActive) {
            objectGestureRef.current = promptGesture({
              shiftKey: e.evt.shiftKey,
              altKey: e.evt.altKey || (window as ModifierKeyWindow).altKeyDown,
            });
            setDrawing(true);
            drawingStageRef.current = stage;
            const newBox = { start: pos, end: pos };
            previewBoxRef.current = newBox;
            setPreviewBox(newBox);
            setIsMaskInteractionActive(true);
            return;
          }

          const isAltPressedLocal = e.evt.altKey || (window as ModifierKeyWindow).altKeyDown;
          let effectiveTool;

          if (isAiSubjectActive) {
            effectiveTool = ToolType.AiSeletor;
          } else if (isAltPressedLocal) {
            effectiveTool = baseTool === ToolType.Brush ? ToolType.Eraser : ToolType.Brush;
          } else {
            effectiveTool = baseTool;
          }
          const isShiftClick = isBrushActive && e.evt.shiftKey && lastBrushPoint.current;

          if (isShiftClick) {
            const { scale } = imageRenderSize;
            const startImageSpace = lastBrushPoint.current!;
            const endImageSpace = {
              x: pos.x / scale + cropX,
              y: pos.y / scale + cropY,
            };

            const dx = endImageSpace.x - startImageSpace.x;
            const dy = endImageSpace.y - startImageSpace.y;
            const distance = Math.sqrt(dx * dx + dy * dy);
            const steps = Math.max(Math.ceil(distance), 2);
            const interpolatedPoints: Coord[] = [];
            for (let i = 0; i <= steps; i++) {
              const t = i / steps;
              interpolatedPoints.push({
                x: startImageSpace.x + dx * t,
                y: startImageSpace.y + dy * t,
              });
            }

            const imageSpaceLine: DrawnLine = {
              brushSize: brushImageSpaceSize,
              feather: brushSettings?.feather ? brushSettings?.feather / 100 : 0,
              opacity: strokeOpacity,
              flow: strokeFlow,
              hardness: strokeHardness,
              autoMask: strokeAutoMask,
              autoMaskVersion: strokeAutoMaskVersion,
              dabVersion: DAB_VERSION,
              frame: drawingFrame,
              points: interpolatedPoints,
              tool: effectiveTool,
            };

            const activeId = isMasking ? activeMaskId : null;
            const existingLines = activeSubMask?.parameters?.lines || [];

            updateSubMask(activeId, {
              parameters: {
                ...activeSubMask?.parameters,
                lines: [...existingLines, imageSpaceLine],
              },
            });

            lastBrushPoint.current = endImageSpace;
            setDrawing(false);
            currentLine.current = null;
            return;
          }

          setDrawing(true);
          activeStrokeIndex.current = null;
          drawingStageRef.current = stage;

          const newLine: DrawnLine = {
            brushSize: isBrushActive && brushSettings?.size ? brushStageSize : 2,
            points: [isBrushActive ? withPressure(pos, pointerPressure.current) : pos],
            tool: effectiveTool,
          };
          coalescedMoves.current = [];
          liveStroke.current = null;
          currentLine.current = newLine;
          // The press already shows the stroke's first dab (a click commits
          // just that dab), as Capture One and Lightroom paint on the press.
          if (isBrushActive) previewLiveStroke(newLine, null);
        }
        // A click beside a layer's shapes keeps the tool (Capture One); the
        // select tool or Escape ends it.
      },
      [
        previewLiveStroke,
        isGuidedPerspectiveActive,
        drawingFrame,
        isCropping,
        mapScreenToUv,
        isWbPickerActive,
        handleWbClick,
        isInitialDrawing,
        isBrushActive,
        strokeOpacity,
        strokeFlow,
        strokeHardness,
        strokeAutoMask,
        strokeAutoMaskVersion,
        isAiSubjectActive,
        isParametricActive,
        isLumaPickerActive,
        brushSettings,
        isMasking,
        imageRenderSize,
        adjustments,
        activeMaskId,
        activeSubMask,
        updateSubMask,
        cropX,
        cropY,
        isToolActive,
        brushImageSpaceSize,
        brushStageSize,
        baseTool,
        getCanvasPointer,
        setDrawing,
        uncroppedImageRenderSize?.width,
        uncroppedImageRenderSize?.height,
      ],
    );

    const handleMove = useCallback(
      (e: CanvasMoveEvent) => {
        if (colorSampleRef.current && isDrawing.current) {
          const stage = (e as CanvasPointerEvent).target?.getStage?.() ?? drawingStageRef.current;
          const pos = stage ? getCanvasPointer(stage) : null;
          if (pos) {
            const { scale } = imageRenderSize;
            const sample = colorSampleRef.current;
            sample.x1 = pos.x / scale + cropX;
            sample.y1 = pos.y / scale + cropY;
            const dragged = Math.hypot(sample.x1 - sample.x0, sample.y1 - sample.y0) * scale > 4;
            setColorSampleDraft(dragged ? { x0: sample.x0, y0: sample.y0, x1: sample.x1, y1: sample.y1 } : null);
          }
          return;
        }
        if (isGuidedPerspectiveActive && isCropping && draftGuideLine && isDrawing.current) {
          // A stage event: window moves only arrive here while a mask tool is active.
          const stage = (e as CanvasPointerEvent).target.getStage();
          const pos = stage?.getPointerPosition();
          if (!pos || !uncroppedImageRenderSize?.width || !uncroppedImageRenderSize?.height) return;
          const uv = mapScreenToUv(pos.x, pos.y);
          setDraftGuideLine((prev) => (prev ? { p1: prev.p1, p2: uv } : null));
          if (e.evt && e.evt.cancelable) e.evt.preventDefault();
          return;
        }

        if (isWbPickerActive) {
          return;
        }

        let pos;
        if (e && typeof e.target?.getStage === 'function') {
          const stage = e.target.getStage();
          pos = getCanvasPointer(stage);
        } else if (e && (e.clientX != null || (e.touches && e.touches[0]))) {
          const stage = drawingStageRef.current;
          if (stage) {
            stage.setPointersPositions(e);
            pos = getCanvasPointer(stage);
          }
        }

        if (isToolActive) {
          if (pos) {
            cursorPosition.current = pos;
            const rings = brushCursorRef.current;
            if (rings && cursorPreview.visible) {
              if (!brushGesture) {
                rings.position(pos);
                rings.getLayer()?.batchDraw();
              }
            } else {
              setCursorPreview({ x: pos.x, y: pos.y, visible: true });
            }
          } else {
            setCursorPreview((p: CursorPreview) => ({ ...p, visible: false }));
          }
        }

        if (!isDrawing.current || !isToolActive) {
          return;
        }

        if (isAiSubjectActive && previewBoxRef.current && pos) {
          const updatedBox = { ...previewBoxRef.current, end: pos };
          previewBoxRef.current = updatedBox;
          setPreviewBox(updatedBox);
          if (e.evt && e.evt.cancelable) e.evt.preventDefault();
          return;
        }

        if (isInitialDrawing && dragStartPointer.current && activeSubMask) {
          // A drag belongs to the component it started on.
          if (dragStartPointer.current.id !== activeSubMask.id) return;
          const stage =
            drawingStageRef.current || (e && typeof e.target?.getStage === 'function' ? e.target.getStage() : null);
          if (!stage) return;
          const pointerPos = getCanvasPointer(stage);
          if (!pointerPos) return;

          const { scale } = imageRenderSize;
          const updatedParams = shapeFromDrag(
            activeSubMask.type,
            { ...activeSubMask.parameters, frame: drawingFrame },
            dragStartPointer.current,
            { x: pointerPos.x / scale + cropX, y: pointerPos.y / scale + cropY },
            {
              // Image to screen pixels, viewer zoom included.
              scale: screenPixelsPerImagePixel,
              handle: Math.min(effectiveImageDimensions.width, effectiveImageDimensions.height) * 0.2,
              snap: !!(e?.evt?.shiftKey ?? e?.shiftKey),
              drawing: dragStartPointer.current.drawing,
            },
          );
          if (!updatedParams) return;
          dragStartPointer.current.drawing = true;

          setShapeDraft({ id: activeSubMask.id, params: updatedParams });

          if (onLiveMaskPreview && activeContainer && activeSubMask) {
            const previewSubMask = {
              ...activeSubMask,
              parameters: updatedParams,
            };
            const previewContainer = {
              ...activeContainer,
              subMasks: activeContainer.subMasks.map((sm: SubMask) =>
                sm.id === activeSubMask.id ? previewSubMask : sm,
              ),
            };
            onLiveMaskPreview(previewContainer, renderShapesLive ? { renderLive: true } : undefined);
            if (renderShapesLive) dragStartPointer.current.renderedLive = true;
          }

          const activeId = isMasking ? activeMaskId : null;
          if (activeId && !renderShapesLive) {
            updateSubMask(activeId, { parameters: updatedParams });
          }

          if (e.evt && e.evt.cancelable) e.evt.preventDefault();
          return;
        }

        if (!pos) {
          return;
        }

        if (currentLine.current) {
          // Earlier coalesced positions of this move first, then the
          // delivered one; points closer than 2 px to the last are dropped.
          const passed: StrokePoint[] = [];
          const coalesced = coalescedMoves.current;
          coalescedMoves.current = [];
          const stage = drawingStageRef.current;
          if (coalesced.length && stage && isBrushActive) {
            for (const move of coalesced) {
              stage.setPointersPositions(move);
              const at = getCanvasPointer(stage);
              if (at) passed.push(withPressure(at, pointPressure(move, strokePressure)));
            }
            stage.setPointersPositions(e.evt ?? e);
          }
          passed.push(isBrushActive ? withPressure(pos, pointerPressure.current) : pos);
          const added: StrokePoint[] = [];
          let lastPoint = currentLine.current.points[currentLine.current.points.length - 1];
          for (const point of passed) {
            if (lastPoint) {
              const dx = point.x - lastPoint.x;
              const dy = point.y - lastPoint.y;
              if (dx * dx + dy * dy < 4 && !pressureChanged(lastPoint, point)) continue;
            }
            added.push(point);
            lastPoint = point;
          }
          if (!added.length) {
            notePointerSessionStroke(null);
            if (e.evt && e.evt.cancelable) e.evt.preventDefault();
            return;
          }

          const updatedLine = {
            ...currentLine.current,
            points: [...currentLine.current.points, ...added],
          };
          currentLine.current = updatedLine;

          previewLiveStroke(updatedLine, added);
          if (e.evt && e.evt.cancelable) e.evt.preventDefault();
        }
      },
      [
        strokePressure,
        previewLiveStroke,
        isGuidedPerspectiveActive,
        drawingFrame,
        isCropping,
        draftGuideLine,
        mapScreenToUv,
        isToolActive,
        isWbPickerActive,
        isInitialDrawing,
        activeMaskId,
        updateSubMask,
        onLiveMaskPreview,
        activeContainer,
        activeSubMask,
        isBrushActive,
        strokeOpacity,
        strokeFlow,
        strokeHardness,
        strokeAutoMask,
        strokeAutoMaskVersion,
        isAiSubjectActive,
        imageRenderSize,
        cropX,
        cropY,
        effectiveImageDimensions,
        brushSettings,
        isMasking,
        brushImageSpaceSize,
        getCanvasPointer,
        renderShapesLive,
        screenPixelsPerImagePixel,
        cursorPreview.visible,
        brushGesture,
        uncroppedImageRenderSize?.width,
        uncroppedImageRenderSize?.height,
      ],
    );

    const handleUp = useCallback(() => {
      if (!isDrawing.current) {
        return;
      }

      setIsMaskInteractionActive(false);

      if (colorSampleRef.current?.luma) {
        const { x0, y0, x1, y1, add } = colorSampleRef.current;
        colorSampleRef.current = null;
        setColorSampleDraft(null);
        setDrawing(false);
        const area = Math.hypot(x1 - x0, y1 - y0) * imageRenderSize.scale > 4;
        if (activeContainer) {
          void sampleLumaRange(
            activeContainer.id,
            area
              ? { x: Math.min(x0, x1), y: Math.min(y0, y1), width: Math.abs(x1 - x0), height: Math.abs(y1 - y0) }
              : { x: x0, y: y0 },
            add,
          ).catch((error) => toast.error(`Could not sample the tones: ${error}`));
        }
        return;
      }

      if (colorSampleRef.current && activeSubMask) {
        const { x0, y0, x1, y1, add } = colorSampleRef.current;
        colorSampleRef.current = null;
        setColorSampleDraft(null);
        setDrawing(false);
        const { scale } = imageRenderSize;
        const area = Math.hypot(x1 - x0, y1 - y0) * scale > 4;
        const shown = frameGeometry(adjustments);
        const sample: ColorSample = area
          ? { x: Math.min(x0, x1), y: Math.min(y0, y1), width: Math.abs(x1 - x0), height: Math.abs(y1 - y0), ...shown }
          : { x: x0, y: y0, ...shown };
        const params = activeSubMask.parameters;
        // Earlier samples keep the geometry they were taken in.
        const taken = frameGeometry(params);
        const previous: ColorSample[] = (
          params.samples ??
          (params.targetX !== undefined && params.targetX >= 0 && params.targetY !== undefined
            ? [{ x: params.targetX, y: params.targetY }]
            : [])
        ).map((s: ColorSample) => (s.orientationSteps === undefined ? { ...taken, ...s } : s));
        const samples = add ? [...previous, sample].slice(-MAX_COLOR_SAMPLES) : [sample];
        const center = area ? { x: sample.x + sample.width! / 2, y: sample.y + sample.height! / 2 } : sample;
        const newParams = {
          ...params,
          samples,
          targetX: center.x,
          targetY: center.y,
          // Sampling switches an inherited mask to the measured colour range.
          refine: params.refine ?? DEFAULT_COLOR_RANGE.refine,
          rotation: adjustments.rotation || 0,
          flipHorizontal: adjustments.flipHorizontal || false,
          flipVertical: adjustments.flipVertical || false,
          orientationSteps: adjustments.orientationSteps || 0,
        };
        delete newParams.tolerance;
        updateSubMask(isMasking ? activeMaskId : null, { parameters: newParams });
        return;
      }

      if (isGuidedPerspectiveActive && isCropping && draftGuideLine) {
        setDrawing(false);
        const { p1, p2 } = draftGuideLine;
        setDraftGuideLine(null);

        const sc1 = mapUvToScreen(p1);
        const sc2 = mapUvToScreen(p2);
        const dx = sc2.x - sc1.x;
        const dy = sc2.y - sc1.y;

        const orientation = guideOrientation(dx, dy);
        const rejection = guideRejection(adjustments.guidedPerspective?.lines || [], orientation, Math.hypot(dx, dy));
        if (rejection === 'angle') toast.error(t('editor.guided.toast.angleRejected'));
        else if (rejection === 'full') toast.error(t('editor.guided.toast.maxLines'));
        if (!rejection && orientation) {
          const type: GuideOrientation = orientation;

          setAdjustments((prev) => {
            const existingLines = prev.guidedPerspective?.lines || [];
            if (guideRejection(existingLines, type, Math.hypot(dx, dy))) return prev;

            const newGuide: GuideLine = {
              id: crypto.randomUUID(),
              type,
              p1,
              p2,
            };

            const newLines = [...existingLines, newGuide];

            return {
              ...prev,
              guidedPerspective: {
                ...prev.guidedPerspective,
                enabled: newLines.length >= 2,
                lines: newLines,
                autoCrop: true,
              },
            };
          });
        }
        return;
      }

      if (isInitialDrawing && activeSubMask) {
        setDrawing(false);
        // A click without a drag keeps the shape the tool created; a drag
        // started on another component (the tool changed meanwhile) draws
        // nothing and leaves this one waiting.
        const drawn = draftToCommit(shapeDraft, activeSubMask.id);
        const started = dragStartPointer.current?.id === activeSubMask.id;
        if (drawn) updateSubMask(isMasking ? activeMaskId : null, { parameters: drawn });
        setShapeDraft(null);
        dragStartPointer.current = null;
        if (started) useEditorStore.getState().setEditor({ shapeDrawTarget: null });
        return;
      }

      if (!currentLine.current && !(isAiSubjectActive && previewBoxRef.current)) {
        return;
      }

      if (isAiSubjectActive && previewBoxRef.current) {
        const wasDrawing = isDrawing.current;
        setDrawing(false);
        const box = previewBoxRef.current;
        previewBoxRef.current = null;
        setPreviewBox(null);
        drawingStageRef.current = null;

        if (!wasDrawing || !box) {
          return;
        }

        const scale = imageRenderSize.scale;
        const activeId = isMasking ? activeMaskId : null;

        const startPoint = { x: box.start.x / scale + cropX, y: box.start.y / scale + cropY };
        let endPoint = { x: box.end.x / scale + cropX, y: box.end.y / scale + cropY };

        const dx = box.end.x - box.start.x;
        const dy = box.end.y - box.start.y;
        if (Math.sqrt(dx * dx + dy * dy) < 5) {
          endPoint = { x: startPoint.x, y: startPoint.y };
        }

        if (!activeId || !activeSubMask || !onGenerateAiMask) return;
        // The prompts and their mask are written together when the mask is
        // in (one history step per prompt); a press meanwhile builds on the
        // prompts in flight.
        const base =
          pendingObjectPrompts(activeSubMask.id) ??
          storedPrompts(activeSubMask.parameters, useEditorStore.getState().adjustments);
        const prompts = nextPrompts(
          base,
          promptOf(startPoint, endPoint, objectGestureRef.current === 'exclude'),
          objectGestureRef.current,
        );
        const bounds = prompts && promptBounds(prompts);
        if (!prompts || !bounds) return;
        const draft = { id: activeSubMask.id, prompts };
        setObjectPromptsDraft(draft);
        Promise.resolve(onGenerateAiMask(activeId, bounds.start, bounds.end, prompts)).finally(() =>
          setObjectPromptsDraft((shown) => (shown === draft ? null : shown)),
        );
        return;
      }

      const wasDrawing = isDrawing.current;
      setDrawing(false);
      const line = currentLine.current;
      currentLine.current = null;
      drawingStageRef.current = null;

      if (!wasDrawing || !line) {
        return;
      }

      const scale = imageRenderSize.scale;
      const activeId = isMasking ? activeMaskId : null;

      if (isBrushActive) {
        const imageSpaceLine: DrawnLine = {
          brushSize: brushImageSpaceSize,
          feather: brushSettings?.feather ? brushSettings?.feather / 100 : 0,
          opacity: strokeOpacity,
          flow: strokeFlow,
          hardness: strokeHardness,
          autoMask: strokeAutoMask,
          autoMaskVersion: strokeAutoMaskVersion,
          dabVersion: DAB_VERSION,
          ...pressureKeys(line.points, strokePressure),
          frame: drawingFrame,
          points: line.points.map((p: StrokePoint) =>
            withPressure(
              {
                x: p.x / scale + cropX,
                y: p.y / scale + cropY,
              },
              p.p,
            ),
          ),
          tool: line.tool,
        };

        const existingLines = activeSubMask?.parameters?.lines ? [...activeSubMask.parameters.lines] : [];

        if (activeStrokeIndex.current !== null) {
          existingLines[activeStrokeIndex.current] = imageSpaceLine;
        } else {
          existingLines.push(imageSpaceLine);
        }

        updateSubMask(activeId, {
          parameters: {
            ...activeSubMask?.parameters,
            lines: existingLines,
          },
        });

        activeStrokeIndex.current = null;

        const lastPoint = line.points[line.points.length - 1];
        if (lastPoint) {
          lastBrushPoint.current = {
            x: lastPoint.x / scale + cropX,
            y: lastPoint.y / scale + cropY,
          };
        }
      }
    }, [
      strokePressure,
      isGuidedPerspectiveActive,
      drawingFrame,
      isCropping,
      draftGuideLine,
      setAdjustments,
      isInitialDrawing,
      activeMaskId,
      activeSubMask,
      activeContainer,
      cropX,
      cropY,
      brushSettings,
      imageRenderSize.scale,
      isBrushActive,
      strokeOpacity,
      strokeFlow,
      strokeHardness,
      strokeAutoMask,
      strokeAutoMaskVersion,
      isMasking,
      onGenerateAiMask,
      updateSubMask,
      effectiveImageDimensions,
      shapeDraft,
      brushImageSpaceSize,
      setDrawing,
      isAiSubjectActive,
      mapUvToScreen,
      adjustments.guidedPerspective?.lines,
      t,
    ]);

    // Side handles with a fixed aspect ratio (see cropEdges.ts): handled here,
    // since react-image-crop resizes them as corners and collapses the frame.
    const cropEdgeDragging = useRef(false);
    const startCropEdgeDrag = useCallback(
      (e: React.PointerEvent<HTMLDivElement>) => {
        const ord = (e.target as HTMLElement).closest('[data-ord]')?.getAttribute('data-ord');
        const aspect = adjustments.aspectRatio;
        const media = cropImageRef.current;
        const boxWidth = uncroppedImageRenderSize?.width;
        const boxHeight = uncroppedImageRenderSize?.height;
        if (!isCropEdge(ord) || !aspect || !crop || !boxWidth || !boxHeight || !media) return;
        const box = { width: boxWidth, height: boxHeight };
        e.stopPropagation();
        e.preventDefault();
        const toLayout = (value: number, extent: number) => (crop.unit === '%' ? (value / 100) * extent : value);
        const start: CropRect = {
          x: toLayout(crop.x, box.width),
          y: toLayout(crop.y, box.height),
          width: toLayout(crop.width, box.width),
          height: toLayout(crop.height, box.height),
        };
        // Pointer deltas are on screen; the frame is in layout pixels (zoom).
        const screenPerLayout = media.getBoundingClientRect().width / box.width || 1;
        const origin = { x: e.clientX, y: e.clientY };
        let latest: { px: Crop; percent: PercentCrop } | null = null;
        cropEdgeDragging.current = true;
        setIsCropDragging(true);
        cropGesture.start();
        const move = (event: PointerEvent) => {
          event.preventDefault();
          const rect = resizeCropFromEdge(
            start,
            ord,
            (event.clientX - origin.x) / screenPerLayout,
            (event.clientY - origin.y) / screenPerLayout,
            aspect,
            box.width,
            box.height,
          );
          latest = {
            px: { unit: 'px', ...rect },
            percent: {
              unit: '%',
              x: (rect.x / box.width) * 100,
              y: (rect.y / box.height) * 100,
              width: (rect.width / box.width) * 100,
              height: (rect.height / box.height) * 100,
            },
          };
          setCrop(latest.px, latest.percent);
        };
        const end = () => {
          window.removeEventListener('pointermove', move, true);
          window.removeEventListener('pointerup', end, true);
          window.removeEventListener('pointercancel', end, true);
          cropEdgeDragging.current = false;
          setIsCropDragging(false);
          if (latest) handleCropComplete(latest.px, latest.percent);
          cropGesture.end();
        };
        window.addEventListener('pointermove', move, true);
        window.addEventListener('pointerup', end, true);
        window.addEventListener('pointercancel', end, true);
      },
      [adjustments.aspectRatio, uncroppedImageRenderSize, crop, setCrop, handleCropComplete, cropGesture],
    );

    // Lightroom / Capture One: dragging just outside the crop frame (or beside
    // the photo) rotates it around the frame's centre, live, committed on release.
    const cropLayerRef = useRef<HTMLDivElement>(null);
    const cropBoxRef = useRef<HTMLDivElement>(null);
    const [rotateHover, setRotateHover] = useState(false);
    const cropRotationCentre = useCallback(
      (x: number, y: number, target: EventTarget | null) => {
        if (isStraightenActive || isGuidedPerspectiveActive) return null;
        if ((target as Element | null)?.closest?.('.ReactCrop__drag-handle, .ReactCrop__drag-bar')) return null;
        const selection = cropLayerRef.current?.querySelector('.ReactCrop__crop-selection')?.getBoundingClientRect();
        const box = cropBoxRef.current?.getBoundingClientRect();
        if (!selection || !box || !selection.width) return null;
        const dx = Math.max(selection.left - x, 0, x - selection.right);
        const dy = Math.max(selection.top - y, 0, y - selection.bottom);
        if (dx === 0 && dy === 0) return null;
        const besidePhoto = x < box.left || x > box.right || y < box.top || y > box.bottom;
        if (!besidePhoto && Math.hypot(dx, dy) > CROP_ROTATE_BAND) return null;
        return { x: (selection.left + selection.right) / 2, y: (selection.top + selection.bottom) / 2 };
      },
      [isStraightenActive, isGuidedPerspectiveActive],
    );
    const startCropRotation = useCallback(
      (e: React.PointerEvent<HTMLDivElement>) => {
        if (e.button !== 0 || !selectedImage?.isReady) return;
        const centre = cropRotationCentre(e.clientX, e.clientY, e.target);
        if (!centre) return;
        e.stopPropagation();
        e.preventDefault();
        const angleAt = (x: number, y: number) => (Math.atan2(y - centre.y, x - centre.x) * 180) / Math.PI;
        const startAngle = angleAt(e.clientX, e.clientY);
        const startRotation = adjustments.rotation || 0;
        let latest = startRotation;
        const { setEditor } = useEditorStore.getState();
        setEditor({ isRotationActive: true, liveRotation: startRotation });
        const move = (event: PointerEvent) => {
          event.preventDefault();
          let delta = angleAt(event.clientX, event.clientY) - startAngle;
          delta = ((((delta + 180) % 360) + 360) % 360) - 180;
          latest = Math.round(Math.max(-45, Math.min(45, startRotation + delta)) * 10) / 10;
          setEditor({ liveRotation: latest });
        };
        const end = () => {
          window.removeEventListener('pointermove', move, true);
          window.removeEventListener('pointerup', end, true);
          window.removeEventListener('pointercancel', end, true);
          setEditor({ isRotationActive: false, liveRotation: null });
          if (latest !== startRotation) setAdjustments((prev: Adjustments) => ({ ...prev, rotation: latest }));
        };
        window.addEventListener('pointermove', move, true);
        window.addEventListener('pointerup', end, true);
        window.addEventListener('pointercancel', end, true);
      },
      [cropRotationCentre, adjustments.rotation, selectedImage?.isReady, setAdjustments],
    );

    // Gradient lines and handles show while the pointer is over the photo
    // (or a shape is being dragged); outside it only the mask overlay remains.
    const [pointerOverImage, setPointerOverImage] = useState(false);
    const handleMouseEnter = useCallback(() => {
      setPointerOverImage(true);
      if (isToolActive) {
        setCursorPreview({ ...cursorPosition.current, visible: true });
      }
    }, [isToolActive]);

    const handleMouseLeave = useCallback(() => {
      setPointerOverImage(false);
      setCursorPreview((p: CursorPreview) => ({ ...p, visible: false }));
    }, []);

    // A pointermove precedes the mousemove handleMove receives; it carries
    // the coalesced positions (WebKit and Chromium, where supported).
    useEffect(() => {
      if (!isBrushActive) return;
      const onPointerDown = (e: PointerEvent) => {
        pointerPressure.current = pointPressure(e, strokePressure);
      };
      const onPointerMove = (e: PointerEvent) => {
        pointerPressure.current = pointPressure(e, strokePressure);
        if (!isDrawing.current) return;
        const events = typeof e.getCoalescedEvents === 'function' ? e.getCoalescedEvents() : [];
        // The last coalesced event is the delivered one.
        coalescedMoves.current = events.slice(0, -1);
      };
      window.addEventListener('pointerdown', onPointerDown, true);
      window.addEventListener('pointermove', onPointerMove, true);
      return () => {
        window.removeEventListener('pointerdown', onPointerDown, true);
        window.removeEventListener('pointermove', onPointerMove, true);
        coalescedMoves.current = [];
        pointerPressure.current = undefined;
      };
    }, [isBrushActive, strokePressure]);

    useEffect(() => {
      if (!isToolActive) return;

      // Continues a stroke or shape outside the stage. Moves over the stage
      // already reach handleMove through Konva; handling them twice drew each
      // initial radial/linear move twice.
      function onGlobalMove(e: MouseEvent | TouchEvent) {
        if (!isDrawing.current) return;
        const container = drawingStageRef.current?.container?.();
        if (container && e.target instanceof Node && container.contains(e.target)) return;
        handleMove(e);
      }

      function onGlobalUp() {
        if (!isDrawing.current) return;
        handleUp();
      }

      window.addEventListener('mousemove', onGlobalMove, { passive: false });
      window.addEventListener('mouseup', onGlobalUp);
      window.addEventListener('touchmove', onGlobalMove, { passive: false });
      window.addEventListener('touchcancel', onGlobalUp);
      return () => {
        window.removeEventListener('mousemove', onGlobalMove);
        window.removeEventListener('mouseup', onGlobalUp);
        window.removeEventListener('touchmove', onGlobalMove);
        window.removeEventListener('touchcancel', onGlobalUp);
      };
    }, [isToolActive, handleMove, handleUp]);

    const handleStraightenMouseDown = (e: CanvasPointerEvent) => {
      if (e.evt.button !== 0 && !e.evt.touches) {
        return;
      }

      isStraightening.current = true;
      // A line being drawn changes the rotation on release (Auto's pending
      // answer is out of date then).
      lineGesture.start();
      const pos = e.target.getStage()!.getPointerPosition()!;
      setStraightenLine({ start: pos, end: pos });
    };

    const handleStraightenMouseMove = (e: CanvasPointerEvent) => {
      if (!isStraightening.current) {
        return;
      }

      const pos = e.target.getStage()!.getPointerPosition()!;
      setStraightenLine((prev) => ({ ...prev!, end: pos }));
      if (e.evt && e.evt.cancelable) e.evt.preventDefault();
    };

    const handleStraightenMouseUp = () => {
      if (!isStraightening.current) {
        return;
      }
      isStraightening.current = false;
      lineGesture.end();
      if (
        !straightenLine ||
        (straightenLine.start.x === straightenLine.end.x && straightenLine.start.y === straightenLine.end.y)
      ) {
        setStraightenLine(null);
        return;
      }

      const { start, end } = straightenLine;
      const correction = calculateStraightenAngle(end.x - start.x, end.y - start.y);

      onStraighten(correction);
      setStraightenLine(null);
    };

    const handleStraightenMouseLeave = () => {
      if (isStraightening.current) {
        isStraightening.current = false;
        lineGesture.end();
        setStraightenLine(null);
      }
    };

    const cropPreviewUrl = uncroppedAdjustedPreviewUrl || selectedImage.thumbnailUrl;
    const isShowingOriginal = showOriginal;

    const currentTarget = finalPreviewUrl || selectedImage.thumbnailUrl;
    const baseIsReady = displayBase === currentTarget;

    const visiblePatch = interactivePatch ?? (baseIsReady ? null : retainedPatchRef.current);

    useEffect(() => {
      if (baseIsReady && !interactivePatch) {
        retainedPatchRef.current = null;
        // The final frame is on screen: the last drag draft goes.
        clearDraftPatch();
      }
    }, [baseIsReady, interactivePatch]);
    // Another photo never shows this one's draft.
    useEffect(() => clearDraftPatch, [selectedImage.path]);

    const cropImageTransforms = useMemo(() => {
      const rotation = liveRotation !== null && liveRotation !== undefined ? liveRotation : adjustments.rotation || 0;
      return `rotate(${rotation}deg)`;
    }, [adjustments.rotation, liveRotation]);

    const getCropDimensions = () => {
      if (!crop || !uncroppedImageRenderSize?.width || !uncroppedImageRenderSize?.height) {
        return { width: 0, height: 0 };
      }

      const width = crop.unit === '%' ? uncroppedImageRenderSize.width * (crop.width / 100) : crop.width;
      const height = crop.unit === '%' ? uncroppedImageRenderSize.height * (crop.height / 100) : crop.height;

      return { width, height };
    };

    const effectiveCursor = useMemo(() => {
      if (isGuidedPerspectiveActive && isCropping) return 'crosshair';
      if (isWbPickerActive) return 'crosshair';
      if (isParametricActive) return 'crosshair';
      if (isInitialDrawing) return 'crosshair';

      if (isBrushActive) return 'none';

      if (isAiSubjectActive) return 'crosshair';

      return cursorStyle;
    }, [
      isGuidedPerspectiveActive,
      isCropping,
      isWbPickerActive,
      isInitialDrawing,
      isBrushActive,
      isAiSubjectActive,
      isParametricActive,
      cursorStyle,
    ]);

    const handlePreviewUpdate = useCallback(
      (id: string, subMaskPreview: Partial<SubMask>) => {
        if (!activeContainer || !onLiveMaskPreview) return;
        const previewContainer = {
          ...activeContainer,
          subMasks: activeContainer.subMasks.map((sm: SubMask) => (sm.id === id ? { ...sm, ...subMaskPreview } : sm)),
        };
        onLiveMaskPreview(previewContainer, renderShapesLive ? { renderLive: true } : undefined);
      },
      [activeContainer, onLiveMaskPreview, renderShapesLive],
    );

    const handleMaskInteractionStart = useCallback(
      (e?: KonvaEventObject<Event>) => {
        setIsMaskInteractionActive(true);
        const eventType = e?.evt?.type;
        if (eventType === 'touchstart') {
          setIsMaskTouchInteracting(true);
        }
      },
      [setIsMaskTouchInteracting],
    );

    const handleMaskInteractionEnd = useCallback(() => {
      setIsMaskInteractionActive(false);
      setIsMaskTouchInteracting(false);
    }, [setIsMaskTouchInteracting]);

    const maskOpacity = maskOverlayOpacity({
      display: maskDisplayOf(appSettings),
      showingOriginal: isShowingOriginal,
      sliderDragging: isSliderDragging,
      masking: isMasking,
      drawing: isStrokeActive,
      shapeInteracting: isMaskInteractionActive,
      // The overlay belongs to the mask tools: none with the select tool.
      toolActive: !isMasking || Boolean(activeMaskId) || showMaskWithSelectTool,
      controlHovered: isMaskControlHovered,
      rangeDragging: isMaskRangeDragging,
      // Off after the layer's first adjustment, as in Lightroom (notes:
      // reference/layers-panel-c1-lr.md); painting or M shows it again.
      autoHidden: maskOverlayAutoHidden,
    });

    const nativeOverlay = drawsOverlayNatively
      ? nativeMaskOverlay(activeContainer?.id, maskOpacity === 1, appSettings)
      : null;
    useEffect(() => {
      const { nativeMaskOverlay: current, setEditor } = useEditorStore.getState();
      if (!sameNativeMaskOverlay(current, nativeOverlay)) setEditor({ nativeMaskOverlay: nativeOverlay });
    }, [nativeOverlay]);
    useEffect(() => () => useEditorStore.getState().setEditor({ nativeMaskOverlay: null }), []);

    return (
      <div ref={canvasRootRef} className="relative" style={{ width: '100%', height: '100%', cursor: effectiveCursor }}>
        {brushGesture && (
          // Capture One's readout beside the rings while a gesture changes them.
          <div
            className="brush-gesture-readout"
            style={{
              left:
                brushGesture.x +
                (brushGesture.values.size / 2) * screenPixelsPerImagePixel * (2 - brushGesture.values.hardness / 100) +
                14,
              top: brushGesture.y - 34,
            }}
          >
            {(
              [
                ['Size', brushGesture.values.size],
                ['Hardness', brushGesture.values.hardness],
                ['Opacity', brushGesture.values.opacity],
                ['Flow', brushGesture.values.flow],
              ] as const
            ).map(([label, value]) => (
              <div key={label}>
                <span>{label}</span>
                <span>{value}</span>
              </div>
            ))}
          </div>
        )}
        <div
          className="absolute inset-0 w-full h-full transition-opacity duration-200 flex items-center justify-center"
          style={{
            opacity: cropLayerVisible ? 0 : 1,
            pointerEvents: cropLayerVisible ? 'none' : 'auto',
          }}
        >
          <div
            className="opacity-100"
            style={{
              height: '100%',
              position: 'relative',
              width: '100%',
            }}
          >
            <div className="absolute inset-0 w-full h-full">
              <svg
                className="pointer-events-none"
                style={
                  imageRenderSize.width > 0 && imageRenderSize.height > 0
                    ? {
                        position: 'absolute',
                        left: `${imageRenderSize.offsetX}px`,
                        top: `${imageRenderSize.offsetY}px`,
                        width: `${imageRenderSize.width}px`,
                        height: `${imageRenderSize.height}px`,
                        overflow: 'visible',
                      }
                    : {
                        position: 'absolute',
                        inset: '0px',
                        width: '100%',
                        height: '100%',
                        overflow: 'visible',
                      }
                }
              >
                {displayBase && !isWgpuActive && !holdingNativeFrame && !hasRenderedFirstFrame && (
                  <image
                    href={displayBase}
                    onLoad={() => {
                      reportImagePlaceholderLoad(selectedImage.path, displayBase);
                      reportBrowserPreviewLoad(displayBase, true);
                    }}
                    x="0"
                    y="0"
                    width="100%"
                    height="100%"
                    style={{ imageRendering: isMaxZoom ? 'pixelated' : 'auto' }}
                  />
                )}

                {visiblePatch && !isWgpuActive && (
                  <image
                    href={visiblePatch.url}
                    onLoad={() => reportBrowserPreviewLoad(visiblePatch.url, true)}
                    x={`${visiblePatch.normX * 100}%`}
                    y={`${visiblePatch.normY * 100}%`}
                    width={`${visiblePatch.normW * 100}%`}
                    height={`${visiblePatch.normH * 100}%`}
                    preserveAspectRatio="none"
                    style={{ imageRendering: isMaxZoom ? 'pixelated' : 'auto' }}
                  />
                )}
                <BrowserSplitImage path={selectedImage.path} adjustments={adjustments} size={imageRenderSize}
                  transform={transformState} enabled={!isWgpuActive && !isCropping && canRenderSource(selectedImage)} decoded={selectedImage.isDecoded} />
              </svg>
              {/* Slider-drag drafts of the browser renderer (utils/browserDraftPatch.ts). */}
              <div
                ref={registerDraftPatchHost}
                className="pointer-events-none"
                style={{
                  position: 'absolute',
                  left: `${imageRenderSize.offsetX}px`,
                  top: `${imageRenderSize.offsetY}px`,
                  width: `${imageRenderSize.width}px`,
                  height: `${imageRenderSize.height}px`,
                  overflow: 'hidden',
                  // Its own compositing layer: a new draft repaints only
                  // itself, not the viewer's layer under it (WebKitGTK
                  // painted ~20 % of its main thread during drags; notes:
                  // parity-gaps/G55-performance.md).
                  willChange: 'transform',
                }}
              />

              {displayedMaskUrl && (
                <img
                  alt="Mask Overlay"
                  className="absolute object-contain pointer-events-none"
                  src={displayedMaskUrl}
                  onLoad={(event) => reportMaskOverlayLoad(event.currentTarget)}
                  style={{
                    height: `${imageRenderSize.height}px`,
                    left: `${imageRenderSize.offsetX}px`,
                    opacity: maskOpacity,
                    top: `${imageRenderSize.offsetY}px`,
                    // Appear at once when a stroke or drag starts; fade only when hiding.
                    transition: maskOpacity ? 'none' : 'opacity 300ms ease-in-out',
                    width: `${imageRenderSize.width}px`,
                    imageRendering: isMaxZoom ? 'pixelated' : 'auto',
                    zIndex: 3,
                  }}
                />
              )}
            </div>

          </div>

          {isSpotToolActive && redEyeMode && !isCropping && !isMasking && !isWbPickerActive && !isShowingOriginal && (
            <RedEyeOverlay
              rect={{
                left: imageRenderSize.offsetX,
                top: imageRenderSize.offsetY,
                width: imageRenderSize.width,
                height: imageRenderSize.height,
              }}
              cropX={cropX}
              cropY={cropY}
              scale={imageRenderSize.scale || 1}
              zoom={effectiveZoomScale}
            />
          )}
          {isSpotToolActive && !redEyeMode && !isCropping && !isMasking && !isWbPickerActive && !isShowingOriginal && (
            <SpotOverlay
              rect={{
                left: imageRenderSize.offsetX,
                top: imageRenderSize.offsetY,
                width: imageRenderSize.width,
                height: imageRenderSize.height,
              }}
              cropX={cropX}
              cropY={cropY}
              scale={imageRenderSize.scale || 1}
              zoom={effectiveZoomScale}
            />
          )}
          {showFaceBlemishes &&
            !isSpotToolActive &&
            !isCropping &&
            !isMasking &&
            !isWbPickerActive &&
            !isShowingOriginal && (
              <FaceRetouchOverlay
                rect={{
                  left: imageRenderSize.offsetX,
                  top: imageRenderSize.offsetY,
                  width: imageRenderSize.width,
                  height: imageRenderSize.height,
                }}
                cropX={cropX}
                cropY={cropY}
                scale={imageRenderSize.scale || 1}
                zoom={effectiveZoomScale}
              />
            )}
          {relightHandles && selectedImage.isReady && !holdingNativeFrame && adjustments.sectionVisibility?.relight !== false
            && !isCropping && !isMasking && !isWbPickerActive && !isShowingOriginal && !isSpotToolActive && !tatTarget && !showFaceBlemishes && (
            <RelightOverlay key={selectedImage.path} rect={{ left: imageRenderSize.offsetX, top: imageRenderSize.offsetY,
              width: imageRenderSize.width, height: imageRenderSize.height }} zoom={effectiveZoomScale} />
          )}
          {tatTarget && !isCropping && !isMasking && !isWbPickerActive && !isShowingOriginal && (
            <TargetedAdjustmentOverlay
              rect={{
                left: imageRenderSize.offsetX,
                top: imageRenderSize.offsetY,
                width: imageRenderSize.width,
                height: imageRenderSize.height,
              }}
            />
          )}
          {(isMasking || isWbPickerActive) && (
            <div
              style={{
                position: 'absolute',
                top: stageTop,
                left: stageLeft,
                transformOrigin: '0 0',
                transform: `scale(${1 / maxSafeScale})`,
                width: stageWidth * maxSafeScale,
                height: stageHeight * maxSafeScale,
                zIndex: 4,
                touchAction: 'none',
                userSelect: 'none',
                opacity: isShowingOriginal ? 0 : 1,
                transition: 'opacity 150ms ease-in-out',
                ...getEdgeFadeStyle(128),
              }}
            >
              <Stage
                ref={maskStageRef}
                width={stageWidth * maxSafeScale}
                height={stageHeight * maxSafeScale}
                onMouseDown={handleStart}
                onTouchStart={handleStart}
                onMouseEnter={handleMouseEnter}
                onMouseLeave={handleMouseLeave}
                onMouseMove={handleMove}
                onTouchMove={handleMove}
                onMouseUp={handleUp}
                onTouchEnd={handleUp}
              >
                <Layer listening={!showOriginal}>
                  <Group scaleX={maxSafeScale} scaleY={maxSafeScale}>
                    <Group x={groupOffsetX} y={groupOffsetY}>
                      {/* A layer's shapes are edited with its mask tools; with the
                          select tool (no component active) they are neither drawn
                          nor clickable, as in Capture One. */}
                      {isMasking &&
                        activeContainer &&
                        activeMaskId &&
                        sortedSubMasks.map((subMask: SubMask) => {
                          const activeId = activeMaskId;
                          const renderSubMask =
                            subMask.id === activeId && shapeDraft?.id === subMask.id
                              ? { ...subMask, parameters: shapeDraft.params }
                              : subMask;

                          if (
                            (renderSubMask.type === Mask.Linear || renderSubMask.type === Mask.Radial) &&
                            !pointerOverImage &&
                            !isMaskInteractionActive &&
                            !isDrawing.current
                          ) {
                            return null;
                          }

                          return (
                            <Group
                              key={renderSubMask.id}
                              name={HANDLELESS_MASKS.has(renderSubMask.type) ? undefined : 'mask-handles'}
                            >
                              <MaskOverlay
                                adjustments={adjustments}
                                imageHeight={effectiveImageDimensions.height}
                                imageWidth={effectiveImageDimensions.width}
                                isSelected={renderSubMask.id === activeId}
                                isToolActive={isToolActive}
                                isBeingDrawn={isInitialDrawing && renderSubMask.id === activeId}
                                onMaskInteractionEnd={handleMaskInteractionEnd}
                                onMaskInteractionStart={handleMaskInteractionStart}
                                onMaskMouseEnter={() => !isToolActive && setIsMaskHovered(true)}
                                onMaskMouseLeave={() => !isToolActive && setIsMaskHovered(false)}
                                onPreviewUpdate={handlePreviewUpdate}
                                commitWhileDragging={!renderShapesLive}
                                onSelect={() => onSelectMask(renderSubMask.id)}
                                onUpdate={updateSubMask}
                                scale={imageRenderSize.scale}
                                subMask={renderSubMask}
                                offsetX={groupOffsetX}
                                offsetY={groupOffsetY}
                                stageScale={maxSafeScale}
                                sampleDraft={
                                  renderSubMask.id === activeId && !isLumaPickerActive ? colorSampleDraft : null
                                }
                                drawingFrame={drawingFrame}
                              />
                            </Group>
                          );
                        })}

                      {isLumaPickerActive && colorSampleDraft && (
                        <Rect
                          x={(Math.min(colorSampleDraft.x0, colorSampleDraft.x1) - cropX) * imageRenderSize.scale}
                          y={(Math.min(colorSampleDraft.y0, colorSampleDraft.y1) - cropY) * imageRenderSize.scale}
                          width={Math.abs(colorSampleDraft.x1 - colorSampleDraft.x0) * imageRenderSize.scale}
                          height={Math.abs(colorSampleDraft.y1 - colorSampleDraft.y0) * imageRenderSize.scale}
                          stroke="#0ea5e9"
                          dash={[4, 3]}
                          strokeWidth={1.5}
                          listening={false}
                        />
                      )}
                      {isAiSubjectActive &&
                        activeSubMask &&
                        (objectPromptsDraft?.id === activeSubMask.id
                          ? objectPromptsDraft.prompts
                          : storedPrompts(activeSubMask.parameters, adjustments)
                        ).map((prompt, index) => {
                          const color = prompt.exclude ? '#ef4444' : '#22c55e';
                          const { scale } = imageRenderSize;
                          return prompt.type === 'point' ? (
                            <Circle
                              key={index}
                              x={(prompt.x - cropX) * scale}
                              y={(prompt.y - cropY) * scale}
                              radius={5 / maxSafeScale}
                              fill={color}
                              stroke="white"
                              strokeWidth={1.5 / maxSafeScale}
                              listening={false}
                            />
                          ) : (
                            <Rect
                              key={index}
                              x={(Math.min(prompt.startX, prompt.endX) - cropX) * scale}
                              y={(Math.min(prompt.startY, prompt.endY) - cropY) * scale}
                              width={Math.abs(prompt.endX - prompt.startX) * scale}
                              height={Math.abs(prompt.endY - prompt.startY) * scale}
                              stroke={color}
                              strokeWidth={1.5 / maxSafeScale}
                              dash={[4 / maxSafeScale, 3 / maxSafeScale]}
                              listening={false}
                            />
                          );
                        })}
                      {previewBox && (
                        <Rect
                          x={Math.min(previewBox.start.x, previewBox.end.x)}
                          y={Math.min(previewBox.start.y, previewBox.end.y)}
                          width={Math.max(0.1, Math.abs(previewBox.end.x - previewBox.start.x))}
                          height={Math.max(0.1, Math.abs(previewBox.end.y - previewBox.start.y))}
                          stroke="#0ea5e9"
                          strokeWidth={2}
                          dash={[4, 4]}
                          listening={false}
                        />
                      )}
                    </Group>
                  </Group>
                </Layer>
                {/* The brush rings on their own canvas: moving them redraws only it. */}
                <Layer listening={false}>
                  <Group scaleX={maxSafeScale} scaleY={maxSafeScale}>
                    <Group x={groupOffsetX} y={groupOffsetY}>
                      {isBrushActive &&
                        cursorPreview.visible && (
                          <Group
                            ref={brushCursorRef}
                            listening={false}
                            x={(brushGesture ? gestureAnchor ?? cursorPreview : cursorPreview).x}
                            y={(brushGesture ? gestureAnchor ?? cursorPreview : cursorPreview).y}
                          >
                            {[brushCursorRings.outer, brushCursorRings.inner].map((radius, ring) =>
                              ring === 0 && Math.abs(brushCursorRings.outer - brushCursorRings.inner) < 0.5 ? null : (
                                <Group key={ring}>
                                  <Circle radius={radius} stroke="rgba(0, 0, 0, 0.45)" strokeWidth={3} strokeScaleEnabled={false} perfectDrawEnabled={false} />
                                  <Circle
                                    radius={radius}
                                    stroke={ring === 0 ? 'rgba(255, 255, 255, 0.6)' : 'rgba(255, 255, 255, 0.95)'}
                                    strokeWidth={ring === 0 ? 1 : 1.5}
                                    strokeScaleEnabled={false}
                                    perfectDrawEnabled={false}
                                  />
                                </Group>
                              ),
                            )}
                            {/* A plus for drawing, a minus for erasing. */}
                            <Line points={[-4 / effectiveZoomScale, 0, 4 / effectiveZoomScale, 0]} stroke="white" strokeWidth={1} strokeScaleEnabled={false} />
                            {!brushCursorRings.isEraser && (
                              <Line points={[0, -4 / effectiveZoomScale, 0, 4 / effectiveZoomScale]} stroke="white" strokeWidth={1} strokeScaleEnabled={false} />
                            )}
                          </Group>
                        )}
                    </Group>
                  </Group>
                </Layer>
              </Stage>
            </div>
          )}
        </div>

        <div
          className={`absolute inset-0 w-full h-full flex items-center justify-center ${
            rotateHover ? 'crop-rotate-cursor' : ''
          }`}
          ref={cropLayerRef}
          onPointerDownCapture={startCropRotation}
          onPointerMove={(e) => {
            if (e.buttons === 0) setRotateHover(Boolean(cropRotationCentre(e.clientX, e.clientY, e.target)));
          }}
          onPointerLeave={() => setRotateHover(false)}
          style={{
            opacity: cropLayerVisible ? 1 : 0,
            pointerEvents: cropLayerVisible ? 'auto' : 'none',
            // Fades in; leaving crop hides it at once (in the same render), as
            // the display then already shows the cropped photo.
            transition: cropLayerVisible ? 'opacity 100ms' : 'none',
          }}
        >
          {(cropPreviewUrl || isWgpuActive) && uncroppedImageRenderSize && (
            <div
              ref={cropBoxRef}
              style={{
                height: uncroppedImageRenderSize.height,
                position: 'relative',
                width: uncroppedImageRenderSize.width,
              }}
              onPointerDownCapture={(e) => {
                if (e.button !== 0) {
                  e.stopPropagation();
                  return;
                }
                startCropEdgeDrag(e);
              }}
              onMouseDownCapture={(e) => {
                if (e.button !== 0 || cropEdgeDragging.current) {
                  e.stopPropagation();
                }
              }}
            >
              <ReactCrop
                aspect={adjustments.aspectRatio ?? undefined}
                crop={crop ?? undefined}
                onChange={setCrop}
                onComplete={handleCropComplete}
                onDragStart={() => {
                  setIsCropDragging(true);
                  cropGesture.start();
                }}
                onDragEnd={() => {
                  setIsCropDragging(false);
                  cropGesture.end();
                }}
                style={{ '--crop-shade': cropShade } as React.CSSProperties}
                ruleOfThirds={false}
                renderSelectionAddon={() => {
                  const { width, height } = getCropDimensions();
                  if (width <= 0 || height <= 0) {
                    return null;
                  }
                  const showDenseGrid = isRotationActive && !isStraightenActive && !isGuidedPerspectiveActive;
                  const gridVisible = cropGrid === 'always' || (cropGrid === 'drag' && isCropDragging);
                  const currentOverlayMode =
                    isRotationActive || isStraightenActive || isGuidedPerspectiveActive || !gridVisible
                      ? 'none'
                      : overlayMode || 'none';
                  const steps = adjustments.orientationSteps || 0;
                  const orientedW = steps % 2 === 1 ? selectedImage.height : selectedImage.width;
                  const orientedH = steps % 2 === 1 ? selectedImage.width : selectedImage.height;
                  const sizeLabel =
                    crop && crop.unit === '%'
                      ? `${Math.round((crop.width / 100) * orientedW)} × ${Math.round((crop.height / 100) * orientedH)}`
                      : crop
                        ? `${Math.round(crop.width)} × ${Math.round(crop.height)}`
                        : '';
                  return (
                    <>
                      <CompositionOverlays
                        width={width}
                        height={height}
                        mode={currentOverlayMode}
                        rotation={overlayRotation || 0}
                        denseVisible={showDenseGrid}
                      />
                      {isCropDragging && sizeLabel && (
                        <div className="crop-size-label">{sizeLabel} px</div>
                      )}
                    </>
                  );
                }}
              >
                {/* With the native display this only sizes the crop box; the
                    photo itself is drawn natively underneath. */}
                <img
                  alt="Crop preview"
                  ref={cropImageRef}
                  src={isWgpuActive ? TRANSPARENT_PIXEL : cropPreviewUrl}
                  style={{
                    display: 'block',
                    width: `${uncroppedImageRenderSize.width}px`,
                    height: `${uncroppedImageRenderSize.height}px`,
                    objectFit: 'contain',
                    transform: cropImageTransforms,
                    imageRendering: isMaxZoom ? 'pixelated' : 'auto',
                  }}
                />
              </ReactCrop>

              {/* Guides show only while the Guided Upright tool is active, as
                  in Lightroom; automatic modes never show their lines. */}
              {(isStraightenActive || isGuidedPerspectiveActive) && (
                <Stage
                  height={uncroppedImageRenderSize.height}
                  onMouseDown={isStraightenActive ? handleStraightenMouseDown : handleStart}
                  onTouchStart={isStraightenActive ? handleStraightenMouseDown : handleStart}
                  onMouseLeave={isStraightenActive ? handleStraightenMouseLeave : handleMouseLeave}
                  onMouseMove={isStraightenActive ? handleStraightenMouseMove : handleMove}
                  onTouchMove={isStraightenActive ? handleStraightenMouseMove : handleMove}
                  onMouseUp={isStraightenActive ? handleStraightenMouseUp : handleUp}
                  onTouchEnd={isStraightenActive ? handleStraightenMouseUp : handleUp}
                  style={{
                    position: 'absolute',
                    top: 0,
                    left: 0,
                    zIndex: 10,
                    cursor: isGuidedPerspectiveActive || isStraightenActive ? 'crosshair' : 'default',
                    touchAction: 'none',
                    pointerEvents: isStraightenActive || isGuidedPerspectiveActive ? 'auto' : 'none',
                  }}
                  width={uncroppedImageRenderSize.width}
                >
                  <Layer>
                    {straightenLine && (
                      <Line
                        dash={[4, 4]}
                        listening={false}
                        points={[
                          straightenLine.start.x,
                          straightenLine.start.y,
                          straightenLine.end.x,
                          straightenLine.end.y,
                        ]}
                        stroke="#0ea5e9"
                        strokeWidth={2}
                      />
                    )}

                    {(localDragLines || adjustments.guidedPerspective?.lines || []).map((line: GuideLine) => {
                      const sc1 = mapUvToScreen(line.p1);
                      const sc2 = mapUvToScreen(line.p2);
                      const color = GUIDE_COLORS[line.type];
                      const selected = line.id === selectedGuideId;
                      return (
                        <Group key={line.id}>
                          <Line
                            points={[sc1.x, sc1.y, sc2.x, sc2.y]}
                            stroke={color}
                            strokeWidth={selected ? 3 : 2}
                            hitStrokeWidth={12}
                            dash={selected ? undefined : [6, 4]}
                            shadowColor="#000"
                            shadowBlur={2}
                            shadowOpacity={0.6}
                            onMouseDown={(e) => {
                              // Selects the guide (Delete removes it) instead of drawing a new one.
                              e.cancelBubble = true;
                              setSelectedGuideId(line.id);
                            }}
                            onContextMenu={(e) => {
                              e.evt.preventDefault();
                              e.cancelBubble = true;
                              setSelectedGuideId(line.id);
                              showContextMenu(e.evt.clientX, e.evt.clientY, [
                                { label: 'Remove Guide', onClick: () => removeGuides([line.id]) },
                                { type: OPTION_SEPARATOR },
                                { label: 'Clear All Guides', onClick: () => removeGuides(null) },
                              ]);
                            }}
                          />
                          {isGuidedPerspectiveActive && (
                            <>
                              <Circle
                                x={sc1.x}
                                y={sc1.y}
                                radius={6}
                                fill="#ffffff"
                                stroke={color}
                                strokeWidth={2}
                                draggable
                                onMouseDown={(e) => {
                                  e.cancelBubble = true;
                                }}
                                onTouchStart={(e) => {
                                  e.cancelBubble = true;
                                }}
                                onDragMove={(e) => {
                                  const newUv = mapScreenToUv(e.target.x(), e.target.y());
                                  const baseLines = localDragLines || adjustments.guidedPerspective!.lines;
                                  setLocalDragLines(
                                    baseLines.map((l: GuideLine) => (l.id === line.id ? { ...l, p1: newUv } : l)),
                                  );
                                }}
                                onDragEnd={() => {
                                  if (localDragLines) {
                                    setAdjustments((prev) => ({
                                      ...prev,
                                      guidedPerspective: {
                                        ...prev.guidedPerspective,
                                        lines: localDragLines,
                                        enabled: localDragLines.length >= 2,
                                        autoCrop: true,
                                      },
                                    }));
                                    setLocalDragLines(null);
                                  }
                                }}
                              />
                              <Circle
                                x={sc2.x}
                                y={sc2.y}
                                radius={6}
                                fill="#ffffff"
                                stroke={color}
                                strokeWidth={2}
                                draggable
                                onMouseDown={(e) => {
                                  e.cancelBubble = true;
                                }}
                                onTouchStart={(e) => {
                                  e.cancelBubble = true;
                                }}
                                onDragMove={(e) => {
                                  const newUv = mapScreenToUv(e.target.x(), e.target.y());
                                  const baseLines = localDragLines || adjustments.guidedPerspective!.lines;
                                  setLocalDragLines(
                                    baseLines.map((l: GuideLine) => (l.id === line.id ? { ...l, p2: newUv } : l)),
                                  );
                                }}
                                onDragEnd={() => {
                                  if (localDragLines) {
                                    setAdjustments((prev) => ({
                                      ...prev,
                                      guidedPerspective: {
                                        ...prev.guidedPerspective,
                                        lines: localDragLines,
                                        enabled: localDragLines.length >= 2,
                                        autoCrop: true,
                                      },
                                    }));
                                    setLocalDragLines(null);
                                  }
                                }}
                              />
                            </>
                          )}
                        </Group>
                      );
                    })}

                    {draftGuideLine &&
                      (() => {
                        const a = mapUvToScreen(draftGuideLine.p1);
                        const b = mapUvToScreen(draftGuideLine.p2);
                        // Coloured by the axis it will count for while drawing.
                        const orientation = guideOrientation(b.x - a.x, b.y - a.y);
                        const rejected = guideRejection(
                          adjustments.guidedPerspective?.lines || [],
                          orientation,
                          Infinity,
                        );
                        return (
                          <Line
                            points={[a.x, a.y, b.x, b.y]}
                            stroke={!orientation || rejected ? GUIDE_COLORS.invalid : GUIDE_COLORS[orientation]}
                            strokeWidth={2}
                            dash={[4, 4]}
                            listening={false}
                          />
                        );
                      })()}
                  </Layer>
                </Stage>
              )}
            </div>
          )}
        </div>
      </div>
    );
  },
);

export default ImageCanvas;
