import type { FilterCriteria } from '../components/ui/AppProperties';
import { pathKey } from './pathKey';

export type FlagFilter = NonNullable<FilterCriteria['flag']>;
export type FlagCounts = Record<FlagFilter, number>;

// A virtual copy has its own pick/reject state, as in Lightroom (the catalog
// lists it under `<photo>?vc=<copy>`). Flags are kept under `pathKey` (see
// there): read them only through here.
export function imageFlag(flags: Record<string, number>, path: string): number {
  return flags[pathKey(path)] ?? 0;
}

export function flagName(flag: number): Exclude<FlagFilter, 'all'> {
  return flag === 1 ? 'picked' : flag === -1 ? 'rejected' : 'unflagged';
}
