import "../../i18n";
import { afterEach, expect, it, vi } from "vitest";
import { act, cleanup, render } from "@testing-library/react";
import StudioBrowser from "./StudioBrowser";
import { computeGroupedLibrary, type LibraryQueryState } from "../../hooks/useSortedLibrary";
import { RawStatus, SortDirection, type ImageFile } from "../ui/AppProperties";

/**
 * The grid side of the library search at 50,000 photographs (G37): applying
 * a catalog answer to the loaded list (computeGroupedLibrary) and React
 * committing the virtualized grid for it. Node + jsdom: no layout, paint or
 * WebKit, so it bounds the JavaScript part only. Run with
 * `RAWTONIC_BENCH=1 npx vitest run src/components/studio/libraryScale.bench.test.tsx`.
 */
const env = (globalThis as { process?: { env: Record<string, string | undefined> } }).process?.env;
const run = env?.RAWTONIC_BENCH ? it : it.skip;
afterEach(cleanup);

const PHOTOS = 50_000;
const files: ImageFile[] = Array.from({ length: PHOTOS }, (_, i) => ({
  path: `/lib/Capture/2023-${String((i % 12) + 1).padStart(2, "0")}/DSC${String(i).padStart(5, "0")}.ARW`,
  is_edited: false,
  modified: i,
  rating: i % 6,
  tags: i % 5 ? null : ["color:red"],
  exif: null,
  is_virtual_copy: false,
  is_cloud_placeholder: false,
  is_raw: true,
  group_id: null,
}));

function median(work: () => void, runs = 15): number {
  const times: number[] = [];
  for (let i = 0; i < runs; i++) {
    const start = performance.now();
    work();
    times.push(performance.now() - start);
  }
  times.sort((a, b) => a - b);
  return Math.round(times[Math.floor(runs / 2)] * 100) / 100;
}

run("applies a catalog answer and commits the grid for 50,000 photographs", async () => {
  vi.stubGlobal("ResizeObserver", class { observe() {} disconnect() {} });
  const quarter = new Set(files.filter((_, i) => i % 4 === 0).map((f) => f.path));
  const few = new Set(files.filter((_, i) => i % 1000 === 0).map((f) => f.path));
  const state = (catalogMatches: Set<string> | null, rating = 0): LibraryQueryState => ({
    imageList: files,
    imageRatings: {},
    imageFlags: {},
    filterCriteria: { colors: [], rating, rawStatus: RawStatus.All, flag: "all" },
    searchCriteria: { text: catalogMatches ? "x" : "", tags: [], mode: "OR" },
    sortCriteria: { key: "name", order: SortDirection.Ascending },
    catalogMatches,
  });
  computeGroupedLibrary(state(null), {}); // the folder's ordering, once
  const filterMs = {
    none: median(() => computeGroupedLibrary(state(null), {})),
    quarter: median(() => computeGroupedLibrary(state(quarter), {})),
    fifty: median(() => computeGroupedLibrary(state(few), {})),
    quarterAndRating: median(() => computeGroupedLibrary(state(quarter, 3), {})),
  };
  const props = {
    onSelect: vi.fn(),
    onOpen: vi.fn(),
    onRate: vi.fn(),
    onContextMenu: vi.fn(),
    requestThumbnails: vi.fn(),
  };
  const lists = [quarter, few, null].map((m) => computeGroupedLibrary(state(m), {}).displayList);
  const view = render(<StudioBrowser files={files} {...props} />);
  const commitMs: number[] = [];
  for (let i = 0; i < 15; i++) {
    const list = lists[i % lists.length];
    const start = performance.now();
    act(() => view.rerender(<StudioBrowser files={list} {...props} listing={String(i)} />));
    commitMs.push(performance.now() - start);
  }
  commitMs.sort((a, b) => a - b);
  console.log(
    JSON.stringify({
      photos: PHOTOS,
      environment: "node + jsdom (no layout, paint or WebKit)",
      filter_ms_median: filterMs,
      grid_commit_ms_median: Math.round(commitMs[7] * 100) / 100,
      grid_commit_ms_max: Math.round(commitMs[14] * 100) / 100,
    }),
  );
  expect(lists[0]).toHaveLength(PHOTOS / 4);
});
