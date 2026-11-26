import { createStore, type StoreApi } from 'zustand/vanilla';
import type { Crop } from 'react-image-crop';

/** The crop frame the crop tool shows (%, live during a drag; null outside
 * the tool). It changes with every drag step, so it lives in a store of its
 * own: only the frame (`CropFrame`) and the native display's placement
 * subscribe to it, and a drag step re-renders neither the editor nor the
 * canvas (CRP1). The recipe's crop is written on release, as before. */
export interface CropSelectionState {
  crop: Crop | null;
}

export type CropSelection = StoreApi<CropSelectionState>;

export function createCropSelection(): CropSelection {
  return createStore<CropSelectionState>(() => ({ crop: null }));
}
