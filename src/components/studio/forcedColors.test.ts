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
