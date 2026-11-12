import type { FilterCriteria } from '../components/ui/AppProperties';
import { sourceOf } from '../entities/photo/photoRef';
import { pathKey } from './pathKey';

export type FlagFilter = NonNullable<FilterCriteria['flag']>;
export type FlagCounts = Record<FlagFilter, number>;

// Virtual copies share the source asset's pick/reject state in the catalog.
// Flags are kept under `pathKey` (see there): read them only through here.
export function imageFlag(flags: Record<string, number>, path: string): number {
  return flags[pathKey(sourceOf(path))] ?? 0;
}

export function flagName(flag: number): Exclude<FlagFilter, 'all'> {
  return flag === 1 ? 'picked' : flag === -1 ? 'rejected' : 'unflagged';
}
