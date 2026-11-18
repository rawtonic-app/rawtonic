/** Draws scope images (PNG, base64) into a canvas, newest first: an image
 * that arrives while another decodes replaces any waiting one, and a decoded
 * image is not drawn when a newer one is already waiting. Runs in the scope
 * worker (scopeCanvasWorker.ts) on an OffscreenCanvas, so neither decoding
 * nor drawing takes the webview's main thread during drags (notes:
 * parity-gaps/G23-analysis.md, SCP2). */
export interface ScopeTarget {
  clearRect(x: number, y: number, w: number, h: number): void;
  drawImage(image: ImageBitmap, x: number, y: number): void;
}

export function createScopeDrawer(
  decode: (png: string) => Promise<ImageBitmap>,
  getTarget: () => ScopeTarget | null,
  size: number
) {
  let waiting: string | null = null;
  let busy = false;
  const drawWaiting = async () => {
    busy = true;
    while (waiting !== null) {
      const png = waiting;
      waiting = null;
      let bitmap: ImageBitmap | null = null;
      try {
        bitmap = await decode(png);
      } catch {
        // A PNG that does not decode leaves the last image, as an <img> would.
      }
      const target = getTarget();
      if (bitmap && waiting === null && target) {
        target.clearRect(0, 0, size, size);
        target.drawImage(bitmap, 0, 0);
      }
      bitmap?.close();
    }
    busy = false;
  };
  return (png: string) => {
    waiting = png;
    if (!busy) void drawWaiting();
  };
}

/** Whether this webview can draw the scopes in a worker. */
export function offscreenScopesSupported(): boolean {
  return (
    typeof Worker !== 'undefined' &&
    typeof OffscreenCanvas !== 'undefined' &&
    typeof HTMLCanvasElement !== 'undefined' &&
    'transferControlToOffscreen' in HTMLCanvasElement.prototype
  );
}
