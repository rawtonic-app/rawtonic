import type { SelectedImage } from '../components/ui/AppProperties';
import { SOURCE_FRAME_VERSION, type Adjustments } from './adjustments';

/** The frame version a recipe asks for (crates/engine/src/source_frame.rs
 * `requested`): its own, else 0 (an edit from before the key; the editor's
 * new edits carry `SOURCE_FRAME_VERSION`). Versions this build does not
 * know read as its newest. */
export const requestedSourceFrame = (adjustments: Pick<Partial<Adjustments>, 'sourceFrameVersion'>): number =>
  Math.min(adjustments.sourceFrameVersion ?? 0, SOURCE_FRAME_VERSION);

/** The frame `version` develops of a photograph whose versions frame it as
 * `frames` says (`source_frame::effective`; the index is the version):
 * `version` itself when unknown. */
export const frameOf = (frames: readonly number[] | undefined, version: number): number => {
  if (version === 0) return 0;
  if (!frames || frames.length === 0) return version;
  return frames[Math.min(version, frames.length - 1)] ?? version;
};

/**
 * The `sourceFrameVersion` the open photograph must be opened again with,
 * because its recipe develops another frame of the RAW than its source
 * shows (Update frame, a Reset, undo or redo across them), or null when it
 * does not: only decoded RAWs on their file (an offline photograph cannot
 * be developed in another frame), never during a slider drag or while it is
 * being opened again.
 */
export function sourceFrameToOpen(
  image: Pick<SelectedImage, 'isDecoded' | 'isRaw' | 'offline' | 'sourceFrame' | 'sourceFrames' | 'reopenFrame'> | null,
  adjustments: Pick<Partial<Adjustments>, 'sourceFrameVersion'>,
  dragging: boolean,
): number | null {
  if (!image?.isDecoded || !image.isRaw || image.offline || dragging) return null;
  if (image.sourceFrame === undefined || image.reopenFrame !== undefined) return null;
  const wanted = requestedSourceFrame(adjustments);
  return frameOf(image.sourceFrames, wanted) === image.sourceFrame ? null : wanted;
}

/** Whether "Update frame" would change the open photograph: the newest
 * frame is another than its recipe's (Develop's lens tool shows a note). */
export function frameUpdateAvailable(
  image: Pick<SelectedImage, 'isRaw' | 'offline' | 'sourceFrames'> | null,
  adjustments: Pick<Partial<Adjustments>, 'sourceFrameVersion'>,
): boolean {
  if (!image?.isRaw || image.offline || !image.sourceFrames) return false;
  return (
    frameOf(image.sourceFrames, requestedSourceFrame(adjustments)) !==
    frameOf(image.sourceFrames, SOURCE_FRAME_VERSION)
  );
}

/** A new edit's recipe for a source that shows frame `sourceFrame` of a
 * file framed as `sourceFrames` says: its frame version where the newest
 * develops another frame than the source shows (an offline photograph on
 * previews of an earlier frame: its new edit is made on what is shown). */
export function withSourceFrameOf<T extends Partial<Adjustments>>(
  recipe: T,
  sourceFrame: number | null | undefined,
  sourceFrames: readonly number[] | null | undefined,
): T {
  if (sourceFrame === null || sourceFrame === undefined) return recipe;
  const frames = sourceFrames ?? undefined;
  return frameOf(frames, requestedSourceFrame(recipe)) === sourceFrame
    ? recipe
    : { ...recipe, sourceFrameVersion: sourceFrame };
}
