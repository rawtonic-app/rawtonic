/** A call of `run` throttled: at most once per `interval` ms, never two runs
 * at once (a slow run delays the next), and a call always gets a run after
 * it (the last call of a burst is never lost). The first call of a burst
 * runs at once (next task). For refreshing a view while many events arrive,
 * as the grid does while an import copies photographs. */
export interface Throttled {
  (): void;
  /** Drops a run that is waiting (not one that is running). */
  cancel(): void;
}

export function throttleAsync(run: () => Promise<unknown> | unknown, interval: number): Throttled {
  let last = -Infinity;
  let running = false;
  let pending = false;
  let timer: ReturnType<typeof setTimeout> | null = null;

  const schedule = () => {
    if (timer !== null || running) return;
    timer = setTimeout(fire, Math.max(0, last + interval - Date.now()));
  };
  const fire = async () => {
    timer = null;
    if (!pending) return;
    pending = false;
    running = true;
    last = Date.now();
    try {
      await run();
    } catch {
      // A failed refresh is retried by the next call.
    } finally {
      running = false;
      if (pending) schedule();
    }
  };
  const call = (() => {
    pending = true;
    schedule();
  }) as Throttled;
  call.cancel = () => {
    if (timer !== null) clearTimeout(timer);
    timer = null;
    pending = false;
  };
  return call;
}
