// SPDX-License-Identifier: AGPL-3.0-or-later
/**
 * A camera TIFF's edit that cannot tell whether it was made on the file's
 * embedded thumbnail (before 0.1.0-alpha.17.1) or on its full frame
 * (`recipe_frame::undecided` in crates/engine/src/recipe_frame.rs: the RAW
 * passes' versions with only a crop or spots within the thumbnail's
 * corner) opens as it was saved. The editor says so and offers to move it
 * to the full image, one History step; saving keeps the choice (the saved
 * recipe records its frame, so the question does not come again).
 */
import { useEffect } from 'react';
import { invoke } from '@tauri-apps/api/core';
import i18n from 'i18next';
import { debouncedSetHistory, useEditorStore } from '../store/useEditorStore';
import { Invokes } from '../components/ui/AppProperties';
import type { Adjustments } from '../utils/adjustments';
import { notify } from '../activity/notify';
import { useEditorActions } from './useEditorActions';

const isTiff = (path: string) => /\.tiff?$/i.test(path.split('?')[0]);

export function useUndecidedFrame() {
  const ready = useEditorStore((s) => !!s.selectedImage?.isReady);
  const path = useEditorStore((s) => s.selectedImage?.path);
  const { setAdjustments } = useEditorActions();
  useEffect(() => {
    if (!ready || !path || !isTiff(path)) return;
    let active = true;
    const id = `frame-move:${path}`;
    const moved = (adjustments: Adjustments) =>
      invoke<Adjustments | null>(Invokes.UndecidedFrameMove, { path, adjustments });
    const move = async () => {
      const now = useEditorStore.getState();
      if (now.selectedImage?.path !== path) return;
      const result = await moved(now.adjustments);
      if (!result || useEditorStore.getState().selectedImage?.path !== path) return;
      debouncedSetHistory.flush();
      setAdjustments(result);
      debouncedSetHistory.flush();
    };
    moved(useEditorStore.getState().adjustments)
      .then((result) => {
        if (!active || !result) return;
        notify.warn(i18n.t('editor.frameMove.undecided'), {
          toastId: id,
          paths: [path],
          actions: [{ label: i18n.t('editor.frameMove.action'), run: move, settles: true }]
        });
      })
      .catch((error) => console.error('Checking the frame of the edit failed:', error));
    return () => {
      active = false;
      notify.dismiss(id);
    };
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [ready, path]);
}
