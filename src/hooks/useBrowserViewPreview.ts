import { useEffect, useRef, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { Invokes } from "../components/ui/AppProperties";

export interface BrowserViewFrame {
  key: string;
  contentKey: string;
  url: string;
  width: number;
  height: number;
}

/** An isolated developed frame. Stale completions cannot replace a newer
 * photo/recipe, and every object URL is released when superseded/unmounted. */
export function useBrowserViewPreview(
  path: string | null,
  recipe: string | null,
  resolution: number,
  revision = "",
) {
  const key = JSON.stringify([path, recipe, resolution, revision]);
  const contentKey = JSON.stringify([path, recipe, revision]);
  const [frame, setFrame] = useState<BrowserViewFrame | null>(null);
  const [error, setError] = useState<{ key: string; message: string } | null>(
    null,
  );
  const urlRef = useRef<string | null>(null);
  useEffect(() => {
    if (!path) return;
    let cancelled = false;
    const timer = setTimeout(() => {
      void invoke<ArrayBuffer>(Invokes.RenderBrowserView, {
        path,
        jsAdjustments: recipe ? JSON.parse(recipe) : null,
        targetResolution: resolution,
      })
        .then((buffer) => {
          if (cancelled) return;
          if (buffer.byteLength <= 8) throw new Error("Empty browser view");
          const header = new DataView(buffer);
          const url = URL.createObjectURL(
            new Blob([buffer.slice(8)], { type: "image/jpeg" }),
          );
          if (urlRef.current) URL.revokeObjectURL(urlRef.current);
          urlRef.current = url;
          setFrame({
            key,
            contentKey,
            url,
            width: header.getUint32(0, true),
            height: header.getUint32(4, true),
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
    [],
  );
  return {
    frame: path && frame?.contentKey === contentKey ? frame : null,
    error: error?.key === key ? error.message : null,
  };
}
