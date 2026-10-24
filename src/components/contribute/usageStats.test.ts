import '../../i18n';
import i18next from 'i18next';
import { beforeEach, describe, expect, it, vi } from 'vitest';
import { invoke } from '@tauri-apps/api/core';
import relaySchema from '../../../services/relay/src/schema.ts?raw';
import { COPYABLE_ADJUSTMENT_KEYS, INITIAL_ADJUSTMENTS, NON_COPYABLE_ADJUSTMENT_KEYS } from '../../utils/adjustments';
import { useSettingsStore } from '../../store/useSettingsStore';
import type { AppSettings } from '../ui/AppProperties';
import { KEY_TOOLS, STATS_TOOLS, editedTools, noteEdit, noteTool, plainLines } from './usageStats';

vi.mock('@tauri-apps/api/core', () => ({ invoke: vi.fn(() => Promise.resolve()) }));

const statsOn = (on: boolean) => useSettingsStore.setState({ appSettings: { usageStats: on } as AppSettings });

beforeEach(() => vi.mocked(invoke).mockClear());

describe('usage statistics', () => {
  it("counts the relay's tools, and only recipe keys that exist", () => {
    const relay = /export const TOOLS = \[([^\]]*)\]/.exec(relaySchema)?.[1].match(/"(\w+)"/g)?.map((k) => k.slice(1, -1));
    expect(relay).toEqual([...STATS_TOOLS]);
    const recipeKeys = [...COPYABLE_ADJUSTMENT_KEYS, ...NON_COPYABLE_ADJUSTMENT_KEYS];
    for (const key of Object.keys(KEY_TOOLS)) expect(recipeKeys).toContain(key);
  });

  it('names the tools an edit step changed, versions and other tools aside', () => {
    const before = { ...INITIAL_ADJUSTMENTS };
    expect(editedTools(before, { ...before })).toEqual([]);
    expect(editedTools(before, { ...before, exposure: 1, sharpenVersion: 6 } as never)).toEqual(['basic']);
    expect(editedTools(before, { ...before, grainAmount: 20 } as never)).toEqual([]);
    expect(
      editedTools(before, {
        ...before,
        masks: [{ subMasks: [{ type: 'ai-sky' }, { type: 'brush' }] }, { subMasks: [{ type: 'heal' }] }],
      } as never).sort(),
    ).toEqual(['aiMasks', 'masks', 'retouch']);
    // A layer left as it was does not count: editing a brush layer beside
    // an AI layer is a masks edit only.
    const ai = { id: 'a', subMasks: [{ type: 'ai-subject' }] };
    const brush = { id: 'b', subMasks: [{ type: 'brush' }], amount: 1 };
    expect(
      editedTools({ ...before, masks: [ai, brush] } as never, { ...before, masks: [ai, { ...brush, amount: 2 }] } as never),
    ).toEqual(['masks']);
    expect(editedTools({ ...before, masks: [ai, brush] } as never, { ...before, masks: [ai] } as never)).toEqual(['masks']);
    expect(editedTools(before, { ...before, crop: { x: 0, y: 0, width: 10, height: 10 }, redEyes: [{}] } as never).sort()).toEqual([
      'crop',
      'redEye',
    ]);
  });

  it('notes nothing while statistics are off', () => {
    statsOn(false);
    noteTool('export');
    noteEdit(INITIAL_ADJUSTMENTS, { ...INITIAL_ADJUSTMENTS, exposure: 1 });
    expect(invoke).not.toHaveBeenCalled();
    statsOn(true);
    noteTool('export');
    noteEdit(INITIAL_ADJUSTMENTS, { ...INITIAL_ADJUSTMENTS, exposure: 1 });
    expect(vi.mocked(invoke).mock.calls).toEqual([
      ['usage_stats_note_tools', { tools: ['export'] }],
      ['usage_stats_note_tools', { tools: ['basic'] }],
    ]);
  });

  it('says a body in plain words', () => {
    const body = JSON.stringify({
      schema: 1,
      week: '2026-W43',
      app: { version: '0.1.0-alpha.4', os: 'macos', osVersion: '27.0', arch: 'aarch64' },
      gpuVendor: 'apple',
      cameras: [{ make: 'NIKON CORPORATION', model: 'NIKON Z 6' }],
      tools: { curves: true, masks: true },
    });
    const t = i18next.t;
    expect(plainLines(body, t, (tool) => t(`contribute.stats.tools.${tool}`, { defaultValue: tool }))).toEqual([
      'Week 2026-W43',
      'Rawtonic 0.1.0-alpha.4 · macOS 27.0 · aarch64 · Apple graphics',
      'Cameras opened: NIKON CORPORATION NIKON Z 6',
      'Tools used: Curves, Masks',
    ]);
  });
});
