import { useEditorStore } from '../store/useEditorStore';
import { useSnapshotStore, type Snapshot } from '../store/useSnapshotStore';
import { normalizeLoadedAdjustments, type Adjustments } from './adjustments';

/**
 * The snapshot the open photograph is compared with: Before/After's before
 * (the split, the held \ peek) shows it instead of the unedited photograph,
 * as Lightroom's snapshot settings copied to Before. Null when none is
 * chosen, or the chosen one belongs to another photograph or is gone.
 */
export function beforeSnapshot(): Snapshot | null {
  const { beforeSnapshot: chosen, selectedImage } = useEditorStore.getState();
  const snapshots = useSnapshotStore.getState();
  if (!chosen || !selectedImage || chosen.path !== selectedImage.path || snapshots.path !== chosen.path) return null;
  return snapshots.snapshots.find((snapshot) => snapshot.id === chosen.id) ?? null;
}

/** The chosen snapshot's adjustments as a restore of it would load them. */
export function beforeSnapshotAdjustments(): Adjustments | null {
  const snapshot = beforeSnapshot();
  const image = useEditorStore.getState().selectedImage;
  return snapshot && image ? normalizeLoadedAdjustments(snapshot.adjustments, image.isRaw) : null;
}

/** Compares the open photograph with `snapshot` (null: with the unedited
 * photograph again). */
export function compareWithSnapshot(snapshot: Snapshot | null) {
  const { selectedImage, setEditor } = useEditorStore.getState();
  setEditor({
    beforeSnapshot: snapshot && selectedImage ? { path: selectedImage.path, id: snapshot.id } : null,
  });
}
