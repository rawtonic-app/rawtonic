import "../../i18n";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import { cleanup, render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { invoke } from "@tauri-apps/api/core";
import FirstStartToolsSide from "./FirstStartToolsSide";
import { useSettingsStore } from "../../store/useSettingsStore";
import { useUIStore } from "../../store/useUIStore";

vi.mock("@tauri-apps/api/core", () => ({
  isTauri: () => true,
  invoke: vi.fn(),
}));
afterEach(cleanup);
beforeEach(() => {
  localStorage.clear();
  useSettingsStore.setState({ appSettings: null });
  useUIStore.setState({ studioToolLayout: "tools-left" });
  vi.mocked(invoke).mockReset().mockResolvedValue(null);
});
it("offers the current default only on first start and saves a keyboard choice to Develop's setting", async () => {
  const user = userEvent.setup();
  const view = render(<FirstStartToolsSide />);
  const left = await screen.findByRole("radio", { name: "Tools on the left" });
  expect((left as HTMLInputElement).checked).toBe(true);
  left.focus();
  await user.keyboard("{ArrowRight}");
  expect(useUIStore.getState().studioToolLayout).toBe("split-tools");
  expect(localStorage.getItem("rawtonic.studio.layout")).toBe("split-tools");
  expect(
    (
      screen.getByRole("radio", {
        name: "Tools on the right",
      }) as HTMLInputElement
    ).checked,
  ).toBe(true);
  view.unmount();
  render(<FirstStartToolsSide />);
  await waitFor(() => expect(invoke).toHaveBeenCalledTimes(1));
  expect(screen.queryByRole("group")).toBeNull();
});
it.each(["rawtonic.studio.layout", "retouch.studio.layout"])(
  "skips users with a saved %s choice",
  (key) => {
    localStorage.setItem(key, "tools-left");
    render(<FirstStartToolsSide />);
    expect(screen.queryByRole("group")).toBeNull();
    expect(invoke).not.toHaveBeenCalled();
  },
);
it("never flashes a choice before history resolves and skips an existing offline session", async () => {
  let resolve!: (value: string) => void;
  vi.mocked(invoke).mockReturnValue(
    new Promise<string>((done) => {
      resolve = done;
    }),
  );
  render(<FirstStartToolsSide />);
  expect(screen.queryByRole("group")).toBeNull();
  resolve("/offline/old-project");
  await waitFor(() =>
    expect(invoke).toHaveBeenCalledWith("session_last_directory"),
  );
  expect(screen.queryByRole("group")).toBeNull();
});
it("does not ask when history cannot be read", async () => {
  vi.mocked(invoke).mockRejectedValue(new Error("unavailable"));
  render(<FirstStartToolsSide />);
  await waitFor(() => expect(invoke).toHaveBeenCalled());
  expect(screen.queryByRole("group")).toBeNull();
});

it("also saves a click on the preselected default", async () => {
  const user = userEvent.setup();
  render(<FirstStartToolsSide />);
  await user.click(
    await screen.findByRole("radio", { name: "Tools on the left" }),
  );
  expect(localStorage.getItem("rawtonic.studio.layout")).toBe("tools-left");
});

it("skips a legacy user with saved library folders", async () => {
  useSettingsStore.setState({
    appSettings: { rootFolders: ["/old-library"] } as NonNullable<
      ReturnType<typeof useSettingsStore.getState>["appSettings"]
    >,
  });
  render(<FirstStartToolsSide />);
  await waitFor(() => expect(invoke).toHaveBeenCalled());
  expect(screen.queryByRole("group")).toBeNull();
});
