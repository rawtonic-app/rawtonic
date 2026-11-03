// @ts-expect-error node:fs is untyped in the frontend tsconfig
import { readFileSync } from 'node:fs';
import { expect, it } from 'vitest';

// A smoothly turning or pulsing indicator makes the window server composite
// the transparent window on every display frame (WindowServer ~68 % GPU time
// vs ~13 % for eight steps a second; notes: parity-gaps/G43, task SPN).
it('turns and pulses busy indicators in steps, still under reduced motion', () => {
  const read = (path: string) => readFileSync(path, 'utf8') as string;
  const styles = read('src/styles.css');
  expect(styles).toMatch(/--animate-spin: spin 1s steps\(8\) infinite;/);
  expect(styles).toMatch(/@media \(prefers-reduced-motion: reduce\)\s*\{\s*\.animate-spin\s*\{\s*animation: none;/);
  expect(read('src/components/activity/activity.css')).toContain('animation: activity-turn 1s steps(8) infinite;');
  expect(read('src/components/studio/studio.css')).toContain(
    'animation: studio-mask-thumb-busy 1.2s steps(3) infinite;'
  );
  for (const path of ['src/styles.css', 'src/components/activity/activity.css', 'src/components/studio/studio.css']) {
    expect(read(path)).not.toMatch(/animation:[^;]*(linear|ease[a-z-]*) infinite/);
  }
});
