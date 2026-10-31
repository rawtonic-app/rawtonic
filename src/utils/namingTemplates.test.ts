import { beforeEach, expect, it } from "vitest";
import {
  counterDigits,
  dropNamingBlock,
  INGEST_TOKEN_NAMES,
  insertToken,
  joinSegments,
  moveToken,
  namingExample,
  namingTokens,
  NAMING_PRESETS_KEY,
  parseToken,
  formatToken,
  readNamingPresets,
  removeToken,
  splitTemplate,
  swapTokens,
  templateSegments,
  translateLegacyExport,
  validNamingTemplate,
  withCounterDigits,
} from "./namingTemplates";

beforeEach(() => localStorage.clear());

it("names import, rename and export with the same tokens", () => {
  expect(namingExample("{yyyy}{mm}{dd}_{hh}{MM}_{orig_base}_{seq:auto}", "export", "/a/DSC.RAF", "tiff", "", 2, 100)).toBe(
    "20261011_1430_DSC_002.tiff",
  );
  expect(namingExample("{yyyy}{mm}{dd}_{hh}{MM}_{seq:4}.{ext|lower}", "ingest", "DSC.RAF", "", "", 2)).toBe(
    "20261011_1430_0002.raf",
  );
  expect(namingExample("{date:yyyymmdd}_{orig_base}", "rename", "/a/DSC.RAF")).toBe("20261011_DSC.RAF");
});

it("translates templates saved with the first export tokens as the backend does", () => {
  // Mirrors rawtonic_naming::translate_legacy_export (its test uses the same cases).
  expect(translateLegacyExport("{YYYY}{MM}{DD}-{hh}{mm}_{original_filename}_{sequence} x")).toBe(
    "{yyyy}{mm}{dd}-{hh}{MM}_{orig_base}_{seq:auto} x",
  );
  expect(translateLegacyExport("Zürich_{sequence}")).toBe("Zürich_{seq:auto}");
  // The names stay what the old export gave (month vs minute kept apart).
  expect(
    namingExample(translateLegacyExport("{YYYY}{MM}{DD}_{hh}{mm}_{original_filename}_{sequence}"), "export", "/a/DSC.RAF", "jpg", "", 98, 4),
  ).toBe("20261011_1430_DSC_098.jpg");
});

it("round-trips every template through its chips and free text", () => {
  for (const template of [
    "",
    "{orig_base}",
    "Shoot_{seq:4}_{orig_base|upper}.{ext}broken{",
    "{date:yyyymmdd}{time:hhMMss}",
    "a}b{c",
    "{job}/{date}/{format}",
  ]) {
    const { text, tokens } = templateSegments(template);
    expect(text.length).toBe(tokens.length + 1);
    expect(joinSegments(text, tokens), template).toBe(template);
    expect(splitTemplate(template).join("")).toBe(template);
  }
  for (const part of ["{seq:4|upper}", "{date:dd.mm.yyyy}", "{orig_base|lower|pad:3}", "{job}"]) {
    const { name, option, transforms } = parseToken(part);
    expect(formatToken(name, option, transforms)).toBe(part);
  }
});

it("adds, removes, moves and swaps chips without losing text", () => {
  const template = "Wedding_{date}-{seq:3}";
  expect(insertToken(template, 0, 7, "{job}")).toBe("Wedding{job}_{date}-{seq:3}");
  expect(insertToken(template, 2, 0, "{lens}")).toBe("Wedding_{date}-{seq:3}{lens}");
  expect(removeToken(template, 0)).toBe("Wedding_-{seq:3}");
  expect(moveToken(template, 1, 0)).toBe("Wedding_{seq:3}{date}-");
  expect(moveToken(template, 0, 2)).toBe("Wedding_-{seq:3}{date}");
  expect(moveToken(template, 0, 0)).toBe(template);
  expect(swapTokens(template, 0, 1)).toBe("Wedding_{seq:3}-{date}");
  expect(swapTokens(template, 0, -1)).toBe(template);
});

it("chooses the counter's digits for every counter", () => {
  expect(counterDigits("{orig_base}")).toBeNull();
  expect(counterDigits("{seq}")).toBe("");
  expect(withCounterDigits("a{seq}b{seq:2|upper}", "4")).toBe("a{seq:4}b{seq:4|upper}");
  expect(counterDigits(withCounterDigits("{seq:4}", "auto"))).toBe("auto");
  expect(withCounterDigits("{seq:4}", "")).toBe("{seq}");
  expect(namingExample("{seq:auto}", "export", "a.RAF", "jpg", "", 98, 4)).toBe("098.jpg");
  expect(namingExample("{seq:4}.{ext}", "ingest", "a.RAF", "", "", 0, 4)).toBe("0000.RAF");
});

it("offers only the tokens a mode fills", () => {
  const names = (mode: "ingest" | "rename" | "export") => namingTokens(mode).map((t) => t.name);
  expect(names("ingest")).toContain("job");
  expect(names("ingest")).not.toContain("format");
  expect(names("rename")).not.toContain("job");
  expect(names("export")).toEqual(expect.arrayContaining(["job", "format", "recipe", "camera"]));
  for (const mode of ["ingest", "rename", "export"] as const)
    for (const token of namingTokens(mode))
      expect(validNamingTemplate(`${token.value}.x`, mode), `${mode} ${token.value}`).toBe(true);
  // Every token the backend fills stays valid, offered in the lists or not.
  for (const name of INGEST_TOKEN_NAMES.filter((n) => n !== "format" && n !== "recipe"))
    expect(validNamingTemplate(`{${name}}`, "ingest"), name).toBe(true);
  for (const name of INGEST_TOKEN_NAMES) expect(validNamingTemplate(`{${name}}`, "export"), name).toBe(true);
  expect(validNamingTemplate("{seq_start}_{seq:4}.{ext}", "ingest")).toBe(true);
  expect(validNamingTemplate("{session}_{seq:4}.{ext}", "ingest")).toBe(true);
  expect(namingExample("{date:yyyymmdd}_{orig}", "rename", "/a/DSC.NEF")).toBe("20261011_DSC.NEF");
});

it("rejects malformed or unsupported templates before writing files", () => {
  for (const value of ["", "{unknown}", "{orig", "a}", "../{orig}", "{seq:99}", "{seq:}", "{orig|oops}", "{seq:2:4}", "{date:%Y}", "{iso:4}", "{original_filename}"])
    expect(validNamingTemplate(value, "ingest"), value).toBe(false);
  expect(validNamingTemplate("{format}", "ingest")).toBe(false);
  expect(validNamingTemplate("{job}", "rename")).toBe(false);
  expect(validNamingTemplate("{orig_base|upper}_{seq:auto}.{ext|lower}", "rename")).toBe(true);
  expect(validNamingTemplate("{date:dd.mm.yyyy} {time:hhMM}", "export")).toBe(true);
});

it("checks folder templates as the backend renders them", () => {
  expect(validNamingTemplate("", "export", "folder")).toBe(true);
  expect(validNamingTemplate("{job}/{date}/{format}", "export", "folder")).toBe(true);
  expect(validNamingTemplate("Web\\small", "export", "folder")).toBe(true);
  for (const bad of ["../x", "a/../b", "./a", "C:/x", "{oops}", "a:b"])
    expect(validNamingTemplate(bad, "export", "folder"), bad).toBe(false);
  expect(namingExample("{job}/{title}/{format}", "export", "a.RAF", "jpg", "Acme", 1, 1, "folder")).toBe("Acme/JPEG");
});

it("keeps the photograph's extension when an import template leaves it out", () => {
  // Mirrors rawtonic_ingest::render_name (native QA 2026-10-16).
  expect(namingExample("{yyyy}_{orig_base}", "ingest", "DSC.RAF")).toBe("2026_DSC.RAF");
  expect(namingExample("{orig_base}.{ext|lower}", "ingest", "DSC.RAF")).toBe("DSC.raf");
  expect(namingExample("{orig}", "ingest", "DSC.RAF")).toBe("DSC.RAF");
  // Mirrors render_or_original: a name its tokens leave empty is the original's.
  expect(namingExample("{title}", "export", "DSC.RAF")).toBe("DSC.jpg");
});

it("moves repeated tokens by position and inserts new tokens without losing text", () => {
  const parts = ["{yyyy}", "-", "{yyyy}", "_", "{orig_base}"];
  expect(dropNamingBlock(parts, 5, "", 0)).toBe("-{yyyy}_{orig_base}{yyyy}");
  expect(dropNamingBlock(parts, 2, "{seq}")).toBe("{yyyy}-{seq}{yyyy}_{orig_base}");
});

it("takes over the presets saved before the editor was shared, once", () => {
  localStorage.setItem("rawtonic.naming.ingest", JSON.stringify([{ name: "Card", template: "{job}_{seq:4}", sequenceStart: 5 }]));
  localStorage.setItem(
    "rawtonic.naming.export",
    JSON.stringify([
      { name: "Web", template: "{YYYY}{MM}{DD}_{original_filename}_{sequence}" },
      { name: "Card", template: "{original_filename}" },
      { bad: true },
    ]),
  );
  expect(readNamingPresets()).toEqual([
    { name: "Card", template: "{job}_{seq:4}", sequenceStart: 5 },
    { name: "Web", template: "{yyyy}{mm}{dd}_{orig_base}_{seq:auto}" },
  ]);
  // Done once: a preset deleted since does not come back.
  localStorage.setItem(NAMING_PRESETS_KEY, "[]");
  expect(readNamingPresets()).toEqual([]);
});
