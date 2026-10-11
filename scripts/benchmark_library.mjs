#!/usr/bin/env node
// Synthetic CPU-only library query benchmark. Excludes IPC, React and paint.
// Optional --baseline accepts a previously bundled computeSortedLibrary module.
import { build } from "esbuild";
import { mkdtemp, rm } from "node:fs/promises";
import { tmpdir } from "node:os";
import { resolve, join } from "node:path";
import { pathToFileURL } from "node:url";
import { createHash } from "node:crypto";
import { performance } from "node:perf_hooks";

const baselinePath = process.argv[process.argv.indexOf("--baseline") + 1];
const temporary = await mkdtemp(join(tmpdir(), "retouch-query-"));
globalThis.localStorage = {
  getItem: () => null,
  setItem() {},
  removeItem() {},
};
try {
  const output = join(temporary, "query.mjs");
  await build({
    entryPoints: ["src/hooks/useSortedLibrary.ts"],
    bundle: true,
    platform: "node",
    format: "esm",
    outfile: output,
    define: { "import.meta.env.VITE_RAWTONIC_PERF": '"0"' },
    banner: {
      js: `import {createRequire} from 'node:module'; const require = createRequire(${JSON.stringify(resolve("package.json"))});`,
    },
  });
  const current = (await import(pathToFileURL(output))).computeSortedLibrary;
  const baseline = process.argv.includes("--baseline")
    ? (await import(pathToFileURL(resolve(baselinePath)))).computeSortedLibrary
    : null;
  const count = 20596;
  function state() {
    const imageList = Array.from({ length: count }, (_, index) => {
      const n = (index * 7919) % count;
      return {
        path: `/synthetic/shoot-${n % 9}/DSC_${String(n).padStart(5, "0")}.ARW`,
        rating: n % 6,
        tags: n % 3 === 0 ? ["color:red"] : [],
        modified: n,
        is_raw: true,
        is_edited: false,
        is_virtual_copy: false,
        is_cloud_placeholder: false,
        group_id: null,
        exif: null,
      };
    });
    return {
      imageList,
      imageRatings: Object.fromEntries(
        imageList.map((file) => [file.path, file.rating]),
      ),
      imageFlags: {},
      filterCriteria: { rating: 0, colors: [], rawStatus: "all", flag: "all" },
      searchCriteria: { text: "", tags: [], mode: "OR" },
      sortCriteria: { key: "name", order: "asc" },
    };
  }
  const settings = { appSettings: { grouping: "off" } };
  const samples = { current: [], baseline: [] };
  const digests = {};
  // Fresh records in every round include first-query basename extraction cost.
  for (let round = 0; round < 21; round++) {
    const order =
      round % 2
        ? [
            ["current", current],
            ["baseline", baseline],
          ]
        : [
            ["baseline", baseline],
            ["current", current],
          ];
    for (const [name, query] of order) {
      if (!query) continue;
      const input = state();
      const begin = performance.now();
      const files = query(input, settings);
      const duration = performance.now() - begin;
      if (round > 0) samples[name].push(duration);
      digests[name] = createHash("sha256")
        .update(files.map((file) => file.path).join("\n"))
        .digest("hex");
      if (files.length !== count) throw new Error("Query lost photographs");
    }
  }
  if (baseline && digests.current !== digests.baseline)
    throw new Error("Sort order changed");
  // Repeated culling updates keep file records stable. Vary rating overrides,
  // flags, filters and text searches, comparing each result with the baseline.
  const repeated = { current: [], baseline: [] };
  const live = { current: state(), baseline: state() };
  for (const [name, query] of [
    ["current", current],
    ["baseline", baseline],
  ]) {
    if (query) query(live[name], settings);
  }
  for (let round = 0; round < 20; round++) {
    const hashes = {};
    const order =
      round % 2
        ? [
            ["current", current],
            ["baseline", baseline],
          ]
        : [
            ["baseline", baseline],
            ["current", current],
          ];
    for (const [name, query] of order) {
      if (!query) continue;
      const input = live[name];
      const path = input.imageList[round * 997].path;
      input.imageRatings = { ...input.imageRatings, [path]: 5 };
      input.imageFlags = { ...input.imageFlags, [path]: 1 };
      input.filterCriteria = {
        ...input.filterCriteria,
        rating: round % 4,
        flag: round % 2 ? "picked" : "all",
      };
      input.searchCriteria = {
        ...input.searchCriteria,
        text: round % 3 ? "" : "DSC_1",
      };
      const begin = performance.now();
      const files = query(input, settings);
      repeated[name].push(performance.now() - begin);
      hashes[name] = createHash("sha256")
        .update(files.map((file) => file.path).join("\n"))
        .digest("hex");
    }
    if (baseline && hashes.current !== hashes.baseline)
      throw new Error(`Culling order changed in round ${round}`);
  }
  const statistics = (values) => {
    const sorted = [...values].sort((a, b) => a - b);
    return {
      samples: sorted.length,
      p50_ms: sorted[Math.floor(sorted.length * 0.5)],
      p95_ms: sorted[Math.ceil(sorted.length * 0.95) - 1],
    };
  };
  let equivalenceCases = 0;
  if (baseline) {
    const fixture = state();
    fixture.imageList = fixture.imageList.slice(0, 300).map((file, i) => ({
      ...file,
      is_raw: i % 2 === 0,
      is_edited: i % 3 === 0,
      is_virtual_copy: i % 11 === 0,
      group_id: `group-${Math.floor(i / 3)}`,
      exif: {
        DateTimeOriginal:
          i % 4
            ? `2026:09:${String((i % 28) + 1).padStart(2, "0")} 12:00:00`
            : "",
        ExposureTime: i % 3 ? `1/${(i % 20) + 1}` : "invalid",
        FNumber: `f/${(i % 8) + 1}.4`,
        FocalLength: `${i % 100} mm`,
        ISOSpeedRatings: String((i % 8) * 100),
      },
    }));
    fixture.imageFlags = Object.fromEntries(
      fixture.imageList.map((file, i) => [file.path, (i % 3) - 1]),
    );
    for (const key of [
      "name",
      "date",
      "date_taken",
      "iso",
      "shutter_speed",
      "aperture",
      "focal_length",
      "rating",
      "edited",
      "unknown",
    ]) {
      for (const order of ["asc", "desc"]) {
        for (const grouping of ["off", "raw", "jpeg"]) {
          for (const filter of [
            { rating: 0, flag: "all" },
            { rating: 3, flag: "picked" },
            { rating: -1, flag: "rejected" },
          ]) {
            const input = {
              ...fixture,
              sortCriteria: { key, order },
              filterCriteria: { ...fixture.filterCriteria, ...filter },
            };
            const options = {
              appSettings: { grouping, groupEditedFiles: false },
            };
            const result = current(input, options).map((file) => file.path);
            const expected = baseline(input, options).map((file) => file.path);
            if (JSON.stringify(result) !== JSON.stringify(expected))
              throw new Error(
                `Sort/filter equivalence failed: ${key}/${order}/${grouping}/${filter.flag}`,
              );
            equivalenceCases++;
          }
        }
      }
    }
  }
  console.log(
    JSON.stringify(
      {
        count,
        node: process.version,
        scenario:
          "fresh-record filename sort, synthetic paths; no IPC/React/paint",
        current: statistics(samples.current),
        ...(baseline
          ? { baseline: statistics(samples.baseline), identical_order: true }
          : {}),
        repeated_culling_queries: {
          scenario:
            "same source records, changing rating/flag/filter/search; mutation setup excluded",
          current: statistics(repeated.current),
          ...(baseline
            ? { baseline: statistics(repeated.baseline), identical_order: true }
            : {}),
        },
        ...(baseline
          ? { sort_filter_grouping_equivalence_cases: equivalenceCases }
          : {}),
      },
      null,
      2,
    ),
  );
} finally {
  await rm(temporary, { recursive: true, force: true });
}
