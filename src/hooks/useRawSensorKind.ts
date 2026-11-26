import { invoke } from '@tauri-apps/api/core';
import { useEffect, useRef, useState } from 'react';
import { Invokes } from '../components/ui/AppProperties';

type SensorKind = 'bayer' | 'xTrans' | 'unknown';

/**
 * The RAW sensor, or null while the probe is pending. An offline original is
 * not read: the backend answers with the kind its session or catalog
 * remembered from an earlier read of the same file (unknown without one),
 * and a kind read while this photo was online is kept.
 */
export function useRawSensorKind(
  path: string | undefined,
  isRaw: boolean | undefined,
  offline = false
): SensorKind | null {
  const [answer, setAnswer] = useState<{ path: string; sensor: SensorKind } | null>(null);
  const known = useRef(answer);
  useEffect(() => {
    known.current = answer;
  }, [answer]);
  useEffect(() => {
    if (!path || !isRaw) return;
    const kept = known.current;
    if (offline && kept?.path === path && kept.sensor !== 'unknown') return;
    let current = true;
    // The same path is probed again after its original comes back.
    setAnswer(null);
    invoke<SensorKind>(Invokes.RawSensorKind, { path })
      .then((sensor) => {
        if (current) setAnswer({ path, sensor: sensor === 'bayer' || sensor === 'xTrans' ? sensor : 'unknown' });
      })
      .catch((error) => {
        console.error('RAW sensor:', error);
        if (current) setAnswer({ path, sensor: 'unknown' });
      });
    return () => {
      current = false;
    };
  }, [path, isRaw, offline]);
  if (!path || !isRaw) return null;
  return answer?.path === path ? answer.sensor : null;
}
