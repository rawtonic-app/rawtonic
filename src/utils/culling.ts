import type { FilterCriteria } from '../components/ui/AppProperties';
import { sourceOf } from '../entities/photo/photoRef';

export type FlagFilter = NonNullable<FilterCriteria['flag']>;
export type FlagCounts = Record<FlagFilter, number>;

// Virtual copies share the source asset's pick/reject state in the catalog.
export function imageFlag(flags: Record<string, number>, path: string): number {
  return flags[sourceOf(path)] ?? 0;
}

export function flagName(flag: number): Exclude<FlagFilter, 'all'> {
  return flag === 1 ? 'picked' : flag === -1 ? 'rejected' : 'unflagged';
}
