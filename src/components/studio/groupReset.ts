import type { TFunction } from 'i18next';
import { debouncedSetHistory, useEditorStore } from '../../store/useEditorStore';
import { changesValues, nameHistoryStep } from './historyLabels';

/**
 * A tool group's reset (Lightroom's Alt-click on a group title) as one named
 * undo step, "Reset <group>". `reset` is the group's own reset, which writes
 * through the same per-slider path (defaults and version keys as the reset
 * button). A reset of values already at their defaults names no step.
 */
export function resetGroup(reset: () => void, group: string) {
  // An edit still settling is its own step, before the reset.
  debouncedSetHistory.flush();
  const before = useEditorStore.getState();
  const was = before.history[before.historyIndex];
  reset();
  debouncedSetHistory.flush();
  const { history, historyIndex } = useEditorStore.getState();
  const step = history[historyIndex];
  if (step && step !== was && changesValues(was, step))
    nameHistoryStep(step, (t: TFunction) => t('studio.develop.history.resetGroup', { tool: group }));
}
