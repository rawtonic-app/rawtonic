// SPDX-License-Identifier: AGPL-3.0-or-later

/** The body of CHANGELOG.md's `## <version> — <date>` section, or null. */
export function changelogSection(changelog: string, version: string): string | null {
  const lines = changelog.replace(/\r\n?/g, '\n').split('\n');
  const start = lines.findIndex((line) => {
    const m = /^##\s+v?(\S+)(\s|$)/.exec(line);
    return m !== null && m[1] === version;
  });
  if (start < 0) return null;
  const end = lines.findIndex((line, i) => i > start && /^##\s/.test(line));
  const body = lines.slice(start + 1, end < 0 ? undefined : end).join('\n').trim();
  return body || null;
}

type Parsed = { core: number[]; pre: string[] };
function parse(version: string): Parsed | null {
  const m = /^v?(\d+)\.(\d+)\.(\d+)(?:-([0-9A-Za-z.-]+))?(?:\+.*)?$/.exec(version.trim());
  return m ? { core: [+m[1], +m[2], +m[3]], pre: m[4] ? m[4].split('.') : [] } : null;
}

/** Semantic-version precedence (`0.1.0-alpha.7` < `0.1.0-alpha.7.1` <
 * `0.1.0-alpha.10` < `0.1.0`); false when either cannot be read. */
export function isNewerVersion(candidate: string, than: string): boolean {
  const a = parse(candidate);
  const b = parse(than);
  if (!a || !b) return false;
  for (let i = 0; i < 3; i++) if (a.core[i] !== b.core[i]) return a.core[i] > b.core[i];
  if (!a.pre.length || !b.pre.length) return !a.pre.length && b.pre.length > 0;
  for (let i = 0; i < Math.max(a.pre.length, b.pre.length); i++) {
    const x = a.pre[i];
    const y = b.pre[i];
    if (x === undefined || y === undefined) return y === undefined;
    if (x === y) continue;
    const xn = /^\d+$/.test(x);
    const yn = /^\d+$/.test(y);
    if (xn && yn) return +x > +y;
    if (xn !== yn) return yn;
    return x > y;
  }
  return false;
}
