import { aiMaskModeKey, type AiMaskMode } from '../store/useProcessStore';

/** The kind of a component whose computation can wait for memory or run
 * the slower low-memory plan: Subject (`ai-foreground`) and Sky. */
const MODE_KINDS: Record<string, 'subject' | 'sky'> = { 'ai-foreground': 'subject', 'ai-sky': 'sky' };

/** Why the component `type` of the photo `path` takes longer to compute
 * (G74/BRK3), when it does. */
export function aiMaskModeOf(
  type: string,
  path: string | undefined,
  modes: Record<string, AiMaskMode>
): AiMaskMode | undefined {
  const kind = MODE_KINDS[type];
  return kind && path ? modes[aiMaskModeKey(kind, path)] : undefined;
}

/** The i18n key of a computing component's state. */
export function computingLabelKey(
  mode: AiMaskMode | undefined
): 'studio.masks.waitingForMemory' | 'studio.masks.computingSlow' | 'studio.masks.computing' {
  if (mode === 'waitingMemory') return 'studio.masks.waitingForMemory';
  if (mode === 'slow') return 'studio.masks.computingSlow';
  return 'studio.masks.computing';
}
