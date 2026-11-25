import { invoke } from '@tauri-apps/api/core';
import { useEffect, useState } from 'react';
import { Invokes } from '../components/ui/AppProperties';

type SensorKind = 'bayer' | 'xTrans' | 'unknown';

/** The RAW sensor, or null while the probe is pending or the original is offline. */
export function useRawSensorKind(
  path: string | undefined,
  isRaw: boolean | undefined,
  offline = false
): SensorKind | null {
  const [answer, setAnswer] = useState<{ path: string; sensor: SensorKind } | null>(null);
  useEffect(() => {
    if (!path || !isRaw || offline) return;
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
