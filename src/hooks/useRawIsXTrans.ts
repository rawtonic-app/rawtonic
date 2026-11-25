import { invoke } from '@tauri-apps/api/core';
import { useEffect, useState } from 'react';
import { Invokes } from '../components/ui/AppProperties';

/** Whether the RAW at `path` has an X-Trans sensor (`raw_is_xtrans`, read from
 * its tags in a few milliseconds when the photo opens, before its preview is
 * editable): its AI Denoise runs the X-Trans network, which has one quality,
 * from `aiDenoiseVersion` 6 (`withXTransAiDenoiseVersion`). Null while
 * unknown; false for other photos and when the answer cannot be had. */
export function useRawIsXTrans(path: string | undefined, isRaw: boolean | undefined): boolean | null {
  const [answer, setAnswer] = useState<{ path: string; xtrans: boolean } | null>(null);
  useEffect(() => {
    if (!path || !isRaw) return;
    let current = true;
    invoke<boolean>(Invokes.RawIsXTrans, { path })
      .then((xtrans) => {
        if (current) setAnswer({ path, xtrans: xtrans === true });
      })
      .catch((error) => {
        console.error('X-Trans sensor:', error);
        if (current) setAnswer({ path, xtrans: false });
      });
    return () => {
      current = false;
    };
  }, [path, isRaw]);
  if (!path || !isRaw) return false;
  return answer?.path === path ? answer.xtrans : null;
}
