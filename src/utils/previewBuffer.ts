import { Channel, invoke } from '@tauri-apps/api/core';

/** Large Linux patches arrive as a tiny URI packet on the direct channel.
 * Binary replies (small patches, final frames, compatibility mode) still
 * resolve through invoke. Rust drops each channel, releasing its callback,
 * including when a queued request is superseded or fails. */
export function previewBuffer(command: string, args: Record<string, unknown>, direct: boolean): Promise<ArrayBuffer> {
  if (!direct) return invoke<ArrayBuffer>(command, args);
  return new Promise<ArrayBuffer>((resolve, reject) => {
    const channel = new Channel<ArrayBuffer>(resolve);
    void invoke<ArrayBuffer>(command, { ...args, previewChannel: channel }).then((bytes) => {
      // An empty acknowledgement means the frame was sent on the channel.
      if (bytes.byteLength > 0) resolve(bytes);
    }, reject);
  });
}
