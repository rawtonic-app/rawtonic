// The question's data as `plan_export_collisions` returns it; the same JSON is
// produced by Rust (`collisions_contract` in src-tauri/src/export_collisions.rs).
import { expect, it } from 'vitest';
import payloads from './exportCollisions.json';
import type { CollisionSummary } from '../components/ui/ExportImportProperties';

it('the form reads the summary as the Rust side writes it', () => {
  const summary: CollisionSummary = payloads.summary;
  expect(Object.keys(summary).sort()).toEqual(['changedOwn', 'originals', 'other', 'total', 'unchangedOwn']);
  expect(summary.total).toBe(summary.unchangedOwn.count + summary.changedOwn.count + summary.other.count);
  expect(summary.other.names).toEqual(['d.jpg']);
});
