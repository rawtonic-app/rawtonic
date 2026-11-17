import { afterEach, expect, it } from 'vitest';
import { bracketKey, bracketKeyLabels, bracketStep } from './bracketKeys';
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
  // The bracket place itself still sizes while nothing is bound there.
  expect(bracketStep(key({ key: 'ü', code: 'BracketLeft' }))).toBe(-1);
});

it('sizes with the bracket places on German and Swiss layouts unless the key there has another binding (WBRS1)', () => {
  useSettingsStore.setState({
    appSettings: { keybinds: { rotate_left: ['KeyQ'], rotate_right: ['KeyW'] } } as never
  });
  // de-DE: ü and + at the US [ ] places; de-CH: ü and ¨.
  expect(bracketStep(key({ key: 'ü', code: 'BracketLeft' }))).toBe(-1);
  expect(bracketStep(key({ key: '+', code: 'BracketRight' }))).toBe(1);
  expect(bracketStep(key({ key: '¨', code: 'BracketRight' }))).toBe(1);
  expect(bracketKey(key({ key: '*', code: 'BracketRight', shiftKey: true }))).toEqual({ step: 1, shifted: true });
  // Bound to another command: that command keeps the key.
  useSettingsStore.setState({
    appSettings: { keybinds: { toggle_crop: ['BracketLeft'], rotate_left: ['KeyQ'] } } as never
  });
  expect(bracketStep(key({ key: 'ü', code: 'BracketLeft' }))).toBeUndefined();
});

it('names the size keys for hints', () => {
  expect(bracketKeyLabels('macos')).toEqual({ smaller: '[', larger: ']' });
});

it('takes a Rotate key that holds Shift as a plain [, Shift on another Rotate key as the hardness', () => {
  useSettingsStore.setState({
    appSettings: { keybinds: { rotate_left: ['shift', 'KeyL'] } } as never
  });
  expect(bracketKey(key({ key: 'L', code: 'KeyL', shiftKey: true }))).toEqual({ step: -1, shifted: false });
  expect(bracketKey(key({ key: ']', code: 'BracketRight', shiftKey: true }))).toEqual({ step: 1, shifted: true });
  expect(bracketKey(key({ key: 'l', code: 'KeyL' }))).toBeUndefined();
});
