import "../../i18n";
import { afterEach, expect, it, vi } from "vitest";
import { cleanup, fireEvent, render, screen } from "@testing-library/react";
import StudioPeopleMask from "./StudioPeopleMask";
import type { SubMask } from "../panel/right/Masks";
import type { PeoplePart } from "../../utils/maskKinds";

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn(async () => []) }));

afterEach(cleanup);

/** The parameters each part checkbox click sets, starting from `parts`. */
function click(parts: PeoplePart[], label: RegExp) {
  const setParameters = vi.fn();
  const sub = { id: "p", type: "ai-people", visible: true, mode: "additive", parameters: { parts, person: null, maskDataBase64: "data:old" } } as unknown as SubMask;
  render(<StudioPeopleMask sub={sub} setParameters={setParameters} onInteraction={() => {}} />);
  fireEvent.click(screen.getByRole("checkbox", { name: label }));
  return setParameters.mock.calls[0][0] as { parts: PeoplePart[]; maskDataBase64: string | null };
}

it("unticking the last part leaves no bitmap to show", () => {
  expect(click(["lips"], /^Lips$/)).toEqual({ parts: [], maskDataBase64: null });
});

// The entire person holds every part: a part ticked beside it would change
// nothing on the photo, so a part replaces it and it replaces the parts.
it("a part replaces the entire person and the entire person replaces the parts", () => {
  expect(click(["entirePerson"], /^Lips$/).parts).toEqual(["lips"]);
  cleanup();
  expect(click(["lips", "teeth"], /^Entire person$/).parts).toEqual(["entirePerson"]);
  cleanup();
  expect(click(["lips"], /^Teeth$/).parts).toEqual(["lips", "teeth"]);
});
