import { useLayoutEffect, useRef } from 'react';
import { useEditorStore } from '../store/useEditorStore';

type EditorState = ReturnType<typeof useEditorStore.getState>;

/** Applies `select(state)` to the DOM through `apply` whenever it changes,
 * without rendering the component. For data that arrives often and only
 * changes what one element draws: the scopes' ten updates a second during a
 * drag re-rendered the histogram, the curve and the scopes panel each time,
 * work on the webview's main thread that delayed the drag's preview frames
 * (notes: parity-gaps/G23-analysis.md, SCP1). `select` must be stable (a
 * module function or memoised); `deps` re-apply the current value, e.g.
 * when the channel `apply` draws changes. */
export function useEditorStoreDom<T>(
  select: (state: EditorState) => T,
  apply: (value: T) => void,
  deps: readonly unknown[] = []
) {
  const applyRef = useRef(apply);
  applyRef.current = apply;
  useLayoutEffect(() => {
    let last = select(useEditorStore.getState());
    applyRef.current(last);
    return useEditorStore.subscribe((state) => {
      const next = select(state);
      if (next === last) return;
      last = next;
      applyRef.current(next);
    });
    // eslint-disable-next-line react-hooks/exhaustive-deps -- deps re-apply on the caller's terms; select is stable by contract
  }, [select, ...deps]);
}
