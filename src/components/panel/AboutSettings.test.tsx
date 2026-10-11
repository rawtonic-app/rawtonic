import { afterEach, describe, expect, it, vi } from "vitest";
import { cleanup, fireEvent, render, screen } from "@testing-library/react";

vi.mock("@tauri-apps/api/app", () => ({
  getVersion: () => Promise.resolve("0.1.0"),
}));
vi.mock("@tauri-apps/plugin-shell", () => ({ open: vi.fn() }));
vi.mock("@tauri-apps/api/core", () => ({
  invoke: vi.fn(() =>
    Promise.resolve("react 19.2.8 (npm package)\nMIT License"),
  ),
}));

import AboutSettings, { SOURCE_URL } from "./AboutSettings";

describe("AboutSettings", () => {
  afterEach(cleanup);

  it("shows the notices the licenses require", async () => {
    render(<AboutSettings />);
    expect(await screen.findByText(/Rawtonic 0\.1\.0/)).toBeTruthy();
    expect(screen.getByText(/without any warranty/)).toBeTruthy();
    expect(screen.getByText(SOURCE_URL)).toBeTruthy();
    expect(
      screen.getByText(
        "This product includes DNG technology under license by Adobe.",
      ),
    ).toBeTruthy();
    expect(screen.getByText(/Independent JPEG Group/)).toBeTruthy();
    expect(screen.getByText(/not affiliated with/)).toBeTruthy();
    expect(screen.getByText("RapidRAW")).toBeTruthy();
    expect(screen.getByText("MI-GAN")).toBeTruthy();
  });

  it("shows the bundled third-party license texts on request", async () => {
    render(<AboutSettings />);
    fireEvent.click(
      screen.getByRole("button", { name: "Show the license texts" }),
    );
    expect(
      (await screen.findByLabelText("Third-party license texts")).textContent,
    ).toContain("MIT License");
  });
});
