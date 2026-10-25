import type { Adjustments } from "./adjustments";

// Orders belong to history slots. Resolving lens or mask data may replace
// every recipe object while leaving the user's history steps unchanged.
let order = 0;
const histories = new WeakMap<Adjustments[], number[]>();
const listeners = new Set<() => void>();
export const nextUndoOrder = () => ++order;
export const historyOrders = (history: Adjustments[]) =>
  histories.get(history) ?? history.map(() => 0);
export const editUndoOrder = (history: Adjustments[], index: number) =>
  historyOrders(history)[index] ?? 0;
export function setHistoryOrders(history: Adjustments[], orders: number[]) {
  histories.set(history, orders);
}
export function inheritHistoryOrders(
  previous: Adjustments[],
  next: Adjustments[],
  samePhoto: boolean,
) {
  if (!histories.has(next)) {
    const before = samePhoto && next.length > 1 ? historyOrders(previous) : [];
    histories.set(
      next,
      next.map((_, index) => before[index] ?? 0),
    );
  }
}
export function newEditOrder() {
  const next = nextUndoOrder();
  for (const listener of listeners) listener();
  return next;
}
export function onNewEdit(listener: () => void) {
  listeners.add(listener);
  return () => {
    listeners.delete(listener);
  };
}
