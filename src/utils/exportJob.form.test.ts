import { beforeEach, expect, it } from "vitest";
import { readFormDraft, writeFormDraft } from "./exportJob";
import { DEFAULT_RECIPE_DRAFT } from "./exportRecipes";

beforeEach(() => localStorage.clear());

it("brings back a chosen watermark file and font, and a stored folder", () => {
  const draft = {
    ...DEFAULT_RECIPE_DRAFT,
    watermark: true,
    watermarkPath: "/marks/logo.png",
    watermarkFont: "Inter",
    folder: "/out",
  };
  writeFormDraft("/Shoots/A", draft);
  expect(readFormDraft("/Shoots/A")).toEqual(draft);
});

it("ignores unknown keys, wrong types and other libraries", () => {
  localStorage.setItem("rawtonic.export.form:/Shoots/A", JSON.stringify({ quality: "high", extra: 1, format: "png" }));
  expect(readFormDraft("/Shoots/A")).toEqual({ ...DEFAULT_RECIPE_DRAFT, format: "png" });
  expect(readFormDraft("/Shoots/B")).toBeNull();
});
