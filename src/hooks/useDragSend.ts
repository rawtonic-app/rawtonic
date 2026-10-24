import { useEffect, useRef } from 'react';

/** Input closer than this to the previous engine update waits for the next
 * animation frame. */
export const MIN_SEND_INTERVAL_MS = 8;

/**
 * Sends a drag's values to `send` as input arrives, at most one per
 * MIN_SEND_INTERVAL_MS (closer input waits for the next animation frame),
 * like the sliders (AdjustmentControl). `flush` sends a waiting value now
 * (the drag ends); one still waiting at unmount is sent then.
 */
export function useDragSend<T>(send: (value: T) => void) {
  const callback = useRef(send);
  callback.current = send;
  const state = useRef<{ pending: T | null; sentAt: number; frame: number | null }>({
    pending: null,
    sentAt: -Infinity,
    frame: null,
  });
  const api = useRef({
    flush() {
      const s = state.current;
      if (s.frame !== null) cancelAnimationFrame(s.frame);
      s.frame = null;
      if (s.pending === null) return;
      const value = s.pending;
      s.pending = null;
      s.sentAt = performance.now();
      callback.current(value);
    },
    push(value: T) {
      const s = state.current;
      s.pending = value;
      if (performance.now() - s.sentAt >= MIN_SEND_INTERVAL_MS) api.current.flush();
      else if (s.frame === null) s.frame = requestAnimationFrame(api.current.flush);
    },
  });
  useEffect(() => () => api.current.flush(), []);
  return api.current;
}
