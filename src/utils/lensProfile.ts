import { invoke } from '@tauri-apps/api/core';
import { Invokes } from '../components/ui/AppProperties';
import type { Adjustments } from './adjustments';

type LensFields = Pick<Adjustments, 'lensCorrectionMode' | 'lensMaker' | 'lensModel' | 'cameraCaVersion'>;

/** A recipe's lens choice: its mode and, chosen by hand, the lens (Auto's
 * lens is found); for the camera's own data, the version of its CA
 * correction, which its parameters depend on. */
export function lensChoice(adjustments: Partial<LensFields>): string {
  const mode = adjustments.lensCorrectionMode ?? null;
  if (mode === 'camera') return JSON.stringify([mode, adjustments.cameraCaVersion ?? 0]);
  const byHand = mode !== 'auto';
  return JSON.stringify([mode, byHand ? adjustments.lensMaker ?? null : null, byHand ? adjustments.lensModel ?? null : null]);
}

/** Lens parameters still to be looked up: the editor commits a new lens
 * choice (paste, preset, preset Amount) at once with these, its save looks
 * them up (edits.rs, LensLookup), and the editor fills them in for the
 * preview meanwhile (useEditorActions). */
export const PENDING_LENS = { pending: true } as unknown as Adjustments['lensDistortionParams'];

export function isPendingLens(params: unknown): boolean {
  return Boolean(params && typeof params === 'object' && (params as { pending?: unknown }).pending === true);
}

/** The lens fields to commit with a new lens choice: parameters pending,
 * and no lens yet where it is found rather than chosen. */
export function pendingLens(adjustments: Partial<LensFields>): Partial<Adjustments> {
  const mode = adjustments.lensCorrectionMode;
  return mode === 'auto' || mode === 'camera'
    ? { lensMaker: null, lensModel: null, lensDistortionParams: PENDING_LENS }
    : { lensDistortionParams: PENDING_LENS };
}

/** The lens fields `adjustments` gets on the photograph at `path` when it is
 * saved (`resolve_lens_profile`: the save's own lookup), so the preview and
 * the saved recipe agree. Rejects when the lookup fails. */
export function lookUpLensProfile(path: string, adjustments: Partial<Adjustments>): Promise<Partial<Adjustments>> {
  return invoke<Partial<Adjustments>>(Invokes.ResolveLensProfile, { path, adjustments });
}
