import { create } from 'zustand';
import { invoke } from '@tauri-apps/api/core';
import { Invokes, type ImageFile } from '../components/ui/AppProperties';
import { useLibraryStore } from './useLibraryStore';

/** A stack as `stacks_list` lists it: member paths (a virtual copy as
 * `<path>?vc=<id>`), the top first. */
export interface WebStack {
  id: string;
  collapsed: boolean;
  members: string[];
}

/** What a stack command changed (`StackChange` in rawtonic_catalog), kept
 * by the undo step and handed back to `stacks_restore`. */
export interface StackChange {
  before: unknown[];
  after: unknown[];
}

export interface StackEntry {
  stack: WebStack;
  /** Place in the stack, 0 for its top. */
  index: number;
}

/** How a photo is marked in the grid as part of a stack. */
export interface StackMark {
  stackId: string;
  /** Members of the stack in the listing. */
  count: number;
  collapsed: boolean;
  /** Shown first of its stack (its top among those listed). */
  top: boolean;
  /** Its place among the stack's members shown next to each other. */
  position: 'only' | 'first' | 'middle' | 'last';
}

interface StackState {
  library: string | null;
  stacks: WebStack[];
  byPath: Map<string, StackEntry>;
}

export const useStackStore = create<StackState>(() => ({
  library: null,
  stacks: [],
  byPath: new Map(),
}));

/** One key for a path whichever separator a listing used. */
export const stackKey = (path: string) => path.replaceAll('\\', '/');

export function indexStacks(stacks: WebStack[]): Map<string, StackEntry> {
  const byPath = new Map<string, StackEntry>();
  for (const stack of stacks)
    stack.members.forEach((path, index) => byPath.set(stackKey(path), { stack, index }));
  return byPath;
}

export function setStacks(library: string | null, stacks: WebStack[]) {
  useStackStore.setState({ library, stacks, byPath: indexStacks(stacks) });
}

let generation = 0;

/** Reads the stacks of `library` (the open session or catalog) again. */
export async function refreshStacks(library = useLibraryStore.getState().sessionDirectory) {
  const run = ++generation;
  if (!library) {
    setStacks(null, []);
    return;
  }
  try {
    const stacks = await invoke<WebStack[]>(Invokes.StacksList, { directory: library });
    if (run === generation) setStacks(library, stacks);
  } catch {
    if (run === generation) setStacks(library, []);
  }
}

/**
 * Arranges a listing (already filtered and sorted) by its stacks, as the
 * folder and session views show them (Lightroom): each stack's members
 * listed here sit together, in stack order, where the first of them in the
 * sort order is; a collapsed stack shows its top-most listed member alone,
 * marked with the count. A member
 * that leaves the listing (moved to the Trash, offline elsewhere) gives its
 * place to the next, so the next one is the top.
 */
export function arrangeStacks(
  list: ImageFile[],
  byPath: Map<string, StackEntry>,
): { list: ImageFile[]; marks: Map<string, StackMark> } {
  const marks = new Map<string, StackMark>();
  if (!byPath.size) return { list, marks };
  const groups = new Map<string, { file: ImageFile; index: number }[]>();
  for (const file of list) {
    const entry = byPath.get(stackKey(file.path));
    if (!entry) continue;
    const group = groups.get(entry.stack.id);
    if (group) group.push({ file, index: entry.index });
    else groups.set(entry.stack.id, [{ file, index: entry.index }]);
  }
  if (!groups.size) return { list, marks };
  for (const group of groups.values()) group.sort((a, b) => a.index - b.index);
  const arranged: ImageFile[] = [];
  const placed = new Set<string>();
  for (const file of list) {
    const entry = byPath.get(stackKey(file.path));
    if (!entry) {
      arranged.push(file);
      continue;
    }
    const { stack } = entry;
    if (placed.has(stack.id)) continue;
    placed.add(stack.id);
    const group = groups.get(stack.id)!;
    if (group.length < 2) {
      arranged.push(file);
      continue;
    }
    const shown = stack.collapsed ? group.slice(0, 1) : group;
    shown.forEach(({ file: member }, i) => {
      arranged.push(member);
      marks.set(member.path, {
        stackId: stack.id,
        count: group.length,
        collapsed: stack.collapsed,
        top: i === 0,
        position: shown.length === 1 ? 'only' : i === 0 ? 'first' : i === shown.length - 1 ? 'last' : 'middle',
      });
    });
  }
  return { list: arranged, marks };
}
