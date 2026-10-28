import { cleanup, fireEvent, render } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn() }));
vi.mock("react-i18next", () => ({ useTranslation: () => ({ t: (key: string) => key }) }));
const tools = vi.hoisted(() => ({ activeTool: "spot", ready: true, layer: null as unknown }));
vi.mock("./useStudioTools", () => ({ useStudioTools: () => tools }));
vi.mock("./StudioSpotControls", () => ({ default: () => null }));
vi.mock("./StudioLayerControls", () => ({ default: () => null }));

import StudioToolPopover from "./StudioToolPopover";

afterEach(cleanup);

/** The photo with the spot tool's layer, and what reaches it. */
function renderPhoto() {
  const reached = vi.fn();
  const { getByTestId } = render(
    <div data-testid="spot-overlay" onPointerDown={reached}>
      <svg data-testid="photo-inside" />
    </div>,
  );
  return { reached, photo: getByTestId("photo-inside") };
}

describe("tool settings popover", () => {
  it("closes without removing when the spot tool's panel is dismissed on the photo", () => {
    tools.activeTool = "spot";
    const onClose = vi.fn();
    const { reached, photo } = renderPhoto();
    render(<StudioToolPopover x={10} y={10} onClose={onClose} />);
    fireEvent.pointerDown(photo);
    expect(onClose).toHaveBeenCalled();
    expect(reached).not.toHaveBeenCalled();
  });

  it("lets a press beside a brush panel paint at once", () => {
    tools.activeTool = "brush";
    tools.layer = { id: "l", name: "Layer" };
    const onClose = vi.fn();
    const { reached, photo } = renderPhoto();
    render(<StudioToolPopover x={10} y={10} onClose={onClose} />);
    fireEvent.pointerDown(photo);
    expect(onClose).toHaveBeenCalled();
    expect(reached).toHaveBeenCalled();
  });
});
