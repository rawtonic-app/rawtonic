import { describe, expect, it } from "vitest";
import { installRenderCensus, startRenderCensus, takeRenderCensus } from "./renderCensus";

// The hook goes in before react-dom loads (this file imports React only below).
installRenderCensus(globalThis);

describe("render census", () => {
  it("counts each rendered component once, memo wrappers included, and skips bailed-out ones", async () => {
    const { act, createElement: h, memo, useState } = await import("react");
    const { createRoot } = await import("react-dom/client");
    (globalThis as { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true;

    const Compared = memo(
      function Compared({ value }: { value: number; ignored: number }) {
        return h("span", null, value);
      },
      (previous, next) => previous.value === next.value,
    );
    const Simple = memo(function Simple({ value }: { value: number }) {
      return h("span", null, value);
    });
    const Plain = ({ value }: { value: number }) => h("span", null, value);
    let bump: () => void = () => {};
    function App() {
      const [count, setCount] = useState(0);
      bump = () => setCount((c) => c + 1);
      return h(
        "div",
        null,
        h(Compared, { value: 1, ignored: count }),
        h(Compared, { value: count, ignored: 0 }),
        h(Simple, { value: 1 }),
        h(Simple, { value: count }),
        h(Plain, { value: count }),
      );
    }

    const root = createRoot(document.createElement("div"));
    await act(async () => root.render(h(App)));
    startRenderCensus();
    await act(async () => bump());
    expect(takeRenderCensus()).toEqual({
      "(commits)": 1,
      App: 1,
      Compared: 1,
      Simple: 1,
      Plain: 1,
    });
    await act(async () => root.unmount());
  });
});
