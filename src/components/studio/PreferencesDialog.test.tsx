import "../../i18n";
import { afterEach, expect, it } from "vitest";
import { cleanup, render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { useState } from "react";
import PreferencesDialog from "./PreferencesDialog";

afterEach(cleanup);

function Workspace() {
  const [open, setOpen] = useState(false);
  return (
    <>
      <header>
        <button onClick={() => setOpen(true)}>{"Preferences"}</button>
        <button>{"Ingest"}</button>
      </header>
      <main>
        <button>{"Library"}</button>
        {open && (
          <PreferencesDialog>
            <button>{"General"}</button>
            <input aria-label="Name" />
            <button disabled>{"Disabled"}</button>
            <button onClick={() => setOpen(false)}>{"Close"}</button>
          </PreferencesDialog>
        )}
      </main>
    </>
  );
}

it("moves the focus in, keeps Tab inside, makes the workspace inert and returns the focus", async () => {
  const user = userEvent.setup();
  render(<Workspace />);
  const opener = screen.getByRole("button", { name: "Preferences" });
  await user.click(opener);

  const dialog = screen.getByRole("dialog", { name: "Preferences" });
  expect(document.activeElement).toBe(screen.getByRole("button", { name: "General" }));

  // The workspace behind is inert, the dialog and its ancestors are not.
  expect(document.querySelector("header")!.hasAttribute("inert")).toBe(true);
  expect(screen.getByText("Library").hasAttribute("inert")).toBe(true);
  expect(dialog.closest("[inert]")).toBeNull();

  await user.tab();
  expect(document.activeElement).toBe(screen.getByRole("textbox", { name: "Name" }));
  await user.tab();
  expect(document.activeElement).toBe(screen.getByRole("button", { name: "Close" }));
  // Tab from the last control wraps to the first, Shift+Tab back to the last.
  await user.tab();
  expect(document.activeElement).toBe(screen.getByRole("button", { name: "General" }));
  await user.tab({ shift: true });
  expect(document.activeElement).toBe(screen.getByRole("button", { name: "Close" }));

  await user.click(screen.getByRole("button", { name: "Close" }));
  expect(screen.queryByRole("dialog")).toBeNull();
  expect(document.querySelector("[inert]")).toBeNull();
  expect(document.activeElement).toBe(opener);
});

it("leaves elements that were inert before untouched", async () => {
  const user = userEvent.setup();
  render(
    <>
      <div data-testid="busy" inert>
        <button>{"Busy"}</button>
      </div>
      <Workspace />
    </>,
  );
  await user.click(screen.getByRole("button", { name: "Preferences" }));
  await user.click(screen.getByRole("button", { name: "Close" }));
  expect(screen.getByTestId("busy").hasAttribute("inert")).toBe(true);
});
