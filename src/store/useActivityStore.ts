// SPDX-License-Identifier: AGPL-3.0-or-later
/**
 * The activity centre's one store: what runs in the background (with its
 * progress and Cancel), what finished (recent results, which expire), and
 * what needs the user (warnings and failures with their actions, which stay
 * until resolved or dismissed). Every reporting path feeds it: the export
 * queue, imports, AI tasks, model downloads, tethering, the photo write
 * queue, file operations and the app's own notices (`activity/notify`).
 * Toasts only confirm the user's own action and are logged here too.
 *
 * Decision: notes `agent-knowledge/ui-product-decisions.md`, "Activity centre".
 */
import { create } from 'zustand';
import type { ReactNode } from 'react';

export type ActivityStatus = 'running' | 'done' | 'warning' | 'failure';

/** Where an entry comes from (its icon and Copy details' heading). */
export type ActivitySource =
  | 'export'
  | 'import'
  | 'ai'
  | 'download'
  | 'tether'
  | 'write'
  | 'files'
  | 'xmp'
  | 'batch'
  | 'app';

export interface ActivityAction {
  label: string;
  run(): unknown;
  /** Running it settles the entry (it goes): Try again, Forget. */
  settles?: boolean;
}

/** Rich content a notice brought (`notify` with JSX); `close` dismisses it. */
export type ActivityBody = ReactNode | ((context: { closeToast(): void }) => ReactNode);

export interface ActivityEntry {
  /** Stable per thing reported: a later report under the same id replaces it. */
  id: string;
  status: ActivityStatus;
  source: ActivitySource;
  /** One line, plain text: the slot, screen readers and Copy details use it. */
  title: string;
  detail?: string;
  body?: ActivityBody;
  /** Absent while running: progress unknown (a spinner). */
  progress?: { done: number; total: number };
  cancel?: () => unknown;
  actions?: ActivityAction[];
  /** The photographs it is about: Show photos. */
  paths?: string[];
  createdAt: number;
  updatedAt: number;
}

export type ActivityInput = Omit<ActivityEntry, 'id' | 'status' | 'createdAt' | 'updatedAt'> & { id?: string };
type ActivityPatch = Partial<Omit<ActivityEntry, 'id' | 'createdAt' | 'updatedAt'>>;

/** How long a finished task's result stays under Done. */
export const DONE_KEPT_MS = 15 * 60 * 1000;
/** At most this many results are kept (the oldest go first). */
export const DONE_KEPT = 40;
/** At most this many entries need attention; beyond, the oldest go. */
const ATTENTION_KEPT = 200;

interface ActivityState {
  entries: ActivityEntry[];
  /** Whether the popover is open (the slot's button and the shortcut). */
  open: boolean;
  /** Starts (or restarts) a running task; returns its id. */
  start(input: ActivityInput): string;
  /** Changes an entry in place (progress, title, actions). */
  update(id: string, patch: ActivityPatch): void;
  /** A task finished: with a result it goes under Done, else it goes. */
  finish(id: string, result?: Omit<ActivityInput, 'id' | 'source'> & { source?: ActivitySource }): void;
  /** Reports a warning, failure or result; returns its id. */
  report(status: Exclude<ActivityStatus, 'running'>, input: ActivityInput): string;
  /** The user dismissed it (running tasks are cancelled, not dismissed). */
  dismiss(id: string): void;
  /** What it reported is no longer so (written after all, cleared). */
  resolve(id: string): void;
  clearDone(): void;
  /** Drops results older than `DONE_KEPT_MS` (run by a timer). */
  expire(now?: number): void;
  setOpen(open: boolean | ((open: boolean) => boolean)): void;
}

let nextId = 1;
let expiryTimer: ReturnType<typeof setTimeout> | null = null;

const isAttention = (entry: ActivityEntry) => entry.status === 'failure' || entry.status === 'warning';

/** Keeps the newest `DONE_KEPT` results and `ATTENTION_KEPT` notices. */
function trim(entries: ActivityEntry[]): ActivityEntry[] {
  const done = entries.filter((entry) => entry.status === 'done');
  const attention = entries.filter(isAttention);
  if (done.length <= DONE_KEPT && attention.length <= ATTENTION_KEPT) return entries;
  const drop = new Set([
    ...done.sort((a, b) => a.updatedAt - b.updatedAt).slice(0, Math.max(0, done.length - DONE_KEPT)),
    ...attention.sort((a, b) => a.updatedAt - b.updatedAt).slice(0, Math.max(0, attention.length - ATTENTION_KEPT)),
  ]);
  return entries.filter((entry) => !drop.has(entry));
}

function put(entries: ActivityEntry[], entry: ActivityEntry): ActivityEntry[] {
  const at = entries.findIndex((other) => other.id === entry.id);
  if (at < 0) return trim([...entries, entry]);
  const next = entries.slice();
  next[at] = { ...entry, createdAt: entries[at].createdAt };
  return trim(next);
}

export const useActivityStore = create<ActivityState>((set, get) => {
  /** One timer for the earliest result to expire. */
  const schedule = () => {
    if (expiryTimer) clearTimeout(expiryTimer);
    expiryTimer = null;
    const oldest = Math.min(...get().entries.filter((e) => e.status === 'done').map((e) => e.updatedAt));
    if (!Number.isFinite(oldest)) return;
    expiryTimer = setTimeout(() => get().expire(), Math.max(0, oldest + DONE_KEPT_MS - Date.now()) + 50);
  };
  const entry = (status: ActivityStatus, input: ActivityInput): ActivityEntry => {
    const now = Date.now();
    const { id, ...rest } = input;
    return { ...rest, id: id ?? `activity-${nextId++}`, status, createdAt: now, updatedAt: now };
  };
  return {
    entries: [],
    open: false,
    start(input) {
      const started = entry('running', input);
      set((state) => ({ entries: put(state.entries, started) }));
      return started.id;
    },
    update(id, patch) {
      set((state) => ({
        entries: state.entries.map((e) => (e.id === id ? { ...e, ...patch, updatedAt: Date.now() } : e)),
      }));
      schedule();
    },
    finish(id, result) {
      const running = get().entries.find((e) => e.id === id);
      if (!result) {
        get().resolve(id);
        return;
      }
      const done = entry('done', { source: running?.source ?? 'app', ...result, id });
      set((state) => ({ entries: put(state.entries, done) }));
      schedule();
    },
    report(status, input) {
      const reported = entry(status, input);
      set((state) => ({ entries: put(state.entries, reported) }));
      if (status === 'done') schedule();
      return reported.id;
    },
    dismiss(id) {
      set((state) => ({ entries: state.entries.filter((e) => e.id !== id || e.status === 'running') }));
    },
    resolve(id) {
      set((state) => ({ entries: state.entries.filter((e) => e.id !== id) }));
    },
    clearDone() {
      set((state) => ({ entries: state.entries.filter((e) => e.status !== 'done') }));
      schedule();
    },
    expire(now = Date.now()) {
      set((state) => ({
        entries: state.entries.filter((e) => e.status !== 'done' || now - e.updatedAt < DONE_KEPT_MS),
      }));
      schedule();
    },
    setOpen(open) {
      set((state) => ({ open: typeof open === 'function' ? open(state.open) : open }));
    },
  };
});

/** The popover's three sections, newest first. */
export function activitySections(entries: ActivityEntry[]) {
  const newest = (a: ActivityEntry, b: ActivityEntry) => b.updatedAt - a.updatedAt;
  return {
    attention: entries
      .filter(isAttention)
      // Failures before warnings.
      .sort((a, b) => (a.status === b.status ? newest(a, b) : a.status === 'failure' ? -1 : 1)),
    running: entries.filter((e) => e.status === 'running').sort((a, b) => a.createdAt - b.createdAt),
    done: entries.filter((e) => e.status === 'done').sort(newest),
  };
}

/** The text Copy details puts on the clipboard. */
export function activityDetails(entry: ActivityEntry, renderedText?: string): string {
  const lines = [entry.title];
  if (entry.detail) lines.push(entry.detail);
  if (renderedText && renderedText.trim() && renderedText.trim() !== entry.title) lines.push(renderedText.trim());
  if (entry.paths?.length) lines.push('', ...entry.paths.map((path) => path.split('?vc=')[0]));
  return lines.join('\n');
}

/** Test helper: back to an empty store. */
export function resetActivityStore() {
  if (expiryTimer) clearTimeout(expiryTimer);
  expiryTimer = null;
  useActivityStore.setState({ entries: [], open: false });
}
