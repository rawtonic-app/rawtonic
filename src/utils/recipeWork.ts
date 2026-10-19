/** Asynchronous work that ends by updating the open photograph's recipe (AI
 * masks, the placement of imported masks and spots): a batch edit that holds
 * the recipe (Auto straighten) starts only once none is running, and none
 * starts while it holds it, so no result is refused by the hold. */
let running = 0;
const idle = new Set<() => void>();

/** Counts `work` as running until it has settled (its recipe update included). */
export function trackRecipeWork<T>(work: Promise<T>): Promise<T> {
  running += 1;
  return work.finally(() => {
    running -= 1;
    if (running === 0) {
      const waiting = [...idle];
      idle.clear();
      waiting.forEach((resolve) => resolve());
    }
  });
}

/** When such work is running: a promise for the moment none is, else null. */
export function afterRecipeWork(): Promise<void> | null {
  if (running === 0) return null;
  return new Promise<void>((resolve) => idle.add(resolve));
}
