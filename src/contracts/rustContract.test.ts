// Contract between the frontend and the Rust side, until the IPC types are
// generated (architecture cleanup plan, step 0.2a). Each check compares what
// the two sides spell by hand; today's known differences are listed as
// expected, so any new drift fails and fixing one means deleting its entry.
// @ts-expect-error node:fs is untyped in the frontend tsconfig
import { readdirSync, readFileSync, statSync } from 'node:fs';
import { describe, expect, it } from 'vitest';
import { Invokes } from '../components/ui/AppProperties';
import { INITIAL_ADJUSTMENTS, INITIAL_MASK_ADJUSTMENTS, initialAdjustmentsFor } from '../utils/adjustments';

const read = (path: string): string => readFileSync(path, 'utf8') as string;

const rustFiles = (dir: string): string[] =>
  (readdirSync(dir) as string[]).flatMap((name) => {
    const path = `${dir}/${name}`;
    if ((statSync(path) as { isDirectory(): boolean }).isDirectory()) return rustFiles(path);
    return path.endsWith('.rs') ? [path] : [];
  });

const camel = (snake: string): string => snake.replace(/_([a-z0-9])/g, (_, c: string) => c.toUpperCase());

const difference = (a: Iterable<string>, b: Set<string>): string[] => [...a].filter((x) => !b.has(x)).sort();

describe('commands', () => {
  it('Invokes names exactly the commands lib.rs registers', () => {
    const lib = read('src-tauri/src/lib.rs');
    const start = lib.indexOf('generate_handler![');
    const list = lib.slice(start, lib.indexOf('])', start));
    const registered = new Set(
      [...list.matchAll(/([A-Za-z_][A-Za-z0-9_:]*)\s*,/g)].map(([, path]) => path.split('::').pop() as string),
    );
    const invoked = new Set(Object.values(Invokes) as string[]);
    expect(registered.size).toBeGreaterThan(300);
    expect({ notRegistered: difference(invoked, registered), notInInvokes: difference(registered, invoked) }).toEqual({
      notRegistered: [],
      notInInvokes: [],
    });
  });
});

describe('settings', () => {
  it('AppSettings has the same fields in TypeScript and Rust', () => {
    const rust = read('src-tauri/src/app_settings.rs');
    const start = rust.indexOf('pub struct AppSettings {');
    const body = rust.slice(start, rust.indexOf('\n}', start));
    expect(body).not.toMatch(/#\[serde\((rename|alias|flatten)\b/);
    const rustFields = new Set([...body.matchAll(/^\s+pub ([a-z0-9_]+):/gm)].map(([, name]) => camel(name)));
    const ts = read('src/components/ui/AppProperties.tsx');
    const tsStart = ts.indexOf('export interface AppSettings {');
    const tsBody = ts.slice(tsStart, ts.indexOf('\n}', tsStart));
    const tsFields = new Set([...tsBody.matchAll(/^ {2}([A-Za-z0-9_]+)\??:/gm)].map(([, name]) => name));
    expect(rustFields.size).toBeGreaterThan(100);
    expect({ onlyInRust: difference(rustFields, tsFields), onlyInTypeScript: difference(tsFields, rustFields) }).toEqual({
      // Known drift on aa8e7f3c (plan step 0.2b removes these entries).
      onlyInRust: ['groupAssociatedFiles', 'lastUpdateCheck', 'linuxGpuOptimizationMigratedV1', 'taggingThreadCount'],
      onlyInTypeScript: [],
    });
  });
});

describe('recipe', () => {
  // Every recipe key the editor writes must be spelled somewhere in the Rust
  // crates that read recipes; a key renamed on one side would otherwise
  // silently render as its default. Keys that only steer the editor's own
  // interface are listed as such.
  const rustSources = ['crates/engine/src', 'crates/gpu/src', 'crates/export/src', 'crates/rawdecode/src', 'src-tauri/src']
    .flatMap(rustFiles)
    .map(read)
    .join('\n');
  const spelled = (key: string) => rustSources.includes(`"${key}"`);

  it('every top-level key of a new edit is read by Rust', () => {
    const keys = new Set([...Object.keys(INITIAL_ADJUSTMENTS), ...Object.keys(initialAdjustmentsFor(true))]);
    expect(keys.size).toBeGreaterThan(100);
    expect([...keys].filter((key) => !spelled(key)).sort()).toEqual(
      // Known on aa8e7f3c: keys Rust never reads (interface state, or read under another spelling).
      [],
    );
  });

  it('every mask adjustment key is read by Rust', () => {
    const keys = Object.keys(INITIAL_MASK_ADJUSTMENTS);
    expect(keys.length).toBeGreaterThan(20);
    expect(keys.filter((key) => !spelled(key)).sort()).toEqual([]);
  });
});
