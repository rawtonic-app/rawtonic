import { describe, expect, it } from 'vitest';
import { importEndView, watchUserActivity } from './importInteraction';

describe('the view when an import ends', () => {
  const base = { imported: true, viewerShown: false, sameView: true, acted: false };
  it('shows Previous Import when nobody touched the app while it ran', () => {
    expect(importEndView(base)).toBe('show');
  });
  it('keeps a photographer culling during the import where they are and offers Show', () => {
    expect(importEndView({ ...base, acted: true })).toBe('offer');
  });
  it('keeps an open photograph or another view and offers Show', () => {
    expect(importEndView({ ...base, viewerShown: true })).toBe('offer');
    expect(importEndView({ ...base, sameView: false })).toBe('offer');
  });
  it('has nothing to show when nothing was added', () => {
    expect(importEndView({ ...base, imported: false, acted: true })).toBe('stay');
  });
});

describe('watching for the user', () => {
  it('notes a key press, a click and a scroll from the moment it starts until it stops', () => {
    const target = new EventTarget() as unknown as Window;
    const activity = watchUserActivity(target);
    expect(activity.acted()).toBe(false);
    target.dispatchEvent(new Event('keydown'));
    expect(activity.acted()).toBe(true);
    activity.stop();

    for (const type of ['pointerdown', 'wheel']) {
      const watch = watchUserActivity(target);
      target.dispatchEvent(new Event(type));
      expect(watch.acted()).toBe(true);
      watch.stop();
    }
  });
  it('ignores events after it stopped and other events', () => {
    const target = new EventTarget() as unknown as Window;
    const activity = watchUserActivity(target);
    target.dispatchEvent(new Event('mousemove'));
    activity.stop();
    target.dispatchEvent(new Event('keydown'));
    expect(activity.acted()).toBe(false);
  });
});
