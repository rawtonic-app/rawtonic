// SPDX-License-Identifier: AGPL-3.0-or-later
/**
 * Hierarchical keywords (G36/G37). A keyword is stored as its path from the
 * top, levels joined by `|` (`Animals|Dogs`): XMP's `lr:hierarchicalSubject`
 * form, which Lightroom, Capture One and Bridge read and write, and the form
 * the catalog, sidecars and exports keep (`rawtonic_metadata::keywords`).
 *
 * Typing accepts the three forms photographers know: `Animals > Dogs`
 * (Lightroom's display form), `Animals|Dogs` (Capture One's) and
 * `Dogs < Animals` (Lightroom's entry shortcut: the child first). A keyword
 * stored flat before hierarchies, holding `>` or `<` (`places > asia`), stays
 * as it is; the keyword list offers it for conversion. A level in double
 * quotes is one name whatever it holds (`Parent > "comma,term"`,
 * `"a > b"`): commas outside quotes separate keywords, `>`, `|` and `<`
 * outside quotes separate levels, `;` and `/` are always part of a name.
 * Autocomplete offers keywords whose names hold `,`, `>` or `<` in that
 * form (`typedKeyword`), so choosing one applies it as it is.
 */

export const KEYWORD_SEPARATOR = '|';
/** How a path is shown: Lightroom's display form. */
export const DISPLAY_SEPARATOR = ' > ';

/** The levels of a stored keyword, trimmed, without empty ones. */
export function keywordLevels(keyword: string): string[] {
  return keyword
    .split(KEYWORD_SEPARATOR)
    .map((level) => level.trim())
    .filter(Boolean);
}

/** A stored keyword's normal form (`Animals | Dogs` → `Animals|Dogs`). */
export const normalizeKeyword = (keyword: string) => keywordLevels(keyword).join(KEYWORD_SEPARATOR);

/** A stored keyword as shown: `Animals > Dogs`. */
export const displayKeyword = (keyword: string) => keywordLevels(keyword).join(DISPLAY_SEPARATOR) || keyword;

/** A keyword's own name, its last level. */
export const keywordLeaf = (keyword: string) => keywordLevels(keyword).at(-1) ?? keyword;

/** The path of the keyword's parent, or null at the top. */
export function keywordParent(keyword: string): string | null {
  const levels = keywordLevels(keyword);
  return levels.length > 1 ? levels.slice(0, -1).join(KEYWORD_SEPARATOR) : null;
}

/** `text` cut at each character of `separators` outside double quotes. */
function splitOutsideQuotes(text: string, separators: string): string[] {
  const out: string[] = [];
  let piece = '';
  let quoted = false;
  for (const char of text) {
    if (char === '"') quoted = !quoted;
    if (!quoted && separators.includes(char)) {
      out.push(piece);
      piece = '';
    } else piece += char;
  }
  out.push(piece);
  return out;
}

/** A typed level, trimmed; a level in double quotes is its content. */
function typedLevel(text: string): string {
  const level = text.trim();
  return level.length >= 2 && level.startsWith('"') && level.endsWith('"') ? level.slice(1, -1).trim() : level;
}

/** The levels of one typed keyword in any of the three entry forms, the top
 * first; empty when nothing is left. A `|` or `>` makes the levels top
 * first (both may be mixed); `<` alone names the child first; a level in
 * double quotes is taken as it is. */
export function parseKeywordPath(text: string): string[] {
  const split = (separators: string) => splitOutsideQuotes(text, separators).map(typedLevel).filter(Boolean);
  const outside = text.replace(/"[^"]*"/g, '');
  if (/[|>]/.test(outside)) return split('|>');
  if (outside.includes('<')) return split('<').reverse();
  const level = typedLevel(text);
  return level ? [level] : [];
}

/** A stored keyword as typed to get it back: its shown form with each level
 * holding `,`, `>`, `<` or `"` in double quotes (`Parent > "comma,term"`),
 * so autocomplete tells it apart from the hierarchy those would make. */
export function typedKeyword(keyword: string): string {
  return keywordLevels(keyword)
    .map((level) => (/[,<>"]/.test(level) ? `"${level}"` : level))
    .join(DISPLAY_SEPARATOR);
}

/** Keywords typed into the field: comma-separated, each parsed as a path,
 * without duplicates (ignoring case). */
export function parseKeywordEntries(text: string): string[][] {
  const out: string[][] = [];
  const seen = new Set<string>();
  for (const part of splitOutsideQuotes(text, ',')) {
    const levels = parseKeywordPath(part);
    if (!levels.length) continue;
    const key = fold(levels.join(KEYWORD_SEPARATOR));
    if (seen.has(key)) continue;
    seen.add(key);
    out.push(levels);
  }
  return out;
}

const fold = (text: string) => text.toLowerCase();

/** Whether `keyword` is `path` or under it, level by level, ignoring case
 * (as the catalog's keyword rule: `rawtonic_metadata::keywords::is_within`). */
export function isWithinKeyword(keyword: string, path: string): boolean {
  const own = keywordLevels(keyword);
  const levels = keywordLevels(path);
  if (!levels.length || own.length < levels.length) return false;
  return levels.every((level, i) => fold(level) === fold(own[i]));
}

/** `keyword` with its part `from` replaced by `to` (a rename, merge or move
 * of `from`); itself when it is not within `from`. */
export function renameKeyword(keyword: string, from: string, to: string): string {
  if (!isWithinKeyword(keyword, from)) return keyword;
  const rest = keywordLevels(keyword).slice(keywordLevels(from).length);
  return [...keywordLevels(to), ...rest].join(KEYWORD_SEPARATOR);
}

/**
 * The stored keyword for typed `levels`. A single name is a top-level
 * keyword in the field's form for typed keywords (trimmed, lowercase; the
 * exact string is compared, as before hierarchies) unless it is no
 * top-level keyword but the name of exactly one keyword in the tree
 * (`dogs` → `Animals|Dogs`): that keyword, as Lightroom applies a typed name
 * it finds in its list. A path is spelled as the library spells it: each
 * level naming an existing keyword at that place takes its spelling
 * (ignoring case), new levels are lowercased.
 */
export function resolveKeyword(levels: string[], existing: readonly string[]): string {
  const known = new Map<string, string>();
  const leaves = new Map<string, Set<string>>();
  for (const keyword of existing) {
    const parts = keywordLevels(keyword);
    for (let i = 1; i <= parts.length; i++) {
      const path = parts.slice(0, i).join(KEYWORD_SEPARATOR);
      if (!known.has(fold(path))) known.set(fold(path), path);
      const leaf = fold(parts[i - 1]);
      const set = leaves.get(leaf) ?? new Set<string>();
      set.add(known.get(fold(path))!);
      leaves.set(leaf, set);
    }
  }
  if (levels.length === 1) {
    const named = known.has(fold(levels[0])) ? undefined : leaves.get(fold(levels[0]));
    return named?.size === 1 ? [...named][0] : levels[0].toLowerCase();
  }
  return spelledPath(levels, known);
}

/** The path of `levels` as the library spells it: each level naming an
 * existing keyword at that place takes its spelling (ignoring case), new
 * levels are lowercased. A single name is never taken for a keyword deeper
 * in the tree (New Keyword makes what was typed, where it was asked). */
export function resolveKeywordPath(levels: string[], existing: readonly string[]): string {
  const known = new Map<string, string>();
  for (const keyword of existing) {
    const parts = keywordLevels(keyword);
    for (let i = 1; i <= parts.length; i++) {
      const path = parts.slice(0, i).join(KEYWORD_SEPARATOR);
      if (!known.has(fold(path))) known.set(fold(path), path);
    }
  }
  return spelledPath(levels, known);
}

function spelledPath(levels: string[], known: Map<string, string>): string {
  const out: string[] = [];
  for (const level of levels) {
    const path = [...out, level].join(KEYWORD_SEPARATOR);
    const spelled = known.get(fold(path));
    out.push(spelled ? keywordLeaf(spelled) : level.toLowerCase());
  }
  return out.join(KEYWORD_SEPARATOR);
}

/**
 * The stored keywords for the field's text: each comma-separated entry
 * resolved (`resolveKeyword`), without duplicates. An entry that is exactly
 * a keyword stored flat before hierarchies (`places > asia`) or a keyword as
 * `typedKeyword` writes it (`Parent > "comma,term"`, as autocomplete offers
 * it) is that keyword, so applying it again keeps its identity; the Keyword
 * List offers a flat one's conversion.
 */
export function resolveKeywordEntries(text: string, existing: readonly string[]): string[] {
  const legacy = new Map<string, string>();
  for (const keyword of existing) {
    const stored = keyword.trim();
    if (legacyHierarchy(stored)) legacy.set(fold(stored), stored);
    // As autocomplete offers a keyword whose names need quotes: that keyword.
    const typed = typedKeyword(stored);
    if (typed !== displayKeyword(stored)) legacy.set(fold(typed), stored);
  }
  const out: string[] = [];
  for (const part of splitOutsideQuotes(text, ',')) {
    const kept = legacy.get(fold(part.trim()));
    const levels = kept ? null : parseKeywordPath(part);
    const keyword = kept ?? (levels!.length ? resolveKeyword(levels!, existing) : '');
    if (keyword && !out.some((k) => fold(k) === fold(keyword))) out.push(keyword);
  }
  return out;
}

/** A keyword stored flat before hierarchies that reads as one (`places >
 * asia`, `japan < asia`): its stored path, or null. */
export function legacyHierarchy(keyword: string): string | null {
  if (keyword.includes(KEYWORD_SEPARATOR) || !/[<>]/.test(keyword)) return null;
  const levels = parseKeywordPath(keyword);
  return levels.length > 1 ? levels.join(KEYWORD_SEPARATOR) : null;
}

/** Where `path` goes when dropped onto `parent` (null: the top level); null
 * when it cannot (onto itself, its own parent, or below itself). */
export function movedKeyword(path: string, parent: string | null): string | null {
  const leaf = keywordLeaf(path);
  if (parent === null) return keywordParent(path) === null ? null : leaf;
  if (isWithinKeyword(parent, path)) return null;
  const current = keywordParent(path);
  if (current !== null && fold(current) === fold(normalizeKeyword(parent))) return null;
  return [...keywordLevels(parent), leaf].join(KEYWORD_SEPARATOR);
}

export interface KeywordNode {
  /** The stored path (`Animals|Dogs`). */
  path: string;
  name: string;
  /** Photographs with this keyword or one under it. */
  count: number;
  children: KeywordNode[];
}

/**
 * The keyword tree from counted paths (the catalog's keyword list counts
 * every parent too, each photograph once); a parent missing from `values`
 * gets the largest of its children's counts (the least it holds). Siblings
 * in name order, as Lightroom's Keyword List.
 */
export function buildKeywordTree(values: readonly { value: string; count: number }[]): KeywordNode[] {
  const nodes = new Map<string, KeywordNode>();
  const roots: KeywordNode[] = [];
  const node = (path: string): KeywordNode => {
    const key = fold(path);
    let found = nodes.get(key);
    if (found) return found;
    found = { path, name: keywordLeaf(path), count: 0, children: [] };
    nodes.set(key, found);
    const parent = keywordParent(path);
    if (parent === null) roots.push(found);
    else node(parent).children.push(found);
    return found;
  };
  for (const { value, count } of values) {
    const path = normalizeKeyword(value);
    if (!path) continue;
    const entry = node(path);
    entry.count = Math.max(entry.count, count);
  }
  const settle = (list: KeywordNode[]) => {
    list.sort((a, b) => a.name.localeCompare(b.name) || a.path.localeCompare(b.path));
    for (const entry of list) {
      settle(entry.children);
      for (const child of entry.children) entry.count = Math.max(entry.count, child.count);
    }
  };
  settle(roots);
  return roots;
}

/** `values` (counted keyword paths) with the keywords made without
 * photographs and their parents added with a count of 0, unless the library
 * counts them already. */
export function withDeclaredKeywords<T extends { value: string; count: number }>(
  values: readonly T[],
  declared: readonly string[]
): (T | { value: string; count: number })[] {
  const known = new Set(values.map((entry) => fold(normalizeKeyword(entry.value))));
  const out: (T | { value: string; count: number })[] = [...values];
  for (const keyword of declared) {
    const levels = keywordLevels(keyword);
    for (let i = 1; i <= levels.length; i++) {
      const path = levels.slice(0, i).join(KEYWORD_SEPARATOR);
      if (known.has(fold(path))) continue;
      known.add(fold(path));
      out.push({ value: path, count: 0 });
    }
  }
  return out;
}

/** Each path and its parents counted once per photograph, from the tags of
 * the listed photographs (outside a library, where no catalog counts). */
export function countKeywordPaths(keywordsByPhoto: readonly (readonly string[])[]) {
  const counts = new Map<string, { value: string; count: number }>();
  for (const keywords of keywordsByPhoto) {
    const seen = new Set<string>();
    for (const keyword of keywords) {
      const levels = keywordLevels(keyword);
      for (let i = 1; i <= levels.length; i++) {
        const path = levels.slice(0, i).join(KEYWORD_SEPARATOR);
        const key = fold(path);
        if (seen.has(key)) continue;
        seen.add(key);
        const entry = counts.get(key) ?? { value: path, count: 0 };
        entry.count++;
        counts.set(key, entry);
      }
    }
  }
  return [...counts.values()];
}

/** The rows of a keyword value list in tree order (a parent, then the
 * keywords under it) with their depth; `""` (no keyword) last. */
export function treeRows<T extends { value: string }>(values: readonly T[]): { row: T; depth: number }[] {
  const byPath = new Map(values.map((v) => [fold(normalizeKeyword(v.value)), v]));
  const tree = buildKeywordTree(values.filter((v) => v.value !== '').map((v) => ({ value: v.value, count: 0 })));
  const out: { row: T; depth: number }[] = [];
  const walk = (list: KeywordNode[], depth: number) => {
    for (const entry of list) {
      const row = byPath.get(fold(entry.path));
      if (row) out.push({ row, depth });
      walk(entry.children, row ? depth + 1 : depth);
    }
  };
  walk(tree, 0);
  for (const v of values) if (v.value === '') out.push({ row: v, depth: 0 });
  return out;
}

const USER_PREFIX = 'user:';
const COLOR_PREFIX = 'color:';
const tagKeyword = (tag: string) =>
  tag.startsWith(COLOR_PREFIX) ? null : tag.startsWith(USER_PREFIX) ? tag.slice(USER_PREFIX.length) : tag;

/**
 * A photograph's tags after renaming the keyword `from` (and those under
 * it) to `to`, as the backend writes them (`rename_keyword_tags` in
 * src-tauri/src/tagging.rs): a renamed keyword the photograph has already,
 * ignoring case, is kept once in its spelling (a merge); the user's marker
 * wins over an automatic tag. Not sorted.
 */
export function renameKeywordTags(tags: readonly string[], from: string, to: string): string[] {
  return replaceKeywordTags(tags, (keyword) =>
    isWithinKeyword(keyword, from) ? renameKeyword(keyword, from, to) : undefined
  );
}

/** What `keyword` becomes when the keyword `path` is deleted: null for
 * `path` itself, one level up for a keyword under it (Lightroom keeps a
 * deleted keyword's children; `rawtonic_metadata::keywords::without`), else
 * itself. */
export function keywordWithout(keyword: string, path: string): string | null {
  if (!isWithinKeyword(keyword, path)) return keyword;
  const levels = keywordLevels(keyword);
  const own = keywordLevels(path).length;
  if (levels.length === own) return null;
  return levels.filter((_, at) => at !== own - 1).join(KEYWORD_SEPARATOR);
}

/** A photograph's tags after deleting the keyword `path`, as the backend
 * writes them (`delete_keyword_tags` in src-tauri/src/tagging.rs). Not
 * sorted. */
export function deleteKeywordTags(tags: readonly string[], path: string): string[] {
  return replaceKeywordTags(tags, (keyword) =>
    isWithinKeyword(keyword, path) ? keywordWithout(keyword, path) : undefined
  );
}

/** Tags with their keywords replaced: `replace` says undefined to keep a
 * keyword, null to drop it or its new path. */
function replaceKeywordTags(
  tags: readonly string[],
  replace: (keyword: string) => string | null | undefined
): string[] {
  const kept: string[] = [];
  const renamed: string[] = [];
  for (const tag of tags) {
    const keyword = tagKeyword(tag);
    const replaced = keyword === null ? undefined : replace(keyword);
    if (replaced === undefined) kept.push(tag);
    else if (replaced !== null) renamed.push(`${tag.startsWith(USER_PREFIX) ? USER_PREFIX : ''}${replaced}`);
  }
  for (const tag of renamed) {
    const keyword = fold(tagKeyword(tag)!);
    const at = kept.findIndex((t) => {
      const k = tagKeyword(t);
      return k !== null && fold(k) === keyword;
    });
    if (at < 0) kept.push(tag);
    else if (tag.startsWith(USER_PREFIX) && !kept[at].startsWith(USER_PREFIX)) kept[at] = `${USER_PREFIX}${kept[at]}`;
  }
  return kept;
}

/** Whether any of `tags` names a keyword within `path`. */
export const tagsHaveKeyword = (tags: readonly string[] | null | undefined, path: string) =>
  (tags ?? []).some((tag) => {
    const keyword = tagKeyword(tag);
    return keyword !== null && isWithinKeyword(keyword, path);
  });
