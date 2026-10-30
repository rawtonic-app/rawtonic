import { invoke } from '@tauri-apps/api/core';
import { Invokes } from '../components/ui/AppProperties';
import { useEditorStore } from '../store/useEditorStore';
import { useLibraryStore } from '../store/useLibraryStore';
import { globalImageCache } from './ImageLRUCache';
import { invokeInOrder } from './catalogWrites';

/** A capture time as a camera writes it (`rawtonic_catalog::capture_time`). */
export interface CaptureTime {
  /** Local wall time, `YYYY-MM-DD HH:MM:SS`. */
  local: string;
  /** Fraction digits as written (`"05"` is 50 ms). */
  subsec?: string | null;
  /** UTC offset as written (`+02:00`). */
  offset?: string | null;
}

export interface ShiftParts {
  negative: boolean;
  days: string;
  hours: string;
  minutes: string;
  seconds: string;
}

export type Excluded = 'notInLibrary' | 'otherLibrary' | 'offline' | 'noCaptureTime';

export interface PreviewItem {
  assetId: string;
  path: string;
  camera: string;
  source: CaptureTime | null;
  corrected: CaptureTime | null;
}

export interface CapturePreview {
  library: string | null;
  items: PreviewItem[];
  excluded: { path: string; reason: Excluded }[];
}

export type PlanRequest =
  | { kind: 'shift'; parts: ShiftParts }
  | { kind: 'reference'; assetId: string; corrected: string }
  | { kind: 'reset' };

export interface PlannedChange {
  assetId: string;
  path: string;
  expected: CaptureTime | null;
  new: CaptureTime | null;
  before: CaptureTime;
  after: CaptureTime;
}

export type PlanError =
  | { kind: 'field'; field: 'days' | 'hours' | 'minutes' | 'seconds' }
  | { kind: 'reference'; problem: 'invalid' | 'fraction' | 'outOfRange' }
  | { kind: 'noReference' };

export interface Plan {
  shift: ShiftParts | null;
  error: PlanError | null;
  changes: PlannedChange[];
  outOfRange: string[];
  beforeRange: [CaptureTime, CaptureTime] | null;
  afterRange: [CaptureTime, CaptureTime] | null;
}

export interface Applied {
  conflicts: string[];
  failed: string[];
  undo: PlannedChange[];
}

/** A time as the Metadata tool shows it: `2026-10-30 14:05:09.05 +02:00`. */
export function captureText(time: CaptureTime | null | undefined): string {
  if (!time) return '';
  return `${time.local}${time.subsec ? `.${time.subsec}` : ''}${time.offset ? ` ${time.offset}` : ''}`;
}

/** The EXIF fields of a photo's map a time sets (`null`: removed). */
function exifFields(time: CaptureTime): Record<string, string | null> {
  return {
    DateTimeOriginal: time.local,
    SubSecTimeOriginal: time.subsec ?? null,
    OffsetTimeOriginal: time.offset ?? null,
  };
}

function withTime(exif: Record<string, string> | null | undefined, time: CaptureTime): Record<string, string> {
  const next = { ...(exif ?? {}) };
  for (const [key, value] of Object.entries(exifFields(time))) {
    if (value === null) delete next[key];
    else next[key] = value;
  }
  return next;
}

/** Shows the written times in the grid, Metadata tool and editor: every
 * virtual copy of a photograph shares its file's EXIF. */
export function showCaptureTimes(changes: PlannedChange[]) {
  const byFile = new Map(changes.map((change) => [change.path.split('?vc=')[0], change.after]));
  if (!byFile.size) return;
  const timeOf = (path: string) => byFile.get(path.split('?vc=')[0]);
  useLibraryStore.getState().setLibrary((state) => ({
    imageList: state.imageList.map((image) => {
      const time = timeOf(image.path);
      return time && image.exif ? { ...image, exif: withTime(image.exif, time) } : image;
    }),
  }));
  useEditorStore.getState().setEditor((state) => {
    const time = state.selectedImage && timeOf(state.selectedImage.path);
    if (!state.selectedImage || !time || !state.selectedImage.exif) return state;
    return { selectedImage: { ...state.selectedImage, exif: withTime(state.selectedImage.exif, time) } };
  });
  for (const change of changes) {
    const cached = globalImageCache.get(change.path);
    if (cached?.selectedImage?.exif)
      globalImageCache.set(change.path, {
        ...cached,
        selectedImage: { ...cached.selectedImage, exif: withTime(cached.selectedImage.exif, change.after) },
      });
  }
}

/** Writes `changes` to `library` after the photographs' earlier writes
 * (`invokeInOrder`: their write queue, then the catalog changed), and shows
 * the times written. A conflict writes nothing. */
export async function writeCaptureTimes(library: string, changes: PlannedChange[]): Promise<Applied> {
  const applied = await invokeInOrder<Applied>(Invokes.CaptureTimeApply, {
    library,
    changes,
    paths: changes.map((change) => change.path),
  });
  // Photographs of another library are not listed: nothing to show there.
  if (!applied.conflicts.length) showCaptureTimes(changes);
  return applied;
}

export const previewCaptureTimes = (paths: string[]) =>
  invoke<CapturePreview>(Invokes.CaptureTimePreview, { paths });

export const planCaptureTimes = (items: PreviewItem[], request: PlanRequest) =>
  invoke<Plan>(Invokes.CaptureTimePlan, { items, request });
