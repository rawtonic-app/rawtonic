import type { TFunction } from "i18next";
import {
  Brush,
  Circle,
  Cloud,
  Eraser,
  Image,
  Minus,
  Mountain,
  Pipette,
  Square,
  SquareDashedMousePointer,
  Sun,
  User,
  Users,
  type LucideIcon,
} from "lucide-react";
import { OPTION_HEADER, OPTION_SEPARATOR, type Option } from "../ui/AppProperties";
import { Mask, type SubMask } from "../panel/right/Masks";
import { erasesFromLayer, kindOf, type MaskKind } from "../../utils/maskKinds";

/** Everything a layer or a component can be made from, in three groups as in
 * Lightroom's Create New Mask menu: drawn shapes, automatic selections and
 * ranges. Captions are `studio.develop.layers.groups.<group>`. */
export const MASK_KIND_GROUPS: { group: "draw" | "select" | "range"; kinds: MaskKind[] }[] = [
  { group: "draw", kinds: ["brush", "linear", "radial"] },
  { group: "select", kinds: ["subject", "sky", "background", "object", "people"] },
  { group: "range", kinds: ["color", "luminance", "depth"] },
];

/** One icon per kind, shared by the new-layer menu and grid, the layer rows
 * and the component rows (the drawn ones match the viewer's tool bar). */
export const MASK_KIND_ICONS: Record<MaskKind, LucideIcon> = {
  brush: Brush,
  linear: Minus,
  radial: Circle,
  subject: User,
  sky: Cloud,
  background: Image,
  object: SquareDashedMousePointer,
  people: Users,
  color: Pipette,
  luminance: Sun,
  depth: Mountain,
};

/** The icon of a component (the whole photo has none of the kinds; a brush
 * erasing from its layer shows the eraser of the viewer's tool bar). */
export function componentIcon(sub: SubMask | undefined, layerInverted = false): LucideIcon | null {
  if (!sub) return null;
  if (sub.type === Mask.All) return Square;
  if (erasesFromLayer(sub, layerInverted)) return Eraser;
  const kind = kindOf(sub);
  return kind ? MASK_KIND_ICONS[kind] : null;
}

/** The grouped kind menu (captions, icons, separators between groups) for
 * the new-layer "+" and for a layer's Add / Subtract / Intersect. */
export function maskKindMenu(
  t: TFunction,
  pick: (kind: MaskKind) => void,
  allowed: (kind: MaskKind) => boolean = () => true,
): Option[] {
  return MASK_KIND_GROUPS.flatMap(({ group, kinds }, index) => {
    const shown = kinds.filter(allowed);
    if (shown.length === 0) return [];
    return [
      ...(index > 0 ? [{ type: OPTION_SEPARATOR }] : []),
      { type: OPTION_HEADER, label: t(`studio.develop.layers.groups.${group}`) },
      ...shown.map((kind) => ({
        label: t(`studio.masks.kinds.${kind}`),
        icon: MASK_KIND_ICONS[kind],
        onClick: () => pick(kind),
      })),
    ];
  });
}
