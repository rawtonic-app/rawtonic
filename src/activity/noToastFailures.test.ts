// A failure is never only a toast that vanishes: the app's code reaches
// toasts only through `notify`, whose `error` and `warn` go to the activity
// centre (notes: agent-knowledge/ui-product-decisions.md, "Activity centre").
import { expect, it } from 'vitest';

const sources = import.meta.glob<string>(['../**/*.{ts,tsx}', '!../**/*.test.{ts,tsx}'], {
  eager: true,
  query: '?raw',
  import: 'default',
});
/** The two modules allowed to use react-toastify itself. */
const ALLOWED = ['../components/ui/AppNotices.tsx', './notify.ts'];

it('has no code but notify and the notice area using react-toastify', () => {
  const users = Object.entries(sources)
    .filter(([, text]) => /from ['"]react-toastify['"]/.test(text))
    .map(([path]) => path);
  expect(users.sort()).toEqual(ALLOWED);
});

it('has no toast.error or toast.warn left anywhere', () => {
  const left = Object.entries(sources)
    .filter(([, text]) => /\btoast\.(error|warn|warning)\(/.test(text))
    .map(([path]) => path);
  expect(left).toEqual([]);
});
