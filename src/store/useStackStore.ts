import { create } from 'zustand';
import { invoke } from '@tauri-apps/api/core';
import { EditedStatus, Invokes, RawStatus, type FilterCriteria, type ImageFile } from '../components/ui/AppProperties';
import { selectionCount } from '../utils/librarySearch';
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
  /** Its place among the listed members, 0 for the shown top. */
  index: number;
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

/** Whether a filter or search narrows the listing: then stack members are
 * listed singly, so a filter never hides a matching photo inside a
 * collapsed stack. */
export function listingFiltered(
  filter: FilterCriteria,
  search: { text: string; tags: string[] },
  catalogMatches: Set<string> | null = null,
): boolean {
  return search.text.trim() !== '' || search.tags.length > 0 || catalogMatches !== null
    || (filter.flag ?? 'all') !== 'all' || filter.rating !== 0 || (filter.colors?.length ?? 0) > 0
    || (filter.rawStatus ?? RawStatus.All) !== RawStatus.All
    || (filter.editedStatus ?? EditedStatus.All) !== EditedStatus.All
    || (filter.eyes ?? 'all') !== 'all' || selectionCount(filter.metadata) > 0;
}

/** Whether the shown listing collapses stacks: a folder or the whole
 * session/catalog (not an album, smart album or other view) without a
 * filter or search. */
export function stacksCollapse(state = useLibraryStore.getState()): boolean {
  return state.activeAlbumId === null
    && !listingFiltered(state.filterCriteria, state.searchCriteria, state.catalogMatches);
}

/**
 * Keeps the selection to photos the grid shows with `byPath`: a member
 * hidden in a collapsed stack leaves it and its stack's shown member (the
 * first listed) takes its place, so a rating, flag, export or move acts on
 * the top only (Lightroom). Albums and other views list members singly.
 */
export function keepSelectionShown(byPath: Map<string, StackEntry>) {
  const library = useLibraryStore.getState();
  // Views listing members singly show every member: nothing to move.
  if (!stacksCollapse(library)) return;
  const listed = new Map(library.imageList.map((f) => [stackKey(f.path), f.path]));
  const visible = (path: string) => {
    const entry = byPath.get(stackKey(path));
    if (!entry?.stack.collapsed) return path;
    const top = entry.stack.members.find((m) => listed.has(stackKey(m)));
    return top ? listed.get(stackKey(top))! : path;
  };
  const next = [...new Set(library.multiSelectedPaths.map(visible))];
  const active = library.libraryActivePath ? visible(library.libraryActivePath) : null;
  // The Shift-click anchor too: a hidden one would start no range.
  const anchor = library.selectionAnchorPath ? visible(library.selectionAnchorPath) : null;
  if (next.join('\n') !== library.multiSelectedPaths.join('\n') || active !== library.libraryActivePath
    || anchor !== library.selectionAnchorPath)
    library.setLibrary({ multiSelectedPaths: next, libraryActivePath: active, selectionAnchorPath: anchor });
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
    if (run !== generation) return;
    // First the selection, then the stacks, in one go: the grid drops
    // selected photos that leave it, so a member a collapse hides gives its
    // place to its stack's shown member before it is hidden.
    const byPath = indexStacks(stacks);
    keepSelectionShown(byPath);
    useStackStore.setState({ library, stacks, byPath });
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
        index: i,
        position: shown.length === 1 ? 'only' : i === 0 ? 'first' : i === shown.length - 1 ? 'last' : 'middle',
      });
    });
  }
  return { list: arranged, marks };
}
