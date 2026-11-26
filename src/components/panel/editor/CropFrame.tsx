import { memo, type CSSProperties, type ReactNode } from 'react';
import ReactCrop, { type Crop, type PercentCrop } from 'react-image-crop';
import 'react-image-crop/dist/ReactCrop.css';
import { useStore } from 'zustand';
import CompositionOverlays, { type OverlayMode } from './overlays/CompositionOverlays';
import type { CropSelection } from './cropSelection';

interface CropFrameProps {
  cropSelection: CropSelection;
  aspect: number | undefined;
  onChange(crop: Crop, percentCrop: PercentCrop): void;
  onComplete(crop: Crop, percentCrop: PercentCrop): void;
  onDragStart(): void;
  onDragEnd(): void;
  shade: number;
  /** The crop box in layout pixels (the uncropped frame at this zoom). */
  boxWidth: number;
  boxHeight: number;
  /** The oriented photograph in pixels, for the size label. */
  photoWidth: number;
  photoHeight: number;
  overlayMode: OverlayMode;
  overlayRotation: number;
  denseGrid: boolean;
  dragging: boolean;
  children: ReactNode;
}

/** The crop tool's frame: the only part of the viewer that renders on each
 * drag step (it subscribes to the crop selection itself; CRP1). */
const CropFrame = memo(function CropFrame({
  cropSelection,
  aspect,
  onChange,
  onComplete,
  onDragStart,
  onDragEnd,
  shade,
  boxWidth,
  boxHeight,
  photoWidth,
  photoHeight,
  overlayMode,
  overlayRotation,
  denseGrid,
  dragging,
  children
}: CropFrameProps) {
  const crop = useStore(cropSelection, (state) => state.crop);
  return (
    <ReactCrop
      aspect={aspect}
      crop={crop ?? undefined}
      onChange={onChange}
      onComplete={onComplete}
      onDragStart={onDragStart}
      onDragEnd={onDragEnd}
      style={{ '--crop-shade': shade } as CSSProperties}
      ruleOfThirds={false}
      renderSelectionAddon={() => {
        if (!crop || !boxWidth || !boxHeight) return null;
        const percent = crop.unit === '%';
        const width = percent ? boxWidth * (crop.width / 100) : crop.width;
        const height = percent ? boxHeight * (crop.height / 100) : crop.height;
        if (width <= 0 || height <= 0) return null;
        const sizeLabel = percent
          ? `${Math.round((crop.width / 100) * photoWidth)} × ${Math.round((crop.height / 100) * photoHeight)}`
          : `${Math.round(crop.width)} × ${Math.round(crop.height)}`;
        return (
          <>
            <CompositionOverlays
              width={width}
              height={height}
              mode={overlayMode}
              rotation={overlayRotation}
              denseVisible={denseGrid}
            />
            {dragging && <div className="crop-size-label">{sizeLabel} px</div>}
          </>
        );
      }}
    >
      {children}
    </ReactCrop>
  );
});

export default CropFrame;
