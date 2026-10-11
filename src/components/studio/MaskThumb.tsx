import type { LucideIcon } from "lucide-react";

/**
 * A mask's thumbnail in the layer list (white mask on black, the photo's
 * frame as the viewer shows it). Until it is made, the kind's icon stands in;
 * while an AI mask is computing the box pulses.
 */
export default function MaskThumb({
  url,
  icon: Icon,
  busy = false,
  kind,
}: {
  url?: string;
  icon?: LucideIcon | null;
  busy?: boolean;
  kind: "layer" | "component";
}) {
  return (
    <span
      className={`studio-mask-thumb is-${kind} ${busy ? "is-busy" : ""} ${url ? "has-image" : ""}`}
      aria-hidden="true"
      data-testid="mask-thumb"
    >
      {url ? (
        <img src={url} alt="" draggable={false} />
      ) : (
        Icon && <Icon size={kind === "layer" ? 13 : 10} aria-hidden="true" />
      )}
    </span>
  );
}
