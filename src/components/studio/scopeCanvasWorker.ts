/// <reference lib="webworker" />
// The scope worker: StudioScopes hands it its canvas once, then each scope
// image (PNG, base64) as it arrives; decoding and drawing happen here.
import { createScopeDrawer } from './scopeDrawer';

/** Size of the scope images (`SCOPE_SIZE` in scopes.rs). */
const SIZE = 256;

let context: OffscreenCanvasRenderingContext2D | null = null;
const draw = createScopeDrawer(
  async (png) => createImageBitmap(await (await fetch(`data:image/png;base64,${png}`)).blob()),
  () => context,
  SIZE
);

self.onmessage = (event: MessageEvent<{ canvas?: OffscreenCanvas; png?: string }>) => {
  if (event.data.canvas) context = event.data.canvas.getContext('2d');
  else if (event.data.png) draw(event.data.png);
};
