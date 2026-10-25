// SPDX-License-Identifier: AGPL-3.0-or-later
import { invoke } from "@tauri-apps/api/core";
import { Invokes } from "../components/ui/AppProperties";

const pending = new Set<string>();
let scheduled = false;
function schedule() {
  if (scheduled) return;
  scheduled = true;
  requestAnimationFrame(() => {
    scheduled = false;
    const keys = [...pending].slice(0, 500);
    keys.forEach((key) => pending.delete(key));
    if (keys.length)
      void invoke(Invokes.StorageTouchThumbnails, { keys }).catch(() => {});
    if (pending.size) schedule();
  });
}

/** Record actual webview reads, including reads served by the browser cache.
 * Only opaque thumbnail keys cross IPC; native code chooses the owned root. */
export function noteThumbnailUse(source: string | undefined) {
  if (!source) return;
  try {
    const url = new URL(source);
    if (
      url.protocol !== "asset:" &&
      !(
        ["http:", "https:"].includes(url.protocol) &&
        url.hostname === "asset.localhost"
      )
    )
      return;
    const name = decodeURIComponent(
      url.pathname.slice(url.pathname.lastIndexOf("/") + 1),
    );
    const match = /^([a-f0-9]{64})_(?:small|medium)\.jpg$/i.exec(name);
    if (!match) return;
    pending.add(match[1]);
    schedule();
  } catch {
    /* Blob/data previews and malformed URLs do not name disk caches. */
  }
}
