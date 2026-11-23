// The layouts in multiViewLayout.json are also deserialized by Rust
// (src-tauri multi_view.rs, layouts_from_the_webview_carry_their_zooms).
import { expect, it } from 'vitest';
import payloads from './multiViewLayout.json';
import { layoutZooms } from '../utils/multiView';

const shown = ['/photos/a.dng', '/photos/b.dng'];
const zoom = { scale: 1, center: [0.25, 0.5] as [number, number] };

it('builds the zooms a layout carries as the native side reads them', () => {
  // a was the primary at 1:1; b becomes the primary.
  expect(layoutZooms(false, zoom, '/photos/a.dng', {}, '/photos/b.dng', shown)).toEqual(payloads.unlocked.zooms);
  expect(layoutZooms(true, zoom, '/photos/a.dng', {}, '/photos/b.dng', shown)).toEqual(payloads.locked.zooms);
  // At fit it leaves its own zoom behind.
  expect(layoutZooms(false, null, '/photos/a.dng', { '/photos/a.dng': zoom }, '/photos/b.dng', shown)).toEqual(
    payloads.fit.zooms
  );
});

it('keeps the layout arguments the command takes', () => {
  for (const name of ['unlocked', 'locked', 'fit'] as const) {
    expect(Object.keys(payloads[name]).sort()).toEqual(['cells', 'primary', 'zooms']);
    expect(Object.keys(payloads[name].zooms).sort()).toEqual(['cells', 'zoom']);
  }
  expect(Object.keys(payloads.single).sort()).toEqual(['cells', 'primary']);
});
