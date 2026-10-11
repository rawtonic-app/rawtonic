import { describe, expect, it } from 'vitest';
import {
  SHOWN_LAYOUT,
  presentGroups,
  readModuleLayout,
  requestGroups,
  setPanelGroups,
  togglePanelGroups,
} from './panelLayout';

describe('panel layout', () => {
  it('hides a group while any of it shows and brings it back once all of it is hidden', () => {
    const sides = ['left', 'right'] as const;
    const hidden = togglePanelGroups(SHOWN_LAYOUT, [...sides]);
    expect(hidden).toEqual({ ...SHOWN_LAYOUT, left: false, right: false });
    expect(togglePanelGroups(hidden, [...sides])).toEqual(SHOWN_LAYOUT);
    // One side already hidden (F7): Tab hides the other one too.
    const leftHidden = togglePanelGroups(SHOWN_LAYOUT, ['left']);
    expect(togglePanelGroups(leftHidden, [...sides])).toEqual(hidden);
  });

  it('maps Tab, Shift+Tab and the F keys to the parts a module has', () => {
    const library = presentGroups('library', 'tools-left');
    expect(requestGroups('sides', library)).toEqual(['left']);
    expect(requestGroups('all', library)).toEqual(['left', 'bars']);
    expect(requestGroups('right', library)).toEqual([]);
    expect(requestGroups('filmstrip', library)).toEqual([]);
    const develop = presentGroups('develop', 'tools-left');
    expect(requestGroups('all', develop)).toEqual(['left', 'filmstrip', 'bars']);
    const split = presentGroups('develop', 'split-tools');
    expect(requestGroups('sides', split)).toEqual(['left', 'right']);
    expect(requestGroups('right', split)).toEqual(['right']);
    // Below 1200 px the Library tools beside the right-hand tools are not shown.
    expect(requestGroups('sides', presentGroups('develop', 'split-tools', true))).toEqual(['right']);
  });

  it('shows every part a stored layout does not name or cannot be read', () => {
    expect(readModuleLayout(undefined, 'develop')).toEqual(SHOWN_LAYOUT);
    expect(readModuleLayout({ develop: 'broken' }, 'develop')).toEqual(SHOWN_LAYOUT);
    expect(readModuleLayout({ develop: { left: false, bars: 'no' } }, 'develop')).toEqual({
      ...SHOWN_LAYOUT,
      left: false,
    });
    expect(readModuleLayout({ develop: { left: false } }, 'library')).toEqual(SHOWN_LAYOUT);
  });

  it('shows parts without hiding the others (window-edge strips)', () => {
    const all = setPanelGroups(SHOWN_LAYOUT, ['left', 'right', 'filmstrip', 'bars'], false);
    expect(setPanelGroups(all, ['bars', 'filmstrip'], true)).toEqual({
      left: false,
      right: false,
      filmstrip: true,
      bars: true,
    });
  });
});
