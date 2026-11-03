/**
 * Rawtonic's colour labels as the editor names them: lower case, in the
 * editor state's `color:<name>` tag. The catalog and XMP store the same
 * label capitalised (`ColorLabel` in `crates/metadata/src/labels.rs`).
 */
export const COLOR_LABEL_NAMES = ['red', 'yellow', 'green', 'blue', 'purple'] as const;

export type ColorLabelName = (typeof COLOR_LABEL_NAMES)[number];

export function isColorLabelName(name: string): name is ColorLabelName {
  return (COLOR_LABEL_NAMES as readonly string[]).includes(name);
}
