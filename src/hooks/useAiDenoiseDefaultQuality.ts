import { invoke } from '@tauri-apps/api/core';
import { useEffect, useState } from 'react';
import { Invokes, type AiDenoiseQuality } from '../components/ui/AppProperties';
import { useSettingsStore } from '../store/useSettingsStore';

/** The AI Denoise quality photos get on this computer when AI Denoise is first
 * turned on: Preferences' choice as soon as it is made (the store holds it
 * before the backend has saved it), else for Automatic this computer's
 * (`ai_denoise_default_quality`: Fast where MatchFilter would run on the
 * processor), asked again for each `photo` so a GPU check's verdict that
 * arrived meanwhile counts; null until known. */
export function useAiDenoiseDefaultQuality(photo?: string): AiDenoiseQuality | null {
  const preference = useSettingsStore((s) => s.appSettings?.aiDenoiseDefaultQuality);
  const chosen = preference === 'fast' || preference === 'full' ? preference : null;
  const [automatic, setAutomatic] = useState<AiDenoiseQuality | null>(null);
  useEffect(() => {
    if (chosen) return;
    let current = true;
    invoke<string>(Invokes.AiDenoiseDefaultQuality)
      .then((value) => {
        if (current) setAutomatic(value === 'fast' ? 'fast' : 'full');
      })
      .catch((error) => console.error('AI Denoise default quality:', error));
    return () => {
      current = false;
    };
  }, [chosen, photo]);
  return chosen ?? automatic;
}
