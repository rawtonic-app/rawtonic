import { useId } from "react";
import type { RelightLight } from "../../utils/relight";

/** Own small schematic swatches, not a second photograph renderer. */
export default function RelightPatternPreview({
  pattern,
  image,
}: {
  pattern: RelightLight["pattern"];
  /** An imported pattern's bitmap (absent once only its name was sent). */
  image?: string | null;
}) {
  const id = useId();
  const lines = [4, 12, 20, 28];
  if (pattern === "custom")
    return image ? (
      <img src={image} alt="" className="relight-pattern-preview" />
    ) : (
      <svg viewBox="0 0 40 32" aria-hidden="true" className="relight-pattern-preview" />
    );
  return (
    <svg
      viewBox="0 0 40 32"
      aria-hidden="true"
      className="relight-pattern-preview"
    >
      <defs>
        <pattern id={id} width="10" height="10" patternUnits="userSpaceOnUse">
          <rect width="5" height="5" fill="currentColor" />
          <rect x="5" y="5" width="5" height="5" fill="currentColor" />
        </pattern>
      </defs>
      {pattern === "none" && (
        <rect width="40" height="32" rx="2" fill="currentColor" />
      )}
      {pattern === "window" && (
        <path
          fill="currentColor"
          d="M4 2h14v12H4zM22 2h14v12H22zM4 18h14v12H4zM22 18h14v12H22z"
        />
      )}
      {pattern === "strip" && (
        <path fill="currentColor" d="M0 28L32 0H40L8 32H0z" />
      )}
      {["blinds", "horizontal", "weave"].includes(pattern) &&
        lines.map((y) => (
          <path
            key={y}
            d={`M0 ${y}H40`}
            stroke="currentColor"
            strokeWidth={pattern === "blinds" ? 4 : 2}
          />
        ))}
      {["diagonal", "crosshatch"].includes(pattern) &&
        [-30, -15, 0, 15, 30].map((x) => (
          <path
            key={x}
            d={`M${x} 32l32 -32${pattern === "crosshatch" ? `M${x} 0l32 32` : ""}`}
            stroke="currentColor"
            strokeWidth="2"
          />
        ))}
      {["grid", "weave"].includes(pattern) && (
        <path
          d="M10 0v32M20 0v32M30 0v32M0 8h40M0 16h40M0 24h40"
          stroke="currentColor"
          strokeWidth="2"
        />
      )}
      {pattern === "checkerboard" && (
        <rect width="40" height="32" fill={`url(#${id})`} />
      )}
      {pattern === "dots" &&
        [6, 16, 26, 36].flatMap((x) =>
          [6, 16, 26].map((y) => (
            <circle key={`${x}-${y}`} cx={x} cy={y} r="3" fill="currentColor" />
          )),
        )}
      {pattern === "branches" && (
        <path
          d="M17 32L23 0M20 20L4 8M21 16L36 4M18 27L35 15M22 8L13 0"
          stroke="currentColor"
          strokeWidth="2"
        />
      )}
      {pattern === "palm" && (
        <path
          d="M14 32Q27 20 22 0M20 23L3 18M23 17L6 10M23 9L11 1M20 24L37 24M23 17L39 15M24 9L35 4"
          fill="none"
          stroke="currentColor"
          strokeWidth="3"
        />
      )}
      {["foliage", "noise"].includes(pattern) &&
        Array.from({ length: pattern === "noise" ? 48 : 18 }, (_, i) => (
          <ellipse
            key={i}
            cx={(i * 17 + 3) % 40}
            cy={(i * 13 + 5) % 32}
            rx={pattern === "noise" ? 1.4 : 4}
            ry={pattern === "noise" ? 1 : 2.5}
            transform={`rotate(${i * 37} ${(i * 17 + 3) % 40} ${(i * 13 + 5) % 32})`}
            fill="currentColor"
          />
        ))}
    </svg>
  );
}
