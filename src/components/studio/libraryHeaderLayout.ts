import { useEffect, useState } from "react";

/** Budget in unscaled CSS pixels: enlarged controls get the same room. */
export const libraryHeaderFits = (width: number, scale: number) => width / scale >= 1440;

export function useLibraryHeaderFits(scale: number) {
  const [width, setWidth] = useState(() => window.innerWidth);
  useEffect(() => {
    const resize = () => setWidth(window.innerWidth);
    window.addEventListener("resize", resize);
    return () => window.removeEventListener("resize", resize);
  }, []);
  return libraryHeaderFits(width, scale);
}
