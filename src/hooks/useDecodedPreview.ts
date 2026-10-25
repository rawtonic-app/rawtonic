import { useEffect, useRef, useState } from 'react';
import { noteThumbnailUse } from '../utils/thumbnailCacheUse';

/** Keep the last decoded frame until its replacement is ready. A late decoder
 * cannot resurrect an old photo. The caller owns the URLs. No transition or
 * extra RAF is needed. Interactive patches load directly to avoid this wait. */
export function useDecodedPreview<T extends { url: string }>(frame: T | null, identity: string) {
  const latest = useRef(frame);
  latest.current = frame;
  const [ready, setReady] = useState<{ identity: string; frame: T } | null>(null);
  const url = frame?.url;
  useEffect(() => {
    if (!url) return;
    let cancelled = false;
    const image = new Image();
    const commit = () => {
      if (!cancelled && latest.current?.url === url) {
        noteThumbnailUse(url);
        setReady({ identity, frame: latest.current });
      }
    };
    image.src = url;
    // decode resolves only once pixels are ready for the next paint. Older
    // webviews without it can still wait for the image's load event.
    if (typeof image.decode === 'function') void image.decode().then(commit, () => {});
    else if (image.complete && image.naturalWidth > 0) commit();
    else image.onload = commit;
    return () => { cancelled = true; image.onload = null; };
  }, [url, identity]);
  return frame && ready?.identity === identity ? ready.frame : null;
}
