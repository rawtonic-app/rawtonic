import { expect, it } from "vitest";
import { namingExample, namingTokens, splitTemplate } from "./namingTemplates";
it("keeps month and minute meanings separate between the existing engines", () => {
  expect(
    namingExample(
      "{YYYY}{MM}{DD}_{hh}{mm}_{original_filename}_{sequence}",
      "export",
      "/a/DSC.RAF",
      "tiff",
      "",
      2,
      100,
    ),
  ).toBe("20261011_1430_DSC_002.tiff");
  expect(
    namingExample(
      "{yyyy}{mm}{dd}_{hh}{MM}_{seq:4}.{ext|lower}",
      "ingest",
      "DSC.RAF",
      "",
      "",
      2,
    ),
  ).toBe("20261011_1430_0002.raf");
});
it("retains free text, separators and malformed tokens for editing without data loss", () => {
  const template = "Shoot_{seq:4}_{orig_base|upper}.{ext}broken{";
  expect(splitTemplate(template).join("")).toBe(template);
});
it("offers only populated import tokens and preserves unknown values in previews", () => {
  // Camera tokens are filled from EXIF since the backend reads it.
  expect(namingTokens("ingest").some((t) => t.value === "{camera}")).toBe(
    true,
  );
  for (const token of ["{version}", "{user_text}", "{session}"])
    expect(namingTokens("ingest").some((t) => t.value === token), token).toBe(
      false,
    );
  expect(namingExample("{unknown}_{orig}", "ingest", "a.RAF")).toBe(
    "{unknown}_a.RAF",
  );
});

it("rejects malformed or unsupported templates before writing files", async () => {
  const { validNamingTemplate } = await import("./namingTemplates");
  for (const value of [
    "",
    "{unknown}",
    "{orig",
    "a}",
    "../{orig}",
    "{seq:99}",
    "{seq:}",
    "{orig|oops}",
    "{seq:2:4}",
  ]) {
    expect(validNamingTemplate(value, "ingest"), value).toBe(false);
  }
  expect(
    validNamingTemplate("{orig_base|upper}_{seq:4}.{ext|lower}", "ingest"),
  ).toBe(true);
  expect(
    validNamingTemplate("{YYYY}_{original_filename}_{sequence}", "export"),
  ).toBe(true);
  expect(validNamingTemplate("{seq:4}", "export")).toBe(false);
});

it('moves repeated tokens by position and inserts new tokens without losing text', async () => {
  const { dropNamingBlock } = await import('./namingTemplates');
  const parts = ['{YYYY}', '-', '{YYYY}', '_', '{original_filename}'];
  expect(dropNamingBlock(parts, 5, '', 0)).toBe('-{YYYY}_{original_filename}{YYYY}');
  expect(dropNamingBlock(parts, 0, '', 4)).toBe('{original_filename}{YYYY}-{YYYY}_');
  expect(dropNamingBlock(parts, 2, '{sequence}')).toBe('{YYYY}-{sequence}{YYYY}_{original_filename}');
  expect(dropNamingBlock([], 0, '{sequence}')).toBe('{sequence}');
  expect(dropNamingBlock(parts, 2, '', 2)).toBe(parts.join(''));
});
it("accepts every offered ingest token and keeps the Rust token list", async () => {
  const { validNamingTemplate, INGEST_TOKEN_NAMES } = await import(
    "./namingTemplates"
  );
  for (const token of namingTokens("ingest"))
    expect(validNamingTemplate(`${token.value}.x`, "ingest"), token.value).toBe(
      true,
    );
  for (const name of INGEST_TOKEN_NAMES)
    expect(validNamingTemplate(`{${name}}`, "ingest"), name).toBe(true);
  expect(
    namingExample(
      "{camera}_{orig_num}_ISO{iso}_{shutter}.{ext}",
      "ingest",
      "DSC_0042.ARW",
    ),
  ).toBe("Camera Model_0042_ISO400_1-250.ARW");
});
it('previews zero and a counter crossing a padding boundary', () => {
  expect(namingExample('{sequence}', 'export', 'a.RAF', 'jpg', '', 98, 4)).toBe('098.jpg');
  expect(namingExample('{seq:4}.{ext}', 'ingest', 'a.RAF', '', '', 0, 4)).toBe('0000.RAF');
});
