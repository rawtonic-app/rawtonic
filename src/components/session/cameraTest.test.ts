import { expect, it } from "vitest";
import page from "../../../docs/TETHER_CAMERAS.md?raw";
import {
  COMPATIBILITY,
  buildReport,
  compatibilityOf,
  median,
  percentile,
  scrub,
  settingToChange,
  type ReportInfo,
} from "./cameraTest";

const INFO: ReportInfo = {
  appVersion: "0.1.0-alpha.3",
  system: "macos 26.0.1 (aarch64)",
  model: "Sony Alpha-A7 IV (PC Control)",
  firmware: "2.00",
  library: "2.5.34",
  transport: "imageCaptureCore",
  transferQueue: false,
  freeCameraCopies: true,
};

it("takes medians and nearest-rank percentiles", () => {
  expect(median([])).toBeNull();
  expect(median([3, 1, 2])).toBe(2);
  expect(percentile([1, 2, 3, 4, 5, 6, 7, 8, 9, 10], 95)).toBe(10);
  expect(percentile([1140, 1214, 1100], 95)).toBe(1214);
});

it("keeps paths, photo file names and the serial number out of a message", () => {
  expect(scrub("Downloading DSC01234.ARW from 5a33f1 failed: /Users/someone/Jobs/Acme/DSC01234.ARW", ["5a33f1"])).toBe(
    "Downloading <file> from <serial> failed: <path>",
  );
  // Names with spaces, Windows and quoted paths: nothing after the path's start stays.
  for (const message of [
    "Cannot copy /Users/Jane Doe/Jobs/Acme Secret/DSC00001.ARW",
    "Cannot copy C:\\Users\\Jane Doe\\Acme Secret\\a.raf: disk full",
    'Cannot copy "/home/jane doe/Acme Secret/x" (denied)',
    "Cannot copy \\\\server\\Jane Doe\\Acme Secret",
    "Cannot copy ~/Acme Secret/x",
  ])
    expect(scrub(message)).toMatch(/^Cannot copy "?<path>$/);
  // Camera values with a slash stay.
  expect(scrub("Shutter speed 1/250 at f/8")).toBe("Shutter speed 1/250 at f/8");
  expect(scrub("x".repeat(400)).length).toBe(160);
});

it("writes an anonymous report with the camera, its firmware, the driver and every step", () => {
  const report = buildReport(INFO, {
    connect: { state: "passed", ms: 2310 },
    shots: { state: "passed", asked: 3, arrived: 3, inLibrary: [900, 950, 1000], onScreen: [1100, 1140, 1214] },
    autofocus: { state: "skipped", reason: "skipped by the user" },
    burst: { state: "failed", asked: 5, arrived: 4, ms: 4200, reason: scrub("lost /Users/someone/DSC00001.ARW") },
  });
  expect(report).toContain("- Camera: Sony Alpha-A7 IV (PC Control)");
  expect(report).toContain("- Firmware: 2.00");
  expect(report).toContain("- Camera driver: libgphoto2 2.5.34 via ImageCaptureCore (macOS camera service)");
  expect(report).toContain("- Compatibility list: tested");
  expect(report).toContain("| Connect | pass | 2310 ms |");
  expect(report).toContain("photo on screen median 1140 ms, p95 1214 ms (n=3)");
  expect(report).toContain("| Burst from Rawtonic | FAIL | 4/5 in the library;");
  expect(report).toContain("| Cable out and back in | not run |  |");
  expect(report).not.toMatch(/someone|DSC00001/);
  const mock = buildReport({ ...INFO, model: "Mock Camera", firmware: null, library: null, transport: "mock" }, {});
  expect(mock).toContain("- Camera driver: mock camera (no camera library)");
  expect(mock).toContain("- Firmware: not reported");
  expect(mock).toContain("- Compatibility list: not listed");
});

it("finds a camera's entry by its model name or its maker", () => {
  expect(compatibilityOf("Fuji GFX 50 R")?.level).toBe("tested");
  expect(compatibilityOf("Canon EOS R5")?.level).toBe("notSupported");
  expect(compatibilityOf("Nikon Z 8")).toBeUndefined();
});

it("lists every entry of the compatibility data on the docs page with its level", () => {
  const rows = page.split("\n").filter((line) => line.startsWith("| ") && !line.startsWith("| Camera"));
  expect(rows).toHaveLength(COMPATIBILITY.length);
  const levels = { tested: "tested", reported: "reported", notSupported: "not supported" };
  for (const entry of COMPATIBILITY) {
    const row = rows.find((line) => line.startsWith(`| ${entry.name} |`));
    expect(row, entry.name).toBeTruthy();
    expect(row).toContain(`| ${levels[entry.level]} |`);
    if (entry.photoOnScreen) expect(row).toContain(`${entry.photoOnScreen.median} / ${entry.photoOnScreen.p95} ms`);
  }
});

it("changes a setting the camera lets change to another of its values, never Save to", () => {
  const settings = [
    { key: "capturetarget", value: "card", choices: ["card", "sdram"], readOnly: false },
    { key: "iso", value: "6400", choices: ["100", "6400"], readOnly: true },
    { key: "whitebalance", value: "Daylight", choices: ["Auto", "Daylight"], readOnly: false },
  ];
  const pick = settingToChange(settings);
  expect(pick?.setting.key).toBe("whitebalance");
  expect(pick?.other).toBe("Auto");
  expect(settingToChange([settings[0]])).toBeNull();
});
