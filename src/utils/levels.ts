import { INITIAL_LEVELS, type Level, type Levels } from "./adjustments";

/** The Levels channels: the RGB composite and each colour channel. */
export const LEVEL_CHANNELS = ["rgb", "red", "green", "blue"] as const;
export type LevelChannel = (typeof LEVEL_CHANNELS)[number];

/** The five values of a channel as the panel shows them: input black,
 * midtone (−100…100, a gamma), input white, output black and white. */
export type LevelValue = "inBlack" | "midtone" | "inWhite" | "outBlack" | "outWhite";

export const MIDTONE_RANGE = 100;

/** Midtone value (−100…100) for a gamma and back: gamma 2^(m/50). */
export const midtoneOf = (gamma: number) =>
  Math.round(50 * Math.log2(Math.max(0.1, gamma)));
export const gammaOf = (midtone: number) =>
  Number((2 ** (midtone / 50)).toFixed(3));

const clamp = (v: number, lo: number, hi: number) =>
  Math.max(lo, Math.min(hi, v));

export const levelOf = (levels: Partial<Levels> | undefined, channel: LevelChannel): Level => ({
  ...INITIAL_LEVELS[channel],
  ...levels?.[channel],
});

/** A value of a level in the panel's units. */
export function levelValue(level: Level, key: LevelValue): number {
  return key === "midtone" ? midtoneOf(level.gamma) : level[key];
}

/** The level after setting one value (panel units), clamped to the range
 * the engine takes: the input black stays below the white (moving one
 * pushes the other, as the sliders did), the midtone within ±100. The
 * output points are free (black above white inverts). */
export function withLevelValue(level: Level, key: LevelValue, raw: number): Level {
  if (!Number.isFinite(raw)) return level;
  const value = Math.round(raw);
  switch (key) {
    case "inBlack": {
      const inBlack = clamp(value, 0, 254);
      return { ...level, inBlack, inWhite: Math.max(level.inWhite, inBlack + 1) };
    }
    case "inWhite": {
      const inWhite = clamp(value, 1, 255);
      return { ...level, inWhite, inBlack: Math.min(level.inBlack, inWhite - 1) };
    }
    case "midtone":
      return { ...level, gamma: gammaOf(clamp(value, -MIDTONE_RANGE, MIDTONE_RANGE)) };
    default:
      return { ...level, [key]: clamp(value, 0, 255) };
  }
}

/** The neutral value of each (double-click resets to it). */
export const LEVEL_DEFAULTS: Record<LevelValue, number> = {
  inBlack: 0,
  midtone: 0,
  inWhite: 255,
  outBlack: 0,
  outWhite: 255,
};

/** One key press: 1 level (midtone unit), Shift 10. */
export const nudgeStep = (shift: boolean) => (shift ? 10 : 1);

/** The level after a key press on a value, or null for other keys. */
export function nudgedLevel(
  level: Level,
  key: LevelValue,
  event: { key: string; shiftKey: boolean },
): Level | null {
  const step = nudgeStep(event.shiftKey);
  const current = levelValue(level, key);
  const next =
    event.key === "ArrowRight" || event.key === "ArrowUp"
      ? current + step
      : event.key === "ArrowLeft" || event.key === "ArrowDown"
        ? current - step
        : event.key === "Home"
          ? key === "midtone"
            ? -MIDTONE_RANGE
            : 0
          : event.key === "End"
            ? key === "midtone"
              ? MIDTONE_RANGE
              : 255
            : null;
  return next === null ? null : withLevelValue(level, key, next);
}

/** Where a handle sits on its strip (0–1): input and output points at
 * their level; the midtone where the input range maps to half the output
 * (Photoshop's midtone input, (x)^(1/gamma) = 0.5). */
export function handlePosition(level: Level, key: LevelValue): number {
  if (key === "midtone") {
    const t = 0.5 ** level.gamma;
    return (level.inBlack + t * (level.inWhite - level.inBlack)) / 255;
  }
  return level[key] / 255;
}

/** The level after dragging a handle to `position` (0–1 on its strip),
 * on the same value steps as the fields. */
export function withHandleAt(level: Level, key: LevelValue, position: number): Level {
  const p = clamp(position, 0, 1);
  if (key !== "midtone") return withLevelValue(level, key, p * 255);
  const span = Math.max(1, level.inWhite - level.inBlack);
  const t = clamp((p * 255 - level.inBlack) / span, 1e-4, 1 - 1e-4);
  const gamma = Math.log(t) / Math.log(0.5);
  return withLevelValue(level, "midtone", 50 * Math.log2(gamma));
}

const toEncoded = (v: number) => {
  const c = clamp(v, 0, 1);
  return c <= 0.0031308 ? c * 12.92 : 1.055 * c ** (1 / 2.4) - 0.055;
};

/** The shader's `level` for one display value (0–255). */
export function levelOutput(level: Level, v: number): number {
  const x = clamp((v - level.inBlack) / Math.max(level.inWhite - level.inBlack, 1e-4 * 255), 0, 1);
  const gamma = clamp(level.gamma, 0.1, 10);
  return level.outBlack + (level.outWhite - level.outBlack) * x ** (1 / gamma);
}

/**
 * The levels after the black or white point picker: `pixels` are the
 * colours (linear sRGB) Levels receives around the picked point, sampled
 * with Levels left out (editor_preview.rs `sample_curve_input`). Their mean
 * display value becomes the active channel's input black or white; a
 * colour channel receives the RGB composite's output. On the composite the
 * darkest (black) or brightest (white) channel decides, so the point turns
 * black or white without clipping past it. Null without a sample.
 */
export function pickedLevels(
  levels: Partial<Levels> | undefined,
  channel: LevelChannel,
  point: "black" | "white",
  pixels: [number, number, number][] | null,
): Levels | null {
  if (!pixels?.length) return null;
  const mean = [0, 1, 2].map(
    (c) => pixels.reduce((sum, p) => sum + toEncoded(p[c]) * 255, 0) / pixels.length,
  );
  const all = Object.fromEntries(
    LEVEL_CHANNELS.map((c) => [c, levelOf(levels, c)]),
  ) as Levels;
  let value: number;
  if (channel === "rgb") value = point === "black" ? Math.min(...mean) : Math.max(...mean);
  else value = levelOutput(all.rgb, mean[["red", "green", "blue"].indexOf(channel)]);
  return {
    ...all,
    [channel]: withLevelValue(all[channel], point === "black" ? "inBlack" : "inWhite", value),
  };
}

/**
 * Input levels that clip `fraction` of the pixels at each end, from a
 * 256-bin histogram of the current rendering. The histogram already shows
 * the current input levels, so its positions are mapped back through them.
 */
export function autoLevels(
  bins: number[][],
  current: Level,
  fraction = 0.0005,
): Pick<Level, "inBlack" | "inWhite"> | null {
  const size = Math.min(...bins.map((b) => b.length));
  if (!Number.isFinite(size) || size < 2) return null;
  const totals = bins.map((b) => b.reduce((sum, v) => sum + v, 0));
  if (totals.some((t) => t <= 0)) return null;
  const edge = (fromTop: boolean) => {
    // The channel that reaches furthest out decides (no channel clips more
    // than `fraction`).
    let best = fromTop ? 0 : size - 1;
    bins.forEach((b, c) => {
      let acc = 0;
      for (let k = 0; k < size; k++) {
        const i = fromTop ? size - 1 - k : k;
        acc += b[i];
        if (acc > totals[c] * fraction) {
          best = fromTop ? Math.max(best, i) : Math.min(best, i);
          break;
        }
      }
    });
    return (best / (size - 1)) * 255;
  };
  const back = (shown: number) => {
    const out =
      (shown - current.outBlack) /
      Math.max(1, current.outWhite - current.outBlack);
    return (
      current.inBlack +
      Math.min(1, Math.max(0, out)) * (current.inWhite - current.inBlack)
    );
  };
  const inBlack = Math.round(back(edge(false)));
  const inWhite = Math.round(back(edge(true)));
  if (inWhite - inBlack < 16) return null;
  return { inBlack, inWhite };
}
