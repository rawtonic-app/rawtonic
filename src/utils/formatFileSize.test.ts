import "../i18n";
import i18n from "i18next";
import { afterEach, expect, it } from "vitest";
import { formatFileSize } from "./formatFileSize";
afterEach(async () => { await i18n.changeLanguage("en"); });

it("uses the interface locale for fractional sizes", async () => {
  await i18n.changeLanguage("de");
  expect(formatFileSize(1_500_000)).toBe("1,5 MB");
});
it("steps through decimal units including bytes and terabytes", () => {
  expect(formatFileSize(500)).toBe("500 byte");
  expect(formatFileSize(1_500)).toBe("1.5 kB");
  expect(formatFileSize(1_500_000_000_000)).toBe("1.5 TB");
});


it("handles zero, invalid inputs and rounded unit boundaries", () => {
  expect(formatFileSize(0)).toBe("0 byte");
  expect(formatFileSize(-1)).toBe("0 byte");
  expect(formatFileSize(NaN)).toBe("0 byte");
  expect(formatFileSize(Infinity)).toBe("0 byte");
  expect(formatFileSize(999_999)).toBe("1 MB");
});
