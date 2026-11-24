// SPDX-License-Identifier: AGPL-3.0-or-later
/**
 * "Apply Capture One straightening and crop" (C1G1): photos an earlier build
 * imported keep their edit when Capture One now shows them straightened, and
 * the import names them (`geometryOffers` of the import report). This
 * remembers the offers of the last import per library, so the import report
 * and Develop's Crop tool offer the one explicit action: it replaces only
 * the straightening, quarter turns and crop (`library_apply_source_geometry`),
 * as one Undo step, and never runs by itself.
 */
import { invoke } from '@tauri-apps/api/core';
import i18n from 'i18next';
import { create } from 'zustand';
import { notify } from '../activity/notify';
import { Invokes } from '../components/ui/AppProperties';
import { closeOpenPhotoForLibraryWrite, recipesChangedOutside, recipeWrite } from '../hooks/useEditorActions';
import { useLibraryStore } from '../store/useLibraryStore';
import { importedRecipesAction, type RecipeSnapshot } from './appUndo';

export interface GeometryOffers {
  /** The library the offers are in. */
  directory: string;
  /** The Capture One session or catalog they were read from, as imported. */
  source: string;
  relinks: { from: string; to: string }[];
  /** The photographs' and virtual copies' paths, as the import report names them. */
  paths: string[];
}

export interface GeometryApplied {
  recipeChanges: RecipeSnapshot[];
  unchanged: string[];
  errors: string[];
}

const KEY = 'rawtonic.sourceGeometryOffers';

function load(): GeometryOffers[] {
  try {
    const value = JSON.parse(localStorage.getItem(KEY) ?? '[]');
    return Array.isArray(value) ? value : [];
  } catch {
    return [];
  }
}

function save(offers: GeometryOffers[]) {
  try {
    localStorage.setItem(KEY, JSON.stringify(offers));
  } catch {
    // Private window or blocked storage: the offers last for this run.
  }
}

export const useGeometryOffers = create<{ offers: GeometryOffers[] }>(() => ({ offers: load() }));

/** The offers of the import just made into `directory` (its report is
 * authoritative: an import that names none clears the earlier ones). */
export function rememberGeometryOffers(offers: GeometryOffers) {
  const others = useGeometryOffers.getState().offers.filter((o) => o.directory !== offers.directory);
  const next = offers.paths.length ? [...others, offers] : others;
  save(next);
  useGeometryOffers.setState({ offers: next });
}

function settle(directory: string, paths: string[]) {
  const next = useGeometryOffers
    .getState()
    .offers.map((o) => (o.directory === directory ? { ...o, paths: o.paths.filter((p) => !paths.includes(p)) } : o))
    .filter((o) => o.paths.length);
  save(next);
  useGeometryOffers.setState({ offers: next });
}

/** Whether `path` has an offer in the open library `directory`. */
export function useGeometryOffer(directory: string | null | undefined, path: string | null | undefined): boolean {
  return useGeometryOffers(
    (s) => Boolean(path) && s.offers.some((o) => o.directory === directory && o.paths.includes(path as string))
  );
}

/** Applies the offered geometry to `paths` of the library `directory`
 * (the edits made in Develop are written first, as an import does). One
 * Undo step for all of them. Resolves false when nothing could be written. */
export async function applySourceGeometry(directory: string, paths: string[]): Promise<boolean> {
  // The Undo step belongs to the library that is open: the offers of another
  // one wait until it is.
  if (useLibraryStore.getState().sessionDirectory !== directory) {
    notify.warning(i18n.t('session.toasts.sourceGeometryOtherLibrary'));
    return false;
  }
  const offers = useGeometryOffers.getState().offers.find((o) => o.directory === directory);
  const wanted = paths.filter((p) => offers?.paths.includes(p));
  if (!offers || !wanted.length) return false;
  const unsaved = await closeOpenPhotoForLibraryWrite();
  if (unsaved) {
    notify.error(i18n.t('session.toasts.importUnsavedEdits', { n: unsaved }));
    return false;
  }
  let applied: GeometryApplied;
  try {
    // Through the photos' write queue: a photo opened meanwhile loads (and
    // saves) after the geometry is written.
    applied = await recipeWrite(
      wanted,
      () =>
        invoke<GeometryApplied>(Invokes.LibraryApplySourceGeometry, {
          source: offers.source,
          directory,
          relinks: offers.relinks,
          paths: wanted
        }),
      {
        kind: 'sourceGeometry',
        failedOf: (result) =>
          wanted.filter(
            (path) => !result.recipeChanges.some((c) => c.path === path) && !result.unchanged.includes(path)
          )
      }
    );
  } catch (e) {
    notify.error(i18n.t('session.toasts.sourceGeometryFailed'), { detail: String(e) });
    return false;
  }
  // What was written, and what already follows Capture One, is settled.
  settle(directory, [...applied.recipeChanges.map((c) => c.path), ...applied.unchanged]);
  importedRecipesAction(applied.recipeChanges, 'sourceGeometry');
  await recipesChangedOutside(applied.recipeChanges.map((change) => change.path));
  if (applied.recipeChanges.length)
    notify.success(i18n.t('session.toasts.sourceGeometryApplied', { n: applied.recipeChanges.length }));
  if (applied.errors.length)
    notify.warning(i18n.t('session.toasts.sourceGeometryErrors', { n: applied.errors.length }), {
      detail: applied.errors.join('\n'),
      autoClose: false
    });
  return applied.recipeChanges.length > 0;
}
