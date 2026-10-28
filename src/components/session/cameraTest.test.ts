import { expect, it } from "vitest";
import page from "../../../docs/TETHER_CAMERAS.md?raw";
import {
  COMPATIBILITY,
  buildReport,
  cameraTestPayload,
  compatibilityOf,
  median,
  percentile,
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
  failures: 0,
};

it("takes medians and nearest-rank percentiles", () => {
  expect(median([])).toBeNull();
  expect(median([3, 1, 2])).toBe(2);
  expect(percentile([1, 2, 3, 4, 5, 6, 7, 8, 9, 10], 95)).toBe(10);
  expect(percentile([1140, 1214, 1100], 95)).toBe(1214);
});

it("never puts a step's message into the report, only the test's own words", () => {
  const report = buildReport(INFO, {
    shots: {
      state: "failed",
      asked: 1,
      arrived: 0,
      reason: "photo not added to the library",
      detail: "Cannot copy /Users/Jane Doe/Acme Secret/客户机密.RAF",
    },
  });
  expect(report).toContain("| Shots from Rawtonic | FAIL | 0/1 in the library; photo not added to the library |");
  expect(report).not.toMatch(/Jane|Acme|客户|Cannot copy/);
});

it("writes an anonymous report with the camera, its firmware, the driver and every step", () => {
  const report = buildReport(INFO, {
    connect: { state: "passed", ms: 2310 },
    shots: { state: "passed", asked: 3, arrived: 3, inLibrary: [900, 950, 1000], onScreen: [1100, 1140, 1214] },
    autofocus: { state: "skipped", reason: "skipped by the user" },
    burst: { state: "failed", asked: 5, arrived: 4, ms: 4200, reason: "no photo arrived", detail: "lost /Users/someone/DSC00001.ARW" },
  });
  expect(report).toContain("- Camera: Sony Alpha-A7 IV (PC Control)");
  expect(report).toContain("- Firmware: 2.00");
  expect(report).toContain("- Camera driver: libgphoto2 2.5.34 via ImageCaptureCore (macOS camera service)");
  expect(report).toContain("- Compatibility list: tested");
  expect(report).toContain("- Files not added or shots not delivered during the test: 0");
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

it("marks a report from an experimental driver path and counts shots not received", () => {
  const report = buildReport({ ...INFO, model: "Canon EOS R6", experimental: true, failures: 2, notReceived: 1 }, {});
  expect(report).toContain("- Compatibility list: experimental, testers wanted");
  expect(report).toContain("- Driver path: experimental (Canon EOS");
  expect(report).toContain("- Shots not received (on the memory card if the camera took them): 1");
  expect(buildReport(INFO, {})).not.toContain("Driver path");
});

it("finds a camera's entry by its model name or its maker", () => {
  expect(compatibilityOf("Fuji GFX 50 R")?.level).toBe("tested");
  expect(compatibilityOf("Canon EOS R5")?.level).toBe("experimental");
  expect(compatibilityOf("Nikon Z 8")).toBeUndefined();
});

it("lists every entry of the compatibility data on the docs page with its level", () => {
  const rows = page.split("\n").filter((line) => line.startsWith("| ") && !line.startsWith("| Camera"));
  expect(rows).toHaveLength(COMPATIBILITY.length);
  const levels = {
    tested: "tested",
    reported: "reported",
    experimental: "experimental — testers wanted",
    notSupported: "not supported",
  };
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

it("sets ISO to a plain number: the next one up, or from Auto the lowest from 100 up", () => {
  const iso = (value: string, choices: string[]) =>
    settingToChange([{ key: "iso", value, choices, readOnly: false }])?.other;
  const sony = ["Auto ISO", "50", "100", "200", "400", "800", "Auto ISO Multi Frame Noise Reduction", "100 Multi Frame Noise Reduction"];
  expect(iso("Auto ISO", sony)).toBe("100");
  expect(iso("Auto ISO Multi Frame Noise Reduction", sony)).toBe("100");
  expect(iso("100", sony)).toBe("200");
  expect(iso("800", sony)).toBe("400");
  expect(iso("Auto ISO", ["Auto ISO", "6400", "12800"])).toBe("6400");
  // No two plain numbers: another setting is changed.
  const settings = [
    { key: "iso", value: "Auto ISO", choices: ["Auto ISO", "Auto ISO Multi Frame Noise Reduction"], readOnly: false },
    { key: "whitebalance", value: "Daylight", choices: ["Auto", "Daylight"], readOnly: false },
  ];
  expect(settingToChange(settings)?.setting.key).toBe("whitebalance");
});

it("names the camera test's camera by the library's model name, its first word the make", () => {
  const app = { version: "0.1.0-alpha.4", os: "macos", osVersion: "26.5", arch: "aarch64" };
  expect(cameraTestPayload(app, { model: "Canon EOS R5", firmware: "1.8.1" }, "r")).toEqual({
    schema: 1,
    app,
    camera: { make: "Canon", model: "Canon EOS R5", firmware: "1.8.1" },
    report: "r",
  });
  // One word: no make to tell; no firmware: none sent; markup dropped.
  expect(cameraTestPayload(app, { model: "Mock<b>", firmware: null }, "r")?.camera).toEqual({
    make: "unknown",
    model: "Mockb",
  });
  expect(cameraTestPayload(app, { model: "  " }, "r")).toBeNull();
});
