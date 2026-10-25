// SPDX-License-Identifier: AGPL-3.0-or-later
import { useEffect, useRef, useState, type ImgHTMLAttributes } from "react";
import { convertFileSrc, invoke } from "@tauri-apps/api/core";
import { Image as ImageIcon } from "lucide-react";
import { Invokes } from "./AppProperties";
import { useProcessStore } from "../../store/useProcessStore";

const restoring = new Map<string, Promise<[string, string]>>();
let revision = 0;
function restore(path: string) {
  let request = restoring.get(path);
  if (!request) {
    request = invoke<[string, string]>(Invokes.RestoreThumbnail, { path })
      .then((paths) => {
        // The asset handler resolves URI.path(), ignoring this query. Replacing
        // the store URLs also prevents future mounts from reusing a cached 404.
        const suffix = `?recovered=${Date.now()}-${++revision}`;
        const urls = paths.map(
          (path) => convertFileSrc(path.replace(/\\/g, "/")) + suffix,
        ) as [string, string];
        useProcessStore.getState().setProcess((state) => ({
          thumbnails: { ...state.thumbnails, [path]: urls[0] },
          mediumThumbnails: { ...state.mediumThumbnails, [path]: urls[1] },
        }));
        return urls;
      })
      .finally(() => restoring.delete(path));
    restoring.set(path, request);
  }
  return request;
}

/** A disk-cache URL can expire while a tile is outside the viewport. Restore
 * that photo on demand, once until a successful load, without replacing the native
 * viewport queue or retaining inline pixel data for the whole library. */
export default function CachedThumbnail({
  sourcePath,
  medium = false,
  src,
  alt,
  ...props
}: Omit<ImgHTMLAttributes<HTMLImageElement>, "onError"> & {
  sourcePath: string;
  medium?: boolean;
}) {
  const identity = `${sourcePath}|${src}|${medium}`;
  const current = useRef(identity);
  current.current = identity;
  const mounted = useRef(false);
  const attempted = useRef(false);
  const [replacement, setReplacement] = useState<{
    identity: string;
    url: string | null;
  } | null>(null);
  useEffect(() => {
    mounted.current = true;
    return () => {
      mounted.current = false;
    };
  }, []);
  useEffect(() => {
    attempted.current = false;
  }, [sourcePath, medium]);
  const shown = replacement?.identity === identity ? replacement.url : src;
  if (!shown)
    return <ImageIcon size={24} aria-label={alt} className={props.className} />;
  return (
    <img
      {...props}
      src={shown}
      alt={alt}
      onLoad={(event) => {
        attempted.current = false;
        props.onLoad?.(event);
      }}
      onError={() => {
        setReplacement({ identity, url: null });
        if (attempted.current) return;
        attempted.current = true;
        void restore(sourcePath)
          .then((paths) => {
            if (!mounted.current || current.current !== identity) return;
            setReplacement({ identity, url: paths[medium ? 1 : 0] });
          })
          .catch(() => {
            /* Keep the placeholder for an unavailable original. */
          });
      }}
    />
  );
}
