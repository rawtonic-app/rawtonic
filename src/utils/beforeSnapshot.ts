import { useEditorStore } from '../store/useEditorStore';
import { useSnapshotStore, type Snapshot } from '../store/useSnapshotStore';
import { normalizeLoadedAdjustments, type Adjustments } from './adjustments';
import { withOwnBitmapIds } from './bitmapIds';
import { framingSignature, originalPreviewAdjustments } from './beforeAfter';
import { aiDenoiseAmount, developmentSignature } from './aiDenoise';
import { useSettingsStore } from '../store/useSettingsStore';

const loaded = new WeakMap<Snapshot, { isRaw: boolean; adjustments: Adjustments; comparable: string }>();

/** How a recipe frames the photograph and develops its RAW
 * (`framingSignature`, `developmentSignature`), as a string. */
export function comparisonSignature(adjustments: Adjustments, isRaw: boolean): string {
  return `${framingSignature(adjustments)}|${developmentSignature(adjustments, isRaw)}`;
}

/** `snapshot` as a restore of it would load it, its AI bitmaps under ids of
 * their own (`withOwnBitmapIds`: the native side keeps bitmaps by id and
 * must not take them for the edit's newer ones), and its
 * `comparisonSignature`. The same for each call while the snapshot stays as
 * it is, so its bitmaps are sent once. */
function loadedSnapshot(snapshot: Snapshot, isRaw: boolean) {
  const kept = loaded.get(snapshot);
  if (kept?.isRaw === isRaw) return kept;
  const adjustments = withOwnBitmapIds(normalizeLoadedAdjustments(snapshot.adjustments, isRaw));
  const entry = { isRaw, adjustments, comparable: comparisonSignature(adjustments, isRaw) };
  loaded.set(snapshot, entry);
  return entry;
}

/**
 * Whether `snapshot` can be Before beside the edit (`signature`: the edit's
 * `comparisonSignature`): it frames the photograph as the edit does (crop,
 * orientation, transform and lens geometry: `BEFORE_FRAMING_KEYS`; its
 * spots, drawn masks, relight depth and face retouch belong to its own
 * framing) and develops the RAW as the edit does (AI Denoise, colour noise
 * filter, dead-photosite test), so both sides render from the edit's source
 * and Before is exactly the snapshot's export.
 */
export function comparableWithEdit(snapshot: Snapshot, signature: string, isRaw: boolean): boolean {
  return loadedSnapshot(snapshot, isRaw).comparable === signature;
}

/**
 * Why `snapshot` cannot be Before beside the edit now, or null when it can:
 * `differs` (`comparableWithEdit`), or `aiDenoise` for a RAW snapshot with AI
 * Denoise on Linux, whose browser split decodes without it
 * (`multi_view.rs`). `signature`: the edit's `comparisonSignature`.
 */
export function compareBlocker(
  snapshot: Snapshot,
  signature: string,
  isRaw: boolean,
  osPlatform: string | null | undefined,
): 'differs' | 'aiDenoise' | null {
  if (!comparableWithEdit(snapshot, signature, isRaw)) return 'differs';
  if (osPlatform === 'linux' && isRaw && aiDenoiseAmount(loadedSnapshot(snapshot, isRaw).adjustments) > 0) {
    return 'aiDenoise';
  }
  return null;
}

/**
 * The snapshot the open photograph is compared with: Before/After's before
 * (the split, the held \ peek) shows it instead of the unedited photograph,
 * as Lightroom's snapshot settings copied to Before. Null when none is
 * chosen, the chosen one belongs to another photograph or is gone, or it
 * cannot be compared with the edit as it is now (`compareBlocker`; it comes
 * back when it can).
 */
export function beforeSnapshot(): Snapshot | null {
  const { beforeSnapshot: chosen, selectedImage, adjustments } = useEditorStore.getState();
  const snapshots = useSnapshotStore.getState();
  if (!chosen || !selectedImage || chosen.path !== selectedImage.path || snapshots.path !== chosen.path) return null;
  const snapshot = snapshots.snapshots.find((candidate) => candidate.id === chosen.id);
  const signature = comparisonSignature(adjustments, selectedImage.isRaw);
  const { osPlatform } = useSettingsStore.getState();
  if (!snapshot || compareBlocker(snapshot, signature, selectedImage.isRaw, osPlatform)) return null;
  return snapshot;
}

/** The compared snapshot's adjustments (`beforeSnapshot`) as `loadedSnapshot`
 * prepares them, or null. */
export function beforeSnapshotAdjustments(): Adjustments | null {
  const snapshot = beforeSnapshot();
  const image = useEditorStore.getState().selectedImage;
  if (!snapshot || !image) return null;
  return loadedSnapshot(snapshot, image.isRaw).adjustments;
}

/** The recipe Before/After shows as the before of the edit `adjustments`:
 * the snapshot compared with, or the unedited photograph with the edit's
 * framing (`originalPreviewAdjustments`). */
export function beforeAdjustments(adjustments: Adjustments): Adjustments {
  return originalPreviewAdjustments(adjustments, beforeSnapshotAdjustments());
}

/** Compares the open photograph with `snapshot` (null: with the unedited
 * photograph again). */
export function compareWithSnapshot(snapshot: Snapshot | null) {
  const { selectedImage, setEditor } = useEditorStore.getState();
  setEditor({
    beforeSnapshot: snapshot && selectedImage ? { path: selectedImage.path, id: snapshot.id } : null,
  });
}
