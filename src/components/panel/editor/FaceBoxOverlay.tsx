// SPDX-License-Identifier: AGPL-3.0-or-later
import { invoke } from '@tauri-apps/api/core';
import { useEffect, useState } from 'react';
import { useTranslation } from 'react-i18next';
import { useEditorStore } from '../../../store/useEditorStore';
import { effectiveEyes, markedFace, useEyesStore } from '../../../store/useEyesStore';
import { GEOMETRY_KEYS, ORIENTATION_KEYS } from '../../../utils/geometryKeys';
import { labelPlace, outlineOnPhoto, svgPoints } from '../../../utils/faceBox';
import { EYE_ICONS, stateLabel } from '../../studio/StudioEyes';
import { Invokes } from '../../ui/AppProperties';

interface Props {
  /** The displayed photograph's box in the viewer's (zoomed) content. */
  rect: { left: number; top: number; width: number; height: number };
  /** Mask-frame pixels at the box's top left, and box pixels per mask pixel. */
  cropX: number;
  cropY: number;
  scale: number;
  /** The viewer's zoom, so the line and label keep their size on screen. */
  zoom: number;
}

/** The label's height on screen (CSS pixels). */
const LABEL_HEIGHT = 20;

/** The adjustments that move where the photograph shows a face. */
const SHOWN_KEYS = [...GEOMETRY_KEYS, ...ORIENTATION_KEYS] as const;

/**
 * The box of the face the Loupe's face strip points at (hovered, or pinned
 * by a click), as Lightroom's loupe marks the face chosen in its culling
 * panel: a webview outline over the native photograph, where the recipe's
 * geometry shows that face (`face_boxes_shown`). It carries the face's eye
 * call as the strip's badge does: the same colour, and its icon and words,
 * never colour alone. Shows nothing, takes no pointer.
 */
export default function FaceBoxOverlay({ rect, cropX, cropY, scale, zoom }: Props) {
  const { t } = useTranslation();
  const path = useEditorStore((s) => s.selectedImage?.path);
  const width = useEditorStore((s) => s.selectedImage?.width ?? 0);
  const height = useEditorStore((s) => s.selectedImage?.height ?? 0);
  const geometryKey = useEditorStore((s) => JSON.stringify(SHOWN_KEYS.map((key) => s.adjustments[key])));
  const mark = useEyesStore((s) => (s.faceMark?.path === path ? s.faceMark : null));
  const index = markedFace(mark);
  const faces = mark?.faces;
  const boxesKey = faces ? JSON.stringify(faces.map((f) => f.bbox)) : '';
  // Every face's outline, asked when the faces or the geometry change, so
  // moving from face to face shows its box at once.
  const [shown, setShown] = useState<{ key: string; outlines: ([number, number][] | null)[] } | null>(null);
  const key = `${path}\n${width}x${height}\n${geometryKey}\n${boxesKey}`;
  useEffect(() => {
    if (!path || !boxesKey || !width || !height) return;
    let current = true;
    invoke<([number, number][] | null)[]>(Invokes.FaceBoxesShown, {
      path,
      jsAdjustments: useEditorStore.getState().adjustments,
      width,
      height,
      boxes: JSON.parse(boxesKey),
    })
      .then((outlines) => current && setShown({ key, outlines }))
      .catch((error) => console.error('Showing the face box failed:', error));
    return () => {
      current = false;
    };
    // `key` holds everything the answer depends on.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [key]);

  if (index === null || !faces?.[index] || shown?.key !== key) return null;
  const outline = shown.outlines[index];
  if (!outline) return null;
  const face = faces[index];
  const state = effectiveEyes(face);
  const Icon = EYE_ICONS[state];
  const points = outlineOnPhoto(outline, cropX, cropY, scale);
  const label = labelPlace(points, LABEL_HEIGHT / zoom);
  const text = t('studio.eyes.faceLabel', { n: index + 1, state: stateLabel(t, state) });
  return (
    <div
      className={`face-box-overlay is-${state}`}
      data-testid="face-box-overlay"
      style={{
        position: 'absolute',
        left: rect.left,
        top: rect.top,
        width: rect.width,
        height: rect.height,
        overflow: 'hidden',
        pointerEvents: 'none',
        zIndex: 4,
      }}
    >
      <svg width={rect.width} height={rect.height} style={{ position: 'absolute', inset: 0, overflow: 'visible' }}>
        {/* A dark rim under the coloured line: legible on light and dark skin and backgrounds. */}
        <polygon className="face-box-rim" points={svgPoints(points)} strokeWidth={3.5 / zoom} />
        <polygon
          className="face-box-line"
          points={svgPoints(points)}
          strokeWidth={1.75 / zoom}
          strokeDasharray={state === 'uncertain' ? `${6 / zoom} ${4 / zoom}` : undefined}
        />
      </svg>
      <span
        className="face-box-label"
        role="img"
        aria-label={text}
        style={{
          left: label.x,
          top: label.y,
          transform: `scale(${1 / zoom}) translateY(${label.below ? '2px' : 'calc(-100% - 2px)'})`,
        }}
      >
        <Icon size={12} />
        <span>{text}</span>
      </span>
    </div>
  );
}
