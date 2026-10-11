import "../../i18n";
import { afterEach, expect, it, vi } from "vitest";
import { cleanup, fireEvent, render, screen } from "@testing-library/react";
import ErrorBoundary from "./ErrorBoundary";

afterEach(cleanup);

it("contains a render error to its panel and retries on request", () => {
  vi.spyOn(console, "error").mockImplementation(() => {});
  let fail = true;
  const Tool = () => {
    if (fail) throw new Error("boom");
    return <p>tool works</p>;
  };
  render(
    <>
      <ErrorBoundary name="Library tools">
        <Tool />
      </ErrorBoundary>
      <p>grid still here</p>
    </>,
  );
  expect(screen.getByRole("alert").textContent).toContain("Library tools stopped working");
  expect(screen.getByText("boom")).toBeTruthy();
  expect(screen.getByText("grid still here")).toBeTruthy();
  fail = false;
  fireEvent.click(screen.getByText("Try again"));
  expect(screen.getByText("tool works")).toBeTruthy();
  vi.restoreAllMocks();
});
