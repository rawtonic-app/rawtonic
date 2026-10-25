import { expect, it } from 'vitest';
import { flagTargets, libraryViewer, viewerOnly, viewerShown } from './viewerMode';

const ui = (activeView: string, libraryLoupe = false, isFullScreen = false) => ({ activeView, libraryLoupe, isFullScreen });

it('shows the native viewer in Develop, the loupe and the full screen, not in the grid', () => {
  expect(viewerShown(ui('editor'))).toBe(true);
  expect(viewerShown(ui('library', true))).toBe(true);
  expect(viewerShown(ui('library', false, true))).toBe(true);
  expect(viewerShown(ui('library'))).toBe(false);
  expect(libraryViewer(ui('editor', true, true))).toBe(false);
  expect(viewerOnly(ui('editor', false, true))).toBe(true);
  expect(viewerOnly(ui('editor', true))).toBe(false);
});

it('flags the photograph shown in the loupe and the full screen, the selection in the grid', () => {
  const selection = ['/a', '/b', '/c'];
  expect(flagTargets(ui('library', true), '/b', selection)).toEqual(['/b']);
  expect(flagTargets(ui('library', false, true), '/b', selection)).toEqual(['/b']);
  expect(flagTargets(ui('editor'), '/b', selection)).toEqual(['/b']);
  expect(flagTargets(ui('library'), '/b', selection)).toEqual(selection);
});
