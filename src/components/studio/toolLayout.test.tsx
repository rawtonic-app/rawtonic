import "../../i18n";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { act, cleanup, fireEvent, render, screen } from "@testing-library/react";
import { ToolGroup } from "./StudioDevelop";
import { ToolColumn, keepAbsentTools, mergeToolOrder } from "./toolLayout";
import { useSettingsStore } from "../../store/useSettingsStore";
import { ContextMenuProvider } from "../../context/ContextMenuContext";
import type { AppSettings } from "../ui/AppProperties";

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn(async () => undefined) }));

const titles = () => [...document.querySelectorAll("summary > span")].map((el) => el.textContent);

function Column() {
  return (
    <ContextMenuProvider>
      <ToolColumn scope="develop:all">
        <ToolGroup key="a" title="Alpha">
          a
        </ToolGroup>
        <ToolGroup key="b" title="Beta" open={false}>
          b
        </ToolGroup>
        <ToolGroup key="c" title="Gamma">
          c
        </ToolGroup>
      </ToolColumn>
    </ContextMenuProvider>
  );
}

beforeEach(() => {
  useSettingsStore.setState({ appSettings: {} as AppSettings });
});
afterEach(cleanup);

describe("tool order", () => {
  it("keeps the user's order and places unknown tools after their default neighbour", () => {
    expect(mergeToolOrder(["a", "b", "c", "d"], ["c", "a"])).toEqual(["c", "d", "a", "b"]);
    expect(mergeToolOrder(["a", "b"], undefined)).toEqual(["a", "b"]);
    expect(mergeToolOrder(["x", "a"], ["a"])).toEqual(["x", "a"]);
  });

  it("does not forget the place of tools that are hidden right now", () => {
    expect(keepAbsentTools(["b", "a"], ["c", "a", "e", "b"])).toEqual(["c", "b", "a", "e"]);
  });

  it("orders tools from the settings and restores the default order", () => {
    useSettingsStore.setState({
      appSettings: { toolLayout: { order: { "develop:all": ["c", "a", "b"] } } } as unknown as AppSettings,
    });
    render(<Column />);
    expect(titles()).toEqual(["Gamma", "Alpha", "Beta"]);
    fireEvent.contextMenu(screen.getByText("Alpha"));
    fireEvent.click(screen.getByText("Restore Default Tool Order"));
    expect(useSettingsStore.getState().appSettings?.toolLayout?.order).toEqual({});
    expect(titles()).toEqual(["Alpha", "Beta", "Gamma"]);
  });

  it("moves a tool from its header menu", () => {
    render(<Column />);
    fireEvent.contextMenu(screen.getByText("Alpha"));
    fireEvent.click(screen.getByText("Move Down"));
    expect(titles()).toEqual(["Beta", "Alpha", "Gamma"]);
    expect(useSettingsStore.getState().appSettings?.toolLayout?.order?.["develop:all"]).toEqual([
      "b",
      "a",
      "c",
    ]);
  });
});

describe("open tools", () => {
  it("remembers a closed tool per column across remounts", () => {
    const first = render(<Column />);
    const alpha = screen.getByText("Alpha").closest("details")!;
    expect(alpha.open).toBe(true);
    alpha.open = false;
    fireEvent(alpha, new Event("toggle"));
    expect(useSettingsStore.getState().appSettings?.toolLayout?.open).toEqual({
      "develop:all/Alpha": false,
    });
    first.unmount();
    render(<Column />);
    expect(screen.getByText("Alpha").closest("details")!.open).toBe(false);
    // Defaults still apply to tools the user never opened or closed.
    expect(screen.getByText("Beta").closest("details")!.open).toBe(false);
    expect(screen.getByText("Gamma").closest("details")!.open).toBe(true);
  });
});

describe("dragging tools", () => {
  it("shows the new order while dragging and offers the default order back", async () => {
    vi.stubGlobal("PointerEvent", MouseEvent);
    render(<Column />);
    const pointer = (type: string, y: number) => {
      const event = new MouseEvent(type, { clientX: 5, clientY: y, bubbles: true });
      Object.defineProperty(event, "pointerId", { value: 1 });
      return event;
    };
    const summary = screen.getByText("Alpha").closest("summary")!;
    act(() => {
      summary.dispatchEvent(pointer("pointerdown", 5));
    });
    // jsdom has no layout: every tool's middle is at 0, so the pointer is below all.
    act(() => {
      document.dispatchEvent(pointer("pointermove", 40));
    });
    expect(titles()).toEqual(["Beta", "Gamma", "Alpha"]);
    expect(document.querySelector(".studio-tool-slot.is-dragged")?.textContent).toContain("Alpha");
    expect(document.querySelector(".studio-drag-ghost")?.textContent).toBe("Alpha");
    act(() => {
      document.dispatchEvent(pointer("pointerup", 40));
    });
    expect(useSettingsStore.getState().appSettings?.toolLayout?.order?.["develop:all"]).toEqual(["b", "c", "a"]);
    // The click that ends a drag is swallowed; a later one restores.
    await new Promise((resolve) => setTimeout(resolve, 0));
    fireEvent.click(screen.getByText("Restore default tool order"));
    expect(titles()).toEqual(["Alpha", "Beta", "Gamma"]);
    expect(screen.queryByText("Restore default tool order")).toBeNull();
    vi.unstubAllGlobals();
  });
});
