import { describe, expect, it } from 'vitest';
import { initialAdjustmentsFor } from '../utils/adjustments';
import fixture from '../../crates/engine/tests/fixtures/new-edit-recipe.raw.json';

// The Rust render-input coverage test (crates/export/tests/render_inputs_coverage.rs)
// perturbs every key of the editor's new-edit RAW recipe. It reads that recipe from a
// committed fixture; this test keeps the fixture equal to what the editor starts with.
// After a deliberate change to `initialAdjustmentsFor`, regenerate the fixture from it.
describe('new-edit recipe fixture', () => {
  it('equals initialAdjustmentsFor(true)', () => {
    expect(JSON.parse(JSON.stringify(initialAdjustmentsFor(true)))).toEqual(fixture);
  });
});
