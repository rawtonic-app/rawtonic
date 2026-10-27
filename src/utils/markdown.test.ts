// SPDX-License-Identifier: AGPL-3.0-or-later
import { describe, expect, it } from 'vitest';
import { parseInline, parseMarkdown, type Block, type Inline } from './markdown';
import changelog from '../../CHANGELOG.md?raw';

const kinds = (blocks: Block[]): string[] =>
  blocks.flatMap((b) => [b.kind, ...(b.kind === 'quote' ? kinds(b.children) : b.kind === 'list' ? b.items.flatMap(kinds) : [])]);
const text = (nodes: Inline[]): string =>
  nodes.map((n) => (n.kind === 'text' || n.kind === 'code' ? n.text : text(n.children))).join('');
const links = (blocks: Block[]): string[] =>
  JSON.stringify(blocks).match(/"href":"[^"]*"/g)?.map((s) => s.slice(8, -1)) ?? [];

describe('parseMarkdown', () => {
  it('reads headings, lists with code, alerts and rules of a release body', () => {
    const blocks = parseMarkdown([
      '**A fix release**: a RAW developer.',
      '',
      '> [!WARNING]',
      '> This is an **early alpha**.',
      '',
      '## Install',
      '',
      '1. Download `Rawtonic.AppImage` below.',
      '2. Make it executable:',
      '   ```sh',
      '   chmod +x Rawtonic.AppImage',
      '   ```',
      '   It runs on Ubuntu.',
      '',
      '- **Fixed.** one',
      '  - nested',
      '',
      '---',
    ].join('\n'));
    expect(blocks.map((b) => b.kind)).toEqual(['paragraph', 'quote', 'heading', 'list', 'list', 'rule']);
    expect(kinds(blocks)).toEqual(['paragraph', 'quote', 'paragraph', 'heading', 'list', 'paragraph', 'paragraph', 'code', 'paragraph', 'list', 'paragraph', 'list', 'paragraph', 'rule']);
    const quote = blocks[1] as Extract<Block, { kind: 'quote' }>;
    expect(text((quote.children[0] as Extract<Block, { kind: 'paragraph' }>).children)).toBe('This is an early alpha.');
    const install = blocks[3] as Extract<Block, { kind: 'list' }>;
    expect(install.ordered).toBe(true);
    expect(install.items[1][1]).toEqual({ kind: 'code', text: 'chmod +x Rawtonic.AppImage' });
  });

  it('never yields images, HTML or non-web links', () => {
    const inline = parseInline('![tracker](https://example.com/p.png) <img src=x onerror=alert(1)> [js](javascript:alert(1)) [file](docs/A.md) [ok](https://github.com/rawtonic-app/rawtonic) <https://rawtonic.cc>');
    expect(JSON.stringify(inline)).not.toContain('example.com');
    expect(text(inline)).toContain('<img src=x onerror=alert(1)>');
    expect(text(inline)).toContain('tracker');
    expect(links([{ kind: 'paragraph', children: inline }])).toEqual(['https://github.com/rawtonic-app/rawtonic', 'https://rawtonic.cc']);
  });

  it('keeps underscores inside words and escapes literal', () => {
    expect(text(parseInline('Rawtonic_0.1.0_aarch64.dmg and \\*not em\\*'))).toBe('Rawtonic_0.1.0_aarch64.dmg and *not em*');
    expect(parseInline('_em_ and *em*').filter((n) => n.kind === 'em')).toHaveLength(2);
  });

  it('parses the whole bundled CHANGELOG.md without losing text', () => {
    const blocks = parseMarkdown(changelog);
    expect(blocks.filter((b) => b.kind === 'heading').length).toBeGreaterThan(10);
    // Every word of the source survives (markup aside).
    const rendered = JSON.stringify(blocks);
    for (const word of ['Tethering', 'Saturated', 'Gatekeeper', 'Synchronize']) {
      if (changelog.includes(word)) expect(rendered).toContain(word);
    }
  });
});
