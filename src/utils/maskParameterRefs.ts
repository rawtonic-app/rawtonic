import type { Adjustments, MaskContainer } from './adjustments';
import type { SubMask, SubMaskParameters } from '../components/panel/right/Masks';

// Brush strokes a preview request sends by reference once the backend holds
// them (crates/engine/src/mask_parameters.rs). With many brush layers the
// unchanged strokes were most of every slider move's payload: copying,
// serialising and parsing them cost more than the frame.

/** The start of the error of a request whose reference the backend lacks. */
export const MASK_PARAMETERS_MISSING = 'MASK_PARAMETERS_MISSING';
const REFERENCE_KEY = '$paramsRef';
const TOKEN_KEY = 'paramsToken';
/** Small strokes are cheaper to send than to track. */
const MIN_POINTS = 64;

interface Token {
  token: string;
  fingerprint: string;
}

let revision = 0;
let tokens = new WeakMap<object, Token>();
let acknowledged = new Set<string>();

// Recipe objects are replaced, not edited, so the parameters object is the
// identity; the fingerprint also catches a stroke grown in place.
function fingerprint(parameters: SubMaskParameters): string | null {
  const lines = parameters.lines;
  if (!Array.isArray(lines)) return null;
  let points = 0;
  for (const line of lines) points += Array.isArray(line?.points) ? line.points.length : 0;
  if (points < MIN_POINTS) return null;
  const last = lines[lines.length - 1]?.points?.at(-1);
  return `${lines.length}:${points}:${last?.x},${last?.y}`;
}

function tokenFor(subMask: SubMask): string | null {
  if (subMask.type !== 'brush' || !subMask.parameters || typeof subMask.parameters !== 'object') return null;
  const print = fingerprint(subMask.parameters);
  if (print === null) return null;
  const known = tokens.get(subMask.parameters);
  if (known && known.fingerprint === print) return known.token;
  const token = `${subMask.id}#${++revision}`;
  tokens.set(subMask.parameters, { token, fingerprint: print });
  return token;
}

/**
 * `adjustments` for a preview request: brush components the backend holds
 * become references, the others carry a token naming them for later. Only
 * the containers on the way are copied; `sent` lists the new tokens.
 */
export function withParameterReferences(adjustments: Adjustments): { adjustments: Adjustments; sent: string[] } {
  const sent: string[] = [];
  if (!Array.isArray(adjustments.masks)) return { adjustments, sent };
  let changed = false;
  const masks = adjustments.masks.map((mask: MaskContainer) => {
    if (!Array.isArray(mask.subMasks)) return mask;
    let maskChanged = false;
    const subMasks = mask.subMasks.map((subMask: SubMask) => {
      const token = tokenFor(subMask);
      if (token === null) return subMask;
      maskChanged = true;
      if (acknowledged.has(token)) return { ...subMask, parameters: { [REFERENCE_KEY]: token } };
      sent.push(token);
      return { ...subMask, [TOKEN_KEY]: token };
    });
    if (!maskChanged) return mask;
    changed = true;
    return { ...mask, subMasks };
  });
  return { adjustments: changed ? { ...adjustments, masks } : adjustments, sent };
}

/** The backend received these tokens' parameters. */
export function acknowledgeParameters(sent: string[]) {
  sent.forEach((token) => acknowledged.add(token));
}

/** The backend lost its store (restart, eviction): send everything again. */
export function forgetParameterReferences() {
  acknowledged = new Set();
  tokens = new WeakMap();
}
