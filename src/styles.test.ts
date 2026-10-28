import { expect, it } from 'vitest';

// jsdom computes no cascade from the style sheets; the rules are checked as text.
const read = async (file: string): Promise<string> => {
  const fs = await import(/* @vite-ignore */ 'node:fs' as string);
  return fs.readFileSync(file, 'utf8');
};

it('shows a keyboard focus ring everywhere and none on a mouse click', async () => {
  const styles = await read('src/styles.css');
  expect(styles).not.toMatch(/outline:\s*none\s*!important/);
  expect(styles).toMatch(/:focus:not\(:focus-visible\)\s*\{\s*outline:\s*none;/);
  expect(styles).toMatch(/\n\s*:focus-visible\s*\{\s*outline:\s*2px solid/);
});
