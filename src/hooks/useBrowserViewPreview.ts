import { useEffect, useRef, useState } from 'react';
import { invoke } from '@tauri-apps/api/core';
import { Invokes } from '../components/ui/AppProperties';
import { previewFrameType } from '../utils/previewFrameType';

export interface BrowserViewFrame {
  key: string;
  contentKey: string;
  url: string;
  width: number;
  height: number;
  /** The frame's own width in pixels (as developed). */
  pixelWidth: number;
}

/** The width in a PNG's header or a baseline or progressive JPEG's frame
 * header; 0 if none. */
export function frameWidth(bytes: Uint8Array): number {
  if (bytes.length >= 24 && bytes[0] === 0x89 && bytes[1] === 0x50 && bytes[2] === 0x4e && bytes[3] === 0x47) {
    return ((bytes[16] << 24) | (bytes[17] << 16) | (bytes[18] << 8) | bytes[19]) >>> 0;
  }
  let at = 2;
  while (at + 9 < bytes.length && bytes[at] === 0xff) {
    const marker = bytes[at + 1];
    const length = (bytes[at + 2] << 8) | bytes[at + 3];
    if (marker >= 0xc0 && marker <= 0xcf && marker !== 0xc4 && marker !== 0xc8 && marker !== 0xcc) {
      return (bytes[at + 7] << 8) | bytes[at + 8];
    }
    if (marker === 0xda) return 0;
    at += 2 + length;
  }
  return 0;
}

/** An isolated developed frame. Stale completions cannot replace a newer
 * photo/recipe, and every object URL is released when superseded/unmounted. */
export function useBrowserViewPreview(path: string | null, recipe: string | null, resolution: number, revision = '') {
  const key = JSON.stringify([path, recipe, resolution, revision]);
  const contentKey = JSON.stringify([path, recipe, revision]);
  const [frame, setFrame] = useState<BrowserViewFrame | null>(null);
  const [error, setError] = useState<{ key: string; message: string } | null>(null);
  const urlRef = useRef<string | null>(null);
  useEffect(() => {
    if (!path) return;
    let cancelled = false;
    const timer = setTimeout(() => {
      void invoke<ArrayBuffer>(Invokes.RenderBrowserView, {
        path,
        jsAdjustments: recipe ? JSON.parse(recipe) : null,
        targetResolution: resolution
      })
        .then((buffer) => {
          if (cancelled) return;
          if (buffer.byteLength <= 8) throw new Error('Empty browser view');
          const header = new DataView(buffer);
          const frame = buffer.slice(8);
          const url = URL.createObjectURL(new Blob([frame], { type: previewFrameType(frame) }));
          if (urlRef.current) URL.revokeObjectURL(urlRef.current);
          urlRef.current = url;
          setFrame({
            key,
            contentKey,
            url,
            width: header.getUint32(0, true),
            height: header.getUint32(4, true),
            pixelWidth: frameWidth(new Uint8Array(frame))
          });
        })
        .catch((err: unknown) => {
          if (!cancelled) setError({ key, message: String(err) });
        });
    }, 100);
    return () => {
      cancelled = true;
      clearTimeout(timer);
    };
  }, [path, recipe, resolution, revision, key, contentKey]);
  useEffect(
    () => () => {
      if (urlRef.current) URL.revokeObjectURL(urlRef.current);
    },
    []
  );
  return {
    frame: path && frame?.contentKey === contentKey ? frame : null,
    error: error?.key === key ? error.message : null
  };
}
