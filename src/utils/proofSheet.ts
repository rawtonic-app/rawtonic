// SPDX-License-Identifier: AGPL-3.0-or-later
import { create } from 'zustand';
import { printRadius } from './exportRecipes';

/** Paper sizes in millimetres, portrait. */
export const PAPERS = {
  a4: [210, 297],
  a3: [297, 420],
  letter: [215.9, 279.4],
  legal: [215.9, 355.6],
  tabloid: [279.4, 431.8]
} as const;
export type Paper = keyof typeof PAPERS | 'custom';

/** The Proof Sheet dialog's form, remembered per library. */
export interface ProofDraft {
  paper: Paper;
  customWidthMm: number;
  customHeightMm: number;
  landscape: boolean;
  marginMm: number;
  columns: number;
  rows: number;
  gapMm: number;
  caption: string;
  captionPt: number;
  header: string;
  footer: string;
  pageNumbers: boolean;
  /** Crop marks at the corners of every cell. */
  cropMarks: boolean;
  textPt: number;
  /** The watermark set in the Export form, on every photo. */
  watermark: boolean;
  /** Output sharpening for a print looked at from reading distance. */
  sharpen: boolean;
  /** How strongly, when sharpening is on. */
  sharpenLevel: SharpenLevel;
  ppi: number;
  pdf: boolean;
  jpeg: boolean;
  where: 'output' | 'choose';
  folder?: string;
  name: string;
}

/** Output sharpening strength: the share of the print sharpening's amount. */
export const SHARPEN_AMOUNTS = { low: 50, standard: 100, high: 150 } as const;
export type SharpenLevel = keyof typeof SHARPEN_AMOUNTS;

/** Lightroom's 4 × 5 contact sheet on A4: twenty numbered photos a page. */
export const DEFAULT_PROOF_DRAFT: ProofDraft = {
  paper: 'a4',
  customWidthMm: 210,
  customHeightMm: 297,
  landscape: false,
  marginMm: 12,
  columns: 4,
  rows: 5,
  gapMm: 4,
  caption: '{seq}  {orig}',
  captionPt: 8,
  header: '{job}',
  footer: '{date}',
  pageNumbers: true,
  cropMarks: false,
  textPt: 10,
  watermark: false,
  sharpen: true,
  sharpenLevel: 'standard',
  ppi: 300,
  pdf: true,
  jpeg: false,
  where: 'output',
  name: '{job} proof sheet'
};

/** Ready-made captions; any naming template can be typed instead. */
export const CAPTION_PRESETS = ['{seq}  {orig}', '{orig}', '{seq}', '{seq}  {orig_base}  {rating|stars}', ''] as const;

const formKey = (library: string | null) => `rawtonic.proofSheet.form:${library ?? ''}`;

export function readProofDraft(library: string | null): ProofDraft {
  try {
    const saved = JSON.parse(localStorage.getItem(formKey(library)) ?? 'null');
    if (!saved || typeof saved !== 'object' || Array.isArray(saved)) return { ...DEFAULT_PROOF_DRAFT };
    const draft: Record<string, unknown> = { ...DEFAULT_PROOF_DRAFT };
    for (const [name, fallback] of Object.entries(DEFAULT_PROOF_DRAFT)) {
      if (name in saved && typeof saved[name] === typeof fallback) draft[name] = saved[name];
    }
    if (typeof saved.folder === 'string') draft.folder = saved.folder;
    if (!Object.hasOwn(SHARPEN_AMOUNTS, draft.sharpenLevel as string))
      draft.sharpenLevel = DEFAULT_PROOF_DRAFT.sharpenLevel;
    if (!Object.hasOwn(PAPERS, draft.paper as string) && draft.paper !== 'custom')
      draft.paper = DEFAULT_PROOF_DRAFT.paper;
    return draft as unknown as ProofDraft;
  } catch {
    return { ...DEFAULT_PROOF_DRAFT };
  }
}

export function writeProofDraft(library: string | null, draft: ProofDraft): void {
  try {
    localStorage.setItem(formKey(library), JSON.stringify(draft));
  } catch {
    // Kept for this dialog only.
  }
}

const clamp = (value: number, min: number, max: number, fallback: number) =>
  Number.isFinite(value) ? Math.min(max, Math.max(min, value)) : fallback;

/** The page in millimetres, turned for landscape. */
export function pageSize(draft: ProofDraft): [number, number] {
  const [w, h] =
    draft.paper === 'custom'
      ? [clamp(draft.customWidthMm, 20, 2000, 210), clamp(draft.customHeightMm, 20, 2000, 297)]
      : PAPERS[draft.paper];
  const [short, long] = [Math.min(w, h), Math.max(w, h)];
  return draft.landscape ? [long, short] : [short, long];
}

/** The layout the backend writes (`rawtonic_export::proof_sheet::ProofSheet`). */
export function sheetFromDraft(draft: ProofDraft) {
  const [pageWidthMm, pageHeightMm] = pageSize(draft);
  const margin = clamp(draft.marginMm, 0, 100, 12);
  return {
    pageWidthMm,
    pageHeightMm,
    marginsMm: [margin, margin, margin, margin],
    columns: Math.round(clamp(draft.columns, 1, 20, 4)),
    rows: Math.round(clamp(draft.rows, 1, 30, 5)),
    gapMm: clamp(draft.gapMm, 0, 50, 4),
    caption: draft.caption.trim() ? draft.caption : null,
    captionPt: clamp(draft.captionPt, 4, 72, 8),
    header: draft.header.trim() ? draft.header : null,
    footer: draft.footer.trim() ? draft.footer : null,
    pageNumbers: draft.pageNumbers,
    cropMarks: draft.cropMarks,
    textPt: clamp(draft.textPt, 4, 72, 10),
    ppi: Math.round(clamp(draft.ppi, 72, 1200, 300)),
    pdf: draft.pdf,
    jpeg: draft.jpeg,
    jpegQuality: 92
  };
}

/** Sharpening for a print of the sheet's ppi seen from 30 cm (the export's
 * print sharpening), or none. */
export function sheetSharpening(draft: ProofDraft) {
  if (!draft.sharpen) return null;
  return {
    amount: SHARPEN_AMOUNTS[draft.sharpenLevel] ?? 100,
    radius: printRadius(30, clamp(draft.ppi, 72, 1200, 300)),
    threshold: 1
  };
}

/** Which photos a sheet holds: the selection, else every photo shown, in the
 * order the grid shows them. */
export function sheetPhotos(shown: string[], selected: string[]): string[] {
  if (!selected.length) return shown;
  const chosen = new Set(selected);
  const ordered = shown.filter((path) => chosen.has(path));
  // A selected photo not shown (filtered out since) still comes along, last.
  const rest = selected.filter((path) => !ordered.includes(path));
  return [...ordered, ...rest];
}

interface ProofSheetDialogState {
  paths: string[] | null;
  show(paths: string[]): void;
  close(): void;
}

export const useProofSheetDialog = create<ProofSheetDialogState>((set) => ({
  paths: null,
  show: (paths) => set({ paths }),
  close: () => set({ paths: null })
}));
