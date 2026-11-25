import { invoke } from '@tauri-apps/api/core';
import { useEffect, useState } from 'react';
import { Invokes, type AiDenoiseQuality } from '../components/ui/AppProperties';
import { useSettingsStore } from '../store/useSettingsStore';

/** The AI Denoise quality photos get on this computer when AI Denoise is first
 * turned on (`ai_denoise_default_quality`: the Preferences choice, or Fast
 * where AI Denoise would run on the processor); null until known. */
export function useAiDenoiseDefaultQuality(): AiDenoiseQuality | null {
  const preference = useSettingsStore((s) => s.appSettings?.aiDenoiseDefaultQuality);
  const [quality, setQuality] = useState<AiDenoiseQuality | null>(null);
  useEffect(() => {
    let current = true;
    invoke<string>(Invokes.AiDenoiseDefaultQuality)
      .then((value) => {
        if (current) setQuality(value === 'fast' ? 'fast' : 'full');
      })
      .catch((error) => console.error('AI Denoise default quality:', error));
    return () => {
      current = false;
    };
  }, [preference]);
  return quality;
}
