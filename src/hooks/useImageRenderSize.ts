import { useState, useLayoutEffect } from 'react';
import { fitInViewer, proofMarginPx } from '../utils/viewerDisplay';

export interface ImageDimensions {
  height: number;
  width: number;
}

export interface RenderSize {
  containerHeight: number;
  containerWidth: number;
  height: number;
  offsetX: number;
  offsetY: number;
  scale: number;
  width: number;
  /** Proof margin around the fitted photograph, CSS pixels (0 when off). */
  margin?: number;
}

const DEFAULT_SIZE: RenderSize = {
  width: 0,
  height: 0,
  scale: 1,
  offsetX: 0,
  offsetY: 0,
  containerWidth: 0,
  containerHeight: 0,
  margin: 0,
};

/** Proof margin of the viewer: on/off and its size in % of the shorter side. */
export interface ProofMargin {
  enabled: boolean;
  percent?: number;
}

export const useImageRenderSize = (
  containerRef: React.RefObject<HTMLElement | null>,
  imageDimensions: ImageDimensions | null,
  proofMargin?: ProofMargin,
) => {
  const [renderSize, setRenderSize] = useState<RenderSize>(DEFAULT_SIZE);
  const imgWidth = imageDimensions?.width;
  const imgHeight = imageDimensions?.height;
  const marginEnabled = proofMargin?.enabled ?? false;
  const marginPercent = proofMargin?.percent;

  useLayoutEffect(() => {
    const container = containerRef.current;

    if (!container || !imgWidth || !imgHeight) {
      setRenderSize(DEFAULT_SIZE);
      return;
    }

    const updateSize = () => {
      const { clientWidth: containerWidth, clientHeight: containerHeight } = container;
      const margin = marginEnabled ? proofMarginPx(containerWidth, containerHeight, marginPercent) : 0;
      const fitted = fitInViewer(containerWidth, containerHeight, imgWidth, imgHeight, margin);
      setRenderSize({ ...fitted, containerWidth, containerHeight, margin });
    };

    updateSize();

    const resizeObserver = new ResizeObserver(() => {
      updateSize();
    });

    resizeObserver.observe(container);

    return () => resizeObserver.disconnect();
  }, [containerRef, imgWidth, imgHeight, marginEnabled, marginPercent]);

  return renderSize;
};
