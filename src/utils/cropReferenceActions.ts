import type { Crop } from 'react-image-crop';
import { useEditorStore } from '../store/useEditorStore';
import { getOrientedDimensions } from './cropUtils';
import {
  cropReferenceKey,
  fitRatioCrop,
  recordedCropReference,
  referenceAfterHandEdit,
  referenceForRatioChange
} from './cropReference';

/* The editor's crop reference (utils/cropReference) for the photo it shows. */

/** The photo, oriented frame and rotation the editor shows now. */
function current() {
  const { selectedImage, adjustments } = useEditorStore.getState();
  if (!selectedImage?.width || !selectedImage?.height) return null;
  const { width, height } = getOrientedDimensions(
    selectedImage.width,
    selectedImage.height,
    adjustments.orientationSteps || 0
  );
  return {
    key: cropReferenceKey(selectedImage.path, adjustments as unknown as Record<string, unknown>),
    width,
    height,
    rotation: adjustments.rotation || 0
  };
}

/** Records a crop the user drew, resized or moved by hand (not one a ratio
 * change, a rotation or a reset fitted). */
export function noteHandDrawnCrop(previous: Crop | null | undefined, next: Crop) {
  const now = current();
  if (!now || !(next.width > 0 && next.height > 0)) return;
  const reference = useEditorStore.getState().cropReference;
  useEditorStore.setState({
    cropReference: referenceAfterHandEdit(reference, now.key, previous, next, now.width, now.height, now.rotation)
  });
}

/** The crop for a new ratio on the photo the editor shows (null: none fits). */
export function cropForRatio(aspectRatio: number): Crop | null {
  const now = current();
  const { selectedImage, adjustments, cropReference } = useEditorStore.getState();
  if (!now || !selectedImage) return null;
  // Back on a photo after another one: the reference recorded with its edit.
  const kept = cropReference?.key === now.key ? cropReference : recordedCropReference(selectedImage.path, adjustments);
  const reference = referenceForRatioChange(
    kept ?? null,
    now.key,
    adjustments.crop,
    now.width,
    now.height,
    adjustments.aspectRatio
  );
  if (reference !== cropReference) useEditorStore.setState({ cropReference: reference });
  return fitRatioCrop(
    selectedImage.width,
    selectedImage.height,
    adjustments.orientationSteps || 0,
    aspectRatio,
    now.rotation,
    reference
  );
}
