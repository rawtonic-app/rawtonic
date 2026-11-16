import { afterEach, expect, it } from 'vitest';
import { bracketStep } from './bracketKeys';
import { useSettingsStore } from '../store/useSettingsStore';

const key = (init: KeyboardEventInit) => new KeyboardEvent('keydown', init);

afterEach(() => useSettingsStore.setState({ appSettings: null }));

it('steps for the typed brackets and for the keys bound to Rotate', () => {
  expect(bracketStep(key({ key: '[', code: 'BracketLeft' }))).toBe(-1);
  expect(bracketStep(key({ key: ']', code: 'Digit6', altKey: true }))).toBe(1);
  // Swiss German: the physical [ ] keys type ü and ¨ and carry Rotate.
  expect(bracketStep(key({ key: 'ü', code: 'BracketLeft' }))).toBe(-1);
  expect(bracketStep(key({ key: '¨', code: 'BracketRight' }))).toBe(1);
  expect(bracketStep(key({ key: 'è', code: 'BracketLeft', shiftKey: true }))).toBe(-1);
  // ⌘ keys and other keys are not sizes.
  expect(bracketStep(key({ key: 'ü', code: 'BracketLeft', metaKey: true }))).toBeUndefined();
  expect(bracketStep(key({ key: 'r', code: 'KeyR' }))).toBeUndefined();
});

it("follows the user's Rotate keys", () => {
  useSettingsStore.setState({
    appSettings: { keybinds: { rotate_left: ['KeyQ'] } } as never
  });
  expect(bracketStep(key({ key: 'q', code: 'KeyQ' }))).toBe(-1);
  expect(bracketStep(key({ key: 'ü', code: 'BracketLeft' }))).toBeUndefined();
});
