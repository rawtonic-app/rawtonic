import { invoke } from '@tauri-apps/api/core';
import { EditedStatus, Invokes, LibraryViewMode, RawStatus, type FilterCriteria } from '../components/ui/AppProperties';
import { MISSING_VIEW_ID } from './offline';
import { PREVIOUS_IMPORT_VIEW_ID } from './folderSync';

/**
 * The library's search rules (`rawtonic_catalog::query`): one model for the
 * Library filter and for smart albums. A rule is a condition on one field
 * or a group whose rules must all, any or none hold, nested freely.
 */
export type Field =
  | 'text'
  | 'filename'
  | 'keyword'
  | 'camera'
  | 'make'
  | 'lens'
  | 'fileType'
  | 'rating'
  | 'flag'
  | 'label'
  | 'iso'
  | 'focalLength'
  | 'aperture'
  | 'exposure'
  | 'captureDate'
  | 'importDate'
  | 'album'
  | 'folder'
  | 'offline'
  | 'previousImport';

export type Op =
  | 'contains'
  | 'containsAny'
  | 'notContains'
  | 'is'
  | 'isNot'
  | 'startsWith'
  | 'endsWith'
  | 'empty'
  | 'notEmpty'
  | 'lt'
  | 'le'
  | 'gt'
  | 'ge'
  | 'between'
  | 'inLast'
  | 'notInLast';

export type Combine = 'all' | 'any' | 'none';
export interface Condition {
  field: Field;
  op: Op;
  value?: unknown;
}
export interface Group {
  combine: Combine;
  rules: Rule[];
}
export type Rule = Condition | Group;

export const isGroup = (rule: Rule): rule is Group => 'combine' in rule;

/** The value lists of the Library filter, left to right (Lightroom's metadata
 * columns, Capture One's Filters tool). */
export const FACET_FIELDS = ['captureDate', 'camera', 'lens', 'keyword', 'fileType', 'iso', 'focalLength', 'aperture'] as const;
export type FacetField = (typeof FACET_FIELDS)[number];
/** Values chosen per list ("" = no value: no keyword, unknown camera, …). */
export type MetadataSelection = Partial<Record<FacetField, string[]>>;
export interface FacetValue {
  value: string;
  count: number;
}
export type Facets = Partial<Record<FacetField, FacetValue[]>>;

const NUMERIC: ReadonlySet<FacetField> = new Set(['iso', 'focalLength', 'aperture']);

const all = (rules: Rule[]): Rule => (rules.length === 1 ? rules[0] : { combine: 'all', rules });

/** The last day of a `YYYY-MM` month. */
function monthEnd(month: string): string {
  const [year, m] = month.split('-').map(Number);
  const day = new Date(Date.UTC(year, m, 0)).getUTCDate();
  return `${month}-${String(day).padStart(2, '0')}`;
}

/** One chosen value of a list as a condition. */
function valueRule(field: FacetField, value: string): Rule {
  if (value === '') return { field, op: 'empty' };
  if (field === 'captureDate') return { field, op: 'between', value: [`${value}-01`, monthEnd(value)] };
  if (field === 'fileType') return { field, op: 'is', value };
  if (NUMERIC.has(field)) return { field, op: 'is', value: Number(value) };
  return { field, op: 'is', value };
}

/** The chosen values: any value of a list, every list (Lightroom's columns).
 * `except` leaves one list out (its own counts). */
export function selectionRules(selection: MetadataSelection | undefined, except?: FacetField): Rule[] {
  const rules: Rule[] = [];
  for (const field of FACET_FIELDS) {
    const values = selection?.[field];
    if (field === except || !values?.length) continue;
    const conditions = values.map((value) => valueRule(field, value));
    rules.push(conditions.length === 1 ? conditions[0] : { combine: 'any', rules: conditions });
  }
  return rules;
}

/** The search box: every word somewhere in the searchable text. */
export const textRules = (text: string): Rule[] =>
  text.trim() ? [{ field: 'text', op: 'contains', value: text.trim() }] : [];

/** The Library's culling filters (rating, label, flag) as rules: what the
 * filter bar shows, for a smart album made from it. */
export function cullingRules(filter: FilterCriteria): Rule[] {
  const rules: Rule[] = [];
  if (filter.rating === -1) rules.push({ field: 'rating', op: 'empty' });
  else if (filter.rating === 5) rules.push({ field: 'rating', op: 'is', value: 5 });
  else if (filter.rating > 0) rules.push({ field: 'rating', op: 'ge', value: filter.rating });
  if (filter.colors.length) rules.push({ field: 'label', op: 'is', value: filter.colors });
  if (filter.flag && filter.flag !== 'all') rules.push({ field: 'flag', op: 'is', value: filter.flag });
  return rules;
}

/** What the grid shows as a rule: a session album, Previous Import, the
 * missing photographs or a folder (with its subfolders in the recursive
 * view). `null`: nothing the catalog can scope (no library). */
export function scopeRule(
  folder: string | null,
  albumId: string | null,
  viewMode: LibraryViewMode | undefined,
): Rule | null {
  if (albumId === PREVIOUS_IMPORT_VIEW_ID) return { field: 'previousImport', op: 'is', value: true };
  if (albumId === MISSING_VIEW_ID) return { field: 'offline', op: 'is', value: true };
  if (albumId?.startsWith('session-album:'))
    return { field: 'album', op: 'is', value: albumId.slice('session-album:'.length) };
  if (albumId || !folder) return null;
  return { field: 'folder', op: viewMode === LibraryViewMode.Recursive ? 'contains' : 'is', value: folder };
}

/** The search box and the chosen values as one rule (null: nothing to search). */
export function searchRule(text: string, selection: MetadataSelection | undefined): Rule | null {
  const rules = [...textRules(text), ...selectionRules(selection)];
  return rules.length ? all(rules) : null;
}

/** File types the grid counts as RAW (`is_raw`: the RAW formats, DNG among them). */
const RAW_TYPES = ['raw', 'dng'];

/** Everything the Library filters by now, for a new smart album: the shown
 * album or folder, the search box, the chosen values, the culling filters
 * and RAW / not RAW. Null when it also filters by what rules cannot say
 * (edited or not, search tags), so a smart album never silently shows more
 * than the filter did. */
export function filterAsRule(
  scope: Rule | null,
  text: string,
  selection: MetadataSelection | undefined,
  filter: FilterCriteria,
  tags: string[] = [],
): Group | null {
  if ((filter.editedStatus ?? EditedStatus.All) !== EditedStatus.All || tags.length > 0) return null;
  const raw: Rule[] =
    filter.rawStatus === RawStatus.RawOnly
      ? [{ field: 'fileType', op: 'is', value: RAW_TYPES }]
      : filter.rawStatus === RawStatus.NonRawOnly
        ? [{ field: 'fileType', op: 'isNot', value: RAW_TYPES }]
        : [];
  return {
    combine: 'all',
    rules: [
      ...(scope ? [scope] : []),
      ...textRules(text),
      ...selectionRules(selection),
      ...cullingRules(filter),
      ...raw,
    ],
  };
}

/** A library path without a virtual copy's `?vc=` suffix (the catalog finds
 * the photograph; its copies follow it). */
export const photographPath = (path: string): string => {
  const at = path.indexOf('?vc=');
  return at < 0 ? path : path.slice(0, at);
};

export const searchLibrary = (directory: string, rule: Rule) =>
  invoke<string[]>(Invokes.SessionSearch, { directory, rule });

/** Each list's values with counts within `base` and the other lists' choices. */
export function libraryFacets(directory: string, base: Rule[], selection: MetadataSelection | undefined) {
  const requests = FACET_FIELDS.map((field) => ({ field, rule: all([...base, ...selectionRules(selection, field)]) }));
  return invoke<Facets>(Invokes.SessionFacets, { directory, requests });
}

/** Number of chosen values. */
export const selectionCount = (selection: MetadataSelection | undefined) =>
  FACET_FIELDS.reduce((n, field) => n + (selection?.[field]?.length ?? 0), 0);
