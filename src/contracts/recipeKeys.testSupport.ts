import geometry from '../../crates/recipe/src/geometry.rs?raw';

const names = new Map(
  [...geometry.matchAll(/pub const (\w+):\s*Key<[\s\S]*?>\s*=\s*Key::new\(\s*"([^"]+)"/g)].map((match) => [
    match[1],
    match[2]
  ])
);

/** Read a native key list, resolving geometry's typed constants to stored names.
 * Reject unknown expressions so a registry move cannot silently shrink a contract. */
export function rustRecipeKeys(body: string): string[] {
  return body
    .split(',')
    .map((item) => item.trim())
    .filter(Boolean)
    .map((item) => {
      const literal = item.match(/^"([^"]+)"$/);
      if (literal) return literal[1];
      const key = item.match(/^geometry::(\w+)\.(?:name|into_name)\(\)$/)?.[1];
      const name = key && names.get(key);
      if (!name) throw new Error(`Unresolved native recipe key: ${item}`);
      return name;
    });
}
