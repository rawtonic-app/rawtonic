import geometry from '../../crates/recipe/src/geometry.rs?raw';
import lens from '../../crates/recipe/src/lens.rs?raw';

const names = new Map(
  [...geometry.matchAll(/pub const (\w+):\s*Key<[\s\S]*?>\s*=\s*Key::new\(\s*"([^"]+)"/g)].map((match) => [
    match[1],
    match[2]
  ])
);
const lensNames = new Map(
  [...lens.matchAll(/pub const (\w+):\s*Key<[\s\S]*?>\s*=\s*Key::new\(\s*"([^"]+)"/g)].map((match) => [
    match[1],
    match[2]
  ])
);

/** Read a native key list, resolving the geometry and lens typed constants to stored names.
 * Reject unknown expressions so a registry move cannot silently shrink a contract. */
export function rustRecipeKeys(body: string): string[] {
  return body
    .split(',')
    .map((item) => item.trim())
    .filter(Boolean)
    .map((item) => {
      const literal = item.match(/^"([^"]+)"$/);
      if (literal) return literal[1];
      const key = item.match(/^(geometry|lens)::(\w+)\.(?:name|into_name)\(\)$/);
      const name = key && (key[1] === 'lens' ? lensNames : names).get(key[2]);
      if (!name) throw new Error(`Unresolved native recipe key: ${item}`);
      return name;
    });
}
