// @ts-expect-error node:fs is untyped in the frontend tsconfig
import { readFileSync } from "node:fs";
import { expect, it } from "vitest";

it("preserves dialog borders, native checked indicators and focus in forced colours", () => {
  const css = readFileSync("src/components/studio/studio.css", "utf8") as string;
  const forced = css.slice(css.indexOf("@media (forced-colors: active)"));
  const rule = (selector: string) => {
    const blocks = [...forced.matchAll(/([^{}]+)\{([^{}]*)\}/g)];
    return blocks.find(([, selectors]) => selectors.split(",").some((s) => s.trim() === selector))?.[2] ?? "";
  };
  expect(forced).toContain("@media (forced-colors: active)");
  expect(forced).toMatch(/@layer base\s*\{[^}]*outline: 2px solid Highlight !important/s);
  expect(rule("button.studio-button.is-default:focus")).toContain("outline: 2px solid Highlight !important");
  expect(rule(".studio-dialog")).toContain("border-color: CanvasText");
  expect(rule("button.studio-button")).toContain("border: 1px solid ButtonText");
  for (const type of ["checkbox", "radio"]) {
    expect(rule(`input[type='${type}']`)).toContain("appearance: auto");
    expect(rule(`input[type='${type}']:checked`)).toContain("border-color: Highlight");
    expect(rule(`input[type='${type}']:focus-visible`)).toContain("outline: 2px solid Highlight !important");
  }
  expect(rule(".studio-dialog :focus-visible")).toContain("outline: 2px solid Highlight !important");
  expect(rule("button.studio-button:focus-visible")).toContain("outline: 2px solid Highlight !important");
  expect(rule(".studio-clip-corner .studio-clip-corner-mark")).toContain("stroke: ButtonText");
  expect(rule(".studio-clip-corner.is-on .studio-clip-corner-mark")).toContain("fill: ButtonText");
  expect(rule(".studio-clip-corner.is-lit .studio-clip-corner-mark")).toContain("fill: Highlight");
  expect(rule(".studio-clip-corner.is-on.is-lit .studio-clip-corner-mark")).toContain("stroke: ButtonText");
  expect(rule(".studio-clip-corner:focus-visible")).toContain("outline-color: Highlight !important");
});

it("gives the Export dialog's footer buttons a border and a focus outline in forced colours", () => {
  const css = readFileSync("src/components/studio/studio.css", "utf8") as string;
  const forced = css.slice(css.indexOf("@media (forced-colors: active)"));
  // The rules of one (top-level or @layer) block; comments in the selector
  // list are ignored.
  const rule = (selector: string) => {
    const blocks = [...forced.matchAll(/([^{}]+)\{([^{}]*)\}/g)];
    return blocks.find(([, selectors]) =>
      selectors.replace(/\/\*[\s\S]*?\*\//g, " ").split(",").some((s) => s.trim() === selector))?.[2] ?? "";
  };
  // Cancel and Export (StudioExport's footer) are plain buttons, which the
  // studio's button reset gives no border: a contrast theme kept that and
  // drew them without an edge (WQ4 of the Windows qualification).
  expect(rule(".studio-export-form footer button")).toContain("border: 1px solid ButtonText");
  expect(rule(".studio-export-form footer button:focus-visible")).toContain("outline: 2px solid Highlight !important");
  // System colours only: no palette colours left in these rules.
  expect(rule(".studio-export-form footer button")).not.toMatch(/#|rgba?\(/);
  expect(rule(".studio-export-form footer button:focus-visible")).not.toMatch(/#|rgba?\(/);
});
