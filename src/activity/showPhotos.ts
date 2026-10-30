// SPDX-License-Identifier: AGPL-3.0-or-later
/** Asks the app to show these photographs in the grid (App.tsx), as a view
 * like Missing Photographs: an activity entry's Show photos. */
export const SHOW_ACTIVITY_PHOTOS_EVENT = 'rawtonic:show-activity-photos';
export const ACTIVITY_PHOTOS_VIEW_ID = 'rawtonic:activity-photos';

export type ShowActivityPhotos = { name: string; paths: string[] };

export function showPhotos(name: string, paths: string[]): void {
  window.dispatchEvent(new CustomEvent<ShowActivityPhotos>(SHOW_ACTIVITY_PHOTOS_EVENT, { detail: { name, paths } }));
}
