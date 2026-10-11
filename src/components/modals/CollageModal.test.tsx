import { expect, it } from "vitest";
import { collagePlacements } from "./CollageModal";

const images = [
  { path: "/a.RAF", width: 3000, height: 2000 },
  { path: "/b.RAF", width: 2000, height: 3000 },
];
const halves = [
  { x: 0, y: 0, width: 0.5, height: 1 },
  { x: 0.5, y: 0, width: 0.5, height: 1 },
];

it("splits the spacing between neighbouring cells and keeps it at the outer edge", () => {
  const [a, b] = collagePlacements(images, halves, {}, 1000, 500, 20, 1, false);
  expect(a.cell).toEqual([20, 20, 470, 460]);
  expect(b.cell).toEqual([510, 20, 470, 460]);
});

it("centres a filling photo and scales offsets with the output", () => {
  const states = { "/a.RAF": { offsetX: -10, offsetY: 0, scale: 1 } };
  const [preview] = collagePlacements(
    images,
    halves,
    states,
    1000,
    500,
    20,
    1,
    false,
  );
  const [output] = collagePlacements(
    images,
    halves,
    states,
    4000,
    2000,
    80,
    4,
    false,
  );
  expect(preview.draw[3]).toBeCloseTo(460);
  expect(preview.draw[2]).toBeCloseTo(690);
  // Centred in the 470 px cell at x 20, moved 10 px left.
  expect(preview.draw[0]).toBeCloseTo(20 + (470 - 690) / 2 - 10);
  // The saved collage is the preview scaled 4×.
  output.draw.forEach((v, i) => expect(v).toBeCloseTo(preview.draw[i] * 4));
});

it("fits the whole photo when the original ratio is kept", () => {
  const [, b] = collagePlacements(images, halves, {}, 1000, 500, 20, 1, true);
  expect(b.draw[3]).toBeCloseTo(460);
  expect(b.draw[2]).toBeCloseTo((460 * 2) / 3);
});
