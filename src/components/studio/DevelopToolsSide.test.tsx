import "../../i18n";
import { afterEach, expect, it } from "vitest";
import { cleanup, fireEvent, render, screen } from "@testing-library/react";
import DevelopToolsSide from "./DevelopToolsSide";
import { useUIStore } from "../../store/useUIStore";

afterEach(cleanup);

it("puts Develop's tools on the left or right, the same setting as the top bar button", () => {
  useUIStore.setState({ studioToolLayout: "tools-left" });
  render(<DevelopToolsSide />);
  expect(screen.getByRole("radiogroup", { name: "Develop tools" })).toBeTruthy();
  const left = screen.getByRole("radio", { name: "Left" });
  const right = screen.getByRole("radio", { name: "Right" });
  expect(left.getAttribute("aria-checked")).toBe("true");
  fireEvent.click(right);
  expect(useUIStore.getState().studioToolLayout).toBe("split-tools");
  expect(localStorage.getItem("rawtonic.studio.layout")).toBe("split-tools");
  expect(right.getAttribute("aria-checked")).toBe("true");
  fireEvent.click(left);
  expect(useUIStore.getState().studioToolLayout).toBe("tools-left");
  expect(localStorage.getItem("rawtonic.studio.layout")).toBe("tools-left");
});
