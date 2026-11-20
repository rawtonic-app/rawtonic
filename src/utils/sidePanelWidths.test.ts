import { describe, expect, it } from 'vitest';
import { sidePanelWidths, steppedWidth, viewerMinWidth } from './sidePanelWidths';

describe('sidePanelWidths', () => {
  it('shows the stored widths while the photograph keeps its room', () => {
    expect(sidePanelWidths({ windowWidth: 1600, uiScale: 1, tools: 340, toolsMax: 480, dock: 230 })).toMatchObject({
      tools: 340,
      dock: 230
    });
  });

  it('narrows the dock, then the tools, so the photograph keeps its room', () => {
    // 1000 px: the photograph keeps 320, the tools 340, the dock the rest.
    const a = sidePanelWidths({ windowWidth: 1000, uiScale: 1, tools: 340, toolsMax: 390, dock: 440 });
    expect(a).toMatchObject({ tools: 340, dock: 340, dockCap: 340 });
    expect(1000 - a.tools - a.dock!).toBe(viewerMinWidth(1000));
    // 820 px at 125 % with the dock at 350: the dock at its minimum, the
    // tools in the rest (306 × 1.25 = 382.5 px); the photograph keeps 287.
    const b = sidePanelWidths({ windowWidth: 820, uiScale: 1.25, tools: 390, toolsMax: 390, dock: 350 });
    expect(b).toMatchObject({ tools: 306, dock: 150 });
    expect(820 - b.tools * 1.25 - b.dock!).toBeGreaterThanOrEqual(viewerMinWidth(820));
    // A hidden tool column leaves the room to the dock.
    expect(
      sidePanelWidths({ windowWidth: 820, uiScale: 1, tools: 390, toolsMax: 390, dock: 440, toolsInFlow: false }).dock
    ).toBe(440);
  });

  it('widening the tools takes room from the dock, not the photograph', () => {
    const before = sidePanelWidths({ windowWidth: 1000, uiScale: 1, tools: 340, toolsMax: 390, dock: 440 });
    const tools = steppedWidth(340, before.tools, 10, 270, before.toolsCap);
    const after = sidePanelWidths({ windowWidth: 1000, uiScale: 1, tools, toolsMax: 390, dock: 440 });
    expect(after.tools).toBe(350);
    expect(after.dock).toBe(330);
  });
});

describe('steppedWidth', () => {
  it('steps from the shown width and keeps the stored one when the step cannot move it', () => {
    expect(steppedWidth(480, 390, -10, 270, 390)).toBe(380);
    expect(steppedWidth(480, 390, 10, 270, 390)).toBe(480);
    expect(steppedWidth(300, 270, -10, 270, 390)).toBe(300);
  });
});
