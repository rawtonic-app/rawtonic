// SPDX-License-Identifier: AGPL-3.0-or-later
/** The Markdown of release notes and CHANGELOG.md, parsed into plain data that
 * React renders as text: HTML stays literal text, images become their alt
 * text (nothing is loaded) and only http(s) links survive. */

export type Inline =
  | { kind: 'text'; text: string }
  | { kind: 'code'; text: string }
  | { kind: 'strong' | 'em'; children: Inline[] }
  | { kind: 'link'; href: string; children: Inline[] };

export type Block =
  | { kind: 'heading'; level: number; children: Inline[] }
  | { kind: 'paragraph'; children: Inline[] }
  | { kind: 'code'; text: string }
  | { kind: 'quote'; children: Block[] }
  | { kind: 'list'; ordered: boolean; items: Block[][] }
  | { kind: 'rule' };

const LIST_ITEM = /^(\s*)([-*+]|\d+[.)])\s+(.*)$/;
const indentOf = (line: string) => line.length - line.trimStart().length;

export function parseMarkdown(source: string): Block[] {
  return parseBlocks(source.replace(/\r\n?/g, '\n').split('\n'));
}

function parseBlocks(lines: string[]): Block[] {
  const blocks: Block[] = [];
  let paragraph: string[] = [];
  const flush = () => {
    if (paragraph.length) blocks.push({ kind: 'paragraph', children: parseInline(paragraph.join(' ')) });
    paragraph = [];
  };
  let i = 0;
  while (i < lines.length) {
    const line = lines[i];
    const trimmed = line.trim();
    if (!trimmed) {
      flush();
      i++;
    } else if (/^(```|~~~)/.test(trimmed)) {
      flush();
      const fence = trimmed.slice(0, 3);
      const indent = indentOf(line);
      const code: string[] = [];
      for (i++; i < lines.length && !lines[i].trim().startsWith(fence); i++) {
        code.push(lines[i].slice(Math.min(indent, indentOf(lines[i]))));
      }
      blocks.push({ kind: 'code', text: code.join('\n') });
      i++;
    } else if (/^#{1,6}\s/.test(trimmed)) {
      flush();
      const [, hashes, text] = /^(#{1,6})\s+(.*?)\s*#*$/.exec(trimmed)!;
      blocks.push({ kind: 'heading', level: hashes.length, children: parseInline(text) });
      i++;
    } else if (/^([-*_])(\s*\1){2,}$/.test(trimmed)) {
      flush();
      blocks.push({ kind: 'rule' });
      i++;
    } else if (trimmed.startsWith('>')) {
      flush();
      const quoted: string[] = [];
      for (; i < lines.length && lines[i].trim().startsWith('>'); i++) {
        quoted.push(lines[i].trim().replace(/^>\s?/, ''));
      }
      // GitHub's alert marker ("[!WARNING]") is syntax, not text.
      if (/^\[!\w+\]$/.test(quoted[0]?.trim() ?? '')) quoted.shift();
      blocks.push({ kind: 'quote', children: parseBlocks(quoted) });
    } else if (LIST_ITEM.test(line) && (!paragraph.length || indentOf(line) < 4)) {
      flush();
      const [, indent, marker] = LIST_ITEM.exec(line)!;
      const ordered = /\d/.test(marker);
      const items: Block[][] = [];
      while (i < lines.length) {
        const item = LIST_ITEM.exec(lines[i]);
        if (!item || item[1].length !== indent.length || /\d/.test(item[2]) !== ordered) break;
        const body = [item[3]];
        const contentIndent = item[1].length + item[2].length + 1;
        // Continuation: indented lines and blank lines followed by indented ones.
        for (i++; i < lines.length; i++) {
          const next = lines[i];
          if (next.trim() && indentOf(next) <= indent.length) break;
          if (!next.trim() && !(lines[i + 1]?.trim() && indentOf(lines[i + 1]) > indent.length)) break;
          body.push(next.slice(Math.min(contentIndent, indentOf(next))));
        }
        items.push(parseBlocks(body));
      }
      blocks.push({ kind: 'list', ordered, items });
    } else {
      paragraph.push(trimmed);
      i++;
    }
  }
  flush();
  return blocks;
}

const SAFE_LINK = /^https?:\/\//i;

export function parseInline(text: string): Inline[] {
  const out: Inline[] = [];
  let plain = '';
  const push = (node: Inline) => {
    if (plain) out.push({ kind: 'text', text: plain });
    plain = '';
    out.push(node);
  };
  let i = 0;
  while (i < text.length) {
    const rest = text.slice(i);
    let m: RegExpExecArray | null;
    if (rest[0] === '\\' && /^\\[\\`*_[\]()#+\-.!<>]/.test(rest)) {
      plain += rest[1];
      i += 2;
    } else if ((m = /^(`+)([\s\S]*?[^`])\1(?!`)/.exec(rest))) {
      push({ kind: 'code', text: m[2].trim() });
      i += m[0].length;
    } else if ((m = /^!\[([^\]]*)\]\([^)]*\)/.exec(rest))) {
      // Images are never loaded: only their description is shown.
      plain += m[1];
      i += m[0].length;
    } else if ((m = /^\[((?:[^\]\\]|\\.)*)\]\(\s*<?([^)\s>]*)>?(?:\s+"[^"]*")?\s*\)/.exec(rest))) {
      const children = parseInline(m[1]);
      if (SAFE_LINK.test(m[2])) push({ kind: 'link', href: m[2], children });
      else children.forEach(push);
      i += m[0].length;
    } else if ((m = /^<(https?:\/\/[^>\s]+)>/i.exec(rest))) {
      push({ kind: 'link', href: m[1], children: [{ kind: 'text', text: m[1] }] });
      i += m[0].length;
    } else if ((m = /^(\*\*|__)(?=\S)([\s\S]*?\S)\1/.exec(rest))) {
      push({ kind: 'strong', children: parseInline(m[2]) });
      i += m[0].length;
    } else if ((m = /^(\*|_)(?=\S)([\s\S]*?\S)\1(?![\w*])/.exec(rest)) && !(m[1] === '_' && /\w/.test(text[i - 1] ?? ''))) {
      push({ kind: 'em', children: parseInline(m[2]) });
      i += m[0].length;
    } else {
      plain += rest[0];
      i++;
    }
  }
  if (plain) out.push({ kind: 'text', text: plain });
  return out;
}
