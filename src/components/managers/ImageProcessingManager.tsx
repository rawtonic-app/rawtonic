import { useImageProcessing, type PreviousAdjustments } from '../../hooks/useImageProcessing';
import type { TransformWrapperHandle } from '../panel/Editor';

interface Props {
  transformWrapperRef: React.RefObject<TransformWrapperHandle | null>;
  prevAdjustmentsRef: React.RefObject<PreviousAdjustments | null>;
  previewJobIdRef: React.RefObject<number>;
  latestRenderedJobIdRef: React.RefObject<number>;
  currentResRef: React.RefObject<number>;
}

export default function ImageProcessingManager(props: Props) {
  useImageProcessing(props.transformWrapperRef, props.prevAdjustmentsRef, {
    previewJobIdRef: props.previewJobIdRef,
    latestRenderedJobIdRef: props.latestRenderedJobIdRef,
    currentResRef: props.currentResRef,
  });

  return null;
}
