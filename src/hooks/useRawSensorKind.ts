import { invoke } from '@tauri-apps/api/core';
import { useEffect, useState } from 'react';
import { Invokes } from '../components/ui/AppProperties';

type SensorKind = 'bayer' | 'xTrans' | 'unknown';

/** The RAW sensor, or null while the current photo's probe is pending. */
export function useRawSensorKind(path: string | undefined, isRaw: boolean | undefined): SensorKind | null {
  const [answer, setAnswer] = useState<{ path: string; sensor: SensorKind } | null>(null);
  useEffect(() => {
    if (!path || !isRaw) return;
    let current = true;
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
  }, [path, isRaw]);
  if (!path || !isRaw) return null;
  return answer?.path === path ? answer.sensor : null;
}
