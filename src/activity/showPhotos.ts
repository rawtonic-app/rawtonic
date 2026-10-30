// SPDX-License-Identifier: AGPL-3.0-or-later
/** Asks the app to show these photographs in the grid (App.tsx), as a view
 * like Missing Photographs: an activity entry's Show photos. */
export const SHOW_ACTIVITY_PHOTOS_EVENT = 'rawtonic:show-activity-photos';
export const ACTIVITY_PHOTOS_VIEW_ID = 'rawtonic:activity-photos';

export type ShowActivityPhotos = { name: string; paths: string[] };

/** The view last asked for: listed again when the library refreshes, its
 * paths following renames and moves (`followActivityPhotoMoves`). */
let shown: ShowActivityPhotos | null = null;

export function showPhotos(name: string, paths: string[]): void {
  shown = { name, paths: [...paths] };
  window.dispatchEvent(new CustomEvent<ShowActivityPhotos>(SHOW_ACTIVITY_PHOTOS_EVENT, { detail: shown }));
}

/** The photographs of the activity view, at their current places. */
export function shownActivityPhotos(): ShowActivityPhotos | null {
  return shown;
}

/** After a rename or move: the view's photographs at their new places. */
export function followActivityPhotoMoves(move: (paths: string[]) => string[]): void {
  if (shown) shown = { ...shown, paths: move(shown.paths) };
}
