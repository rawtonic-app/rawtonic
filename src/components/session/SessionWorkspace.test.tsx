import "../../i18n";
import { beforeEach, describe, expect, it, vi } from "vitest";
import { render, screen, waitFor, cleanup, act } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import SessionWorkspace, { countHotFolderCapture, flattenPresets, parseKeywords, restoreLastSession, SLOW_RESTORE_MS, type SessionSummary } from "./SessionWorkspace";
import { useLibraryStore } from "../../store/useLibraryStore";
import { invoke } from "@tauri-apps/api/core";
import { toast } from "react-toastify";
import { useUIStore } from "../../store/useUIStore";
import { ContextMenuProvider } from "../../context/ContextMenuContext";

vi.mock("@tauri-apps/api/core", () => ({
  isTauri: () => false,
  invoke: vi.fn(),
}));
vi.mock("@tauri-apps/plugin-dialog", () => ({
  open: vi.fn(async () => "/jobs"),
}));
vi.mock("@tauri-apps/api/path", () => ({
  join: async (...parts: string[]) => parts.join("/"),
}));
vi.mock("@tauri-apps/plugin-os", () => ({ platform: () => "macos" }));
vi.mock("@tauri-apps/api/event", () => ({ listen: async () => () => {} }));
vi.mock("react-toastify", () => ({
  toast: { error: vi.fn(), success: vi.fn(), info: vi.fn(), dismiss: vi.fn() },
}));

const summary: SessionSummary = {
  directory: "/jobs/Acme",
  manifest: {
    name: "Acme",
    mode: "session",
    capture: "Capture",
    output: "Output",
  },
  assets: 0,
  picked: 0,
  rated: 0,
  xmp_pending: 0,
};
beforeEach(() => {
  cleanup();
  vi.clearAllMocks();
  useUIStore.setState({ activeView: "library" });
  useLibraryStore.setState({
    rootPaths: [],
    imageList: [],
    multiSelectedPaths: [],
    imageFlags: {},
    sessionDirectory: null,
  });
  HTMLDialogElement.prototype.showModal = function () {
    this.setAttribute("open", "");
  };
  HTMLDialogElement.prototype.close = function () {
    this.removeAttribute("open");
  };
  vi.mocked(invoke).mockImplementation(async (command) =>
    command === "session_create"
      ? summary
      : command === "session_summary"
        ? summary
        : command === "session_flags" || command === "session_set_flag"
          ? {}
          : { path: "/jobs/Acme", children: [] },
  );
});

describe("Session workspace keyboard flows", () => {
  it.each(["session", "catalog"] as const)("creates a %s from its welcome choice using the typed name and chosen folder", async (mode) => {
    const user = userEvent.setup();
    const navigate = vi.fn(async () => {});
    const label = mode === "session" ? "project" : "catalog";
    const directory = `/jobs/Acme${mode === "catalog" ? ".catalog" : ""}`;
    const created = { ...summary, directory, manifest: { ...summary.manifest, mode } };
    const base = vi.mocked(invoke).getMockImplementation()!;
    vi.mocked(invoke).mockImplementation(async (command, args) =>
      command === "session_create" || command === "session_summary" ? created : base(command, args),
    );
    render(<SessionWorkspace onNavigate={navigate} />);
    await user.click(screen.getByRole("button", { name: `Create a ${label}` }));
    expect(screen.getAllByRole("radio")).toHaveLength(2);
    const nameField = screen.getByLabelText(mode === "catalog" ? "Catalog name" : "Project name");
    expect(document.activeElement).toBe(nameField);
    await user.type(nameField, "Acme");
    await user.click(screen.getByRole("button", { name: "Location" }));
    await user.click(screen.getByRole("button", { name: `Create ${label}` }));
    await waitFor(() =>
      expect(invoke).toHaveBeenCalledWith("session_create", {
        directory,
        name: "Acme",
        catalog: mode === "catalog",
      }),
    );
    await waitFor(() =>
      expect(navigate).toHaveBeenCalledWith(`${directory}/Capture`, true),
    );
    expect(screen.getByText("Acme")).toBeTruthy();
  });
  it("surfaces a failed create without navigating or dismissing the entered name", async () => {
    const user = userEvent.setup();
    const navigate = vi.fn(async () => {});
    vi.mocked(invoke).mockRejectedValue(
      new Error("A library already exists here"),
    );
    render(<SessionWorkspace onNavigate={navigate} />);
    await user.click(screen.getByRole("button", { name: "Create a project" }));
    await user.type(screen.getByLabelText("Project name"), "Acme");
    await user.click(screen.getByRole("button", { name: "Location" }));
    await user.click(screen.getByRole("button", { name: "Create project" }));
    await waitFor(() =>
      expect(screen.getByRole("alert").textContent).toContain("already exists"),
    );
    expect(navigate).not.toHaveBeenCalled();
    expect(
      (screen.getByLabelText("Project name") as HTMLInputElement).value,
    ).toBe("Acme");
  });
  it("does not treat letters in the session name as culling commands", async () => {
    const user = userEvent.setup();
    render(<SessionWorkspace onNavigate={vi.fn(async () => {})} />);
    await user.click(screen.getByRole("button", { name: "Create a project" }));
    await user.type(screen.getByLabelText("Project name"), "Pixel portraits");
    expect(
      (screen.getByLabelText("Project name") as HTMLInputElement).value,
    ).toBe("Pixel portraits");
    expect(invoke).not.toHaveBeenCalledWith(
      "session_set_flag",
      expect.anything(),
    );
  });
});

it("owns P in a managed session instead of also invoking the legacy preset shortcut", async () => {
  const user = userEvent.setup();
  const navigate = vi.fn(async () => {});
  render(<SessionWorkspace onNavigate={navigate} />);
  await user.click(screen.getByRole("button", { name: "Create a project" }));
  await user.type(screen.getByLabelText("Project name"), "Acme");
  await user.click(screen.getByRole("button", { name: "Location" }));
  await user.click(screen.getByRole("button", { name: "Create project" }));
  await waitFor(() => expect(navigate).toHaveBeenCalled());
  useLibraryStore.setState({
    multiSelectedPaths: ["/jobs/Acme/Capture/test.jpg"],
  });
  const legacyShortcut = vi.fn();
  window.addEventListener("keydown", legacyShortcut);
  await user.keyboard("p");
  await waitFor(() =>
    expect(invoke).toHaveBeenCalledWith("session_set_flag", {
      directory: "/jobs/Acme",
      paths: ["/jobs/Acme/Capture/test.jpg"],
      flag: 1,
    }),
  );
  expect(legacyShortcut).not.toHaveBeenCalled();
  window.removeEventListener("keydown", legacyShortcut);
});

it("rejects with ⇧X and leaves plain X to the proof margin", async () => {
  const user = userEvent.setup();
  const navigate = vi.fn(async () => {});
  render(<SessionWorkspace onNavigate={navigate} />);
  await user.click(screen.getByRole("button", { name: "Create a project" }));
  await user.type(screen.getByLabelText("Project name"), "Acme");
  await user.click(screen.getByRole("button", { name: "Location" }));
  await user.click(screen.getByRole("button", { name: "Create project" }));
  await waitFor(() => expect(navigate).toHaveBeenCalled());
  useLibraryStore.setState({
    multiSelectedPaths: ["/jobs/Acme/Capture/test.jpg"],
  });
  const later = vi.fn();
  window.addEventListener("keydown", later);
  await user.keyboard("x");
  expect(later).toHaveBeenCalledTimes(1);
  expect(invoke).not.toHaveBeenCalledWith("session_set_flag", expect.anything());
  await user.keyboard("{Shift>}X{/Shift}");
  await waitFor(() =>
    expect(invoke).toHaveBeenCalledWith("session_set_flag", {
      directory: "/jobs/Acme",
      paths: ["/jobs/Acme/Capture/test.jpg"],
      flag: -1,
    }),
  );
  window.removeEventListener("keydown", later);
});

it("contains modal shortcuts and closes with Escape without invoking editor commands", async () => {
  const user = userEvent.setup();
  render(<SessionWorkspace onNavigate={vi.fn(async () => {})} />);
  await user.click(screen.getByRole("button", { name: "Create a project" }));
  expect(document.activeElement).toBe(screen.getByLabelText("Project name"));
  const editorKey = vi.fn();
  window.addEventListener("keydown", editorKey);
  await user.keyboard("{Escape}");
  expect(document.querySelector("dialog[open]")).toBeNull();
  expect(editorKey).not.toHaveBeenCalled();
  window.removeEventListener("keydown", editorKey);
});

async function createSession() {
  const user = userEvent.setup();
  const navigate = vi.fn(async () => {});
  render(<SessionWorkspace onNavigate={navigate} />);
  await user.click(screen.getByRole("button", { name: "Create a project" }));
  await user.type(screen.getByLabelText("Project name"), "Acme");
  await user.click(screen.getByRole("button", { name: "Location" }));
  await user.click(screen.getByRole("button", { name: "Create project" }));
  await waitFor(() => expect(navigate).toHaveBeenCalled());
  return user;
}

it("restores persisted flags and serializes rapid P/X/U writes without speculative state", async () => {
  const path = "/jobs/Acme/Capture/test.jpg";
  const originalMock = vi.mocked(invoke).getMockImplementation()!;
  const pending: Array<(value: unknown) => void> = [];
  vi.mocked(invoke).mockImplementation((command, args) => {
    if (command === "session_flags") return Promise.resolve({ [path]: 1 });
    if (command === "session_set_flag") return new Promise((resolve) => pending.push(resolve));
    return originalMock(command, args);
  });
  const user = await createSession();
  expect(useLibraryStore.getState().imageFlags[path]).toBe(1);
  act(() => useLibraryStore.setState({ multiSelectedPaths: [path] }));
  await user.keyboard("{Shift>}X{/Shift}pu");
  expect(pending).toHaveLength(1);
  expect(useLibraryStore.getState().imageFlags[path]).toBe(1);
  await act(async () => pending[0]({ [path]: -1 }));
  expect(useLibraryStore.getState().imageFlags[path]).toBe(-1);
  await waitFor(() => expect(pending).toHaveLength(2));
  await act(async () => pending[1]({ [path]: 1 }));
  await waitFor(() => expect(pending).toHaveLength(3));
  await act(async () => pending[2]({ [path]: 0 }));
  expect(useLibraryStore.getState().imageFlags[path]).toBe(0);
  expect(vi.mocked(invoke).mock.calls.filter(([command]) => command === "session_set_flag").map(([, args]) => (args as { flag: number }).flag)).toEqual([-1, 1, 0]);
});

it("preserves visible flags on failure and ignores acknowledgements from a previous session", async () => {
  const user = await createSession();
  const path = "/jobs/Acme/Capture/test.jpg";
  act(() => useLibraryStore.setState({ multiSelectedPaths: [path], imageFlags: { [path]: 1 } }));
  vi.mocked(invoke).mockRejectedValueOnce(new Error("Disk unavailable"));
  await user.keyboard("{Shift>}X{/Shift}");
  await waitFor(() => expect(toast.error).toHaveBeenCalledWith(expect.stringContaining("Disk unavailable")));
  expect(useLibraryStore.getState().imageFlags[path]).toBe(1);
  let finish!: (value: unknown) => void;
  vi.mocked(invoke).mockImplementationOnce(() => new Promise((resolve) => { finish = resolve; }));
  await user.keyboard("{Shift>}X{/Shift}");
  act(() => useLibraryStore.setState({ sessionDirectory: "/jobs/Another", imageFlags: {} }));
  await act(async () => finish({ [path]: -1 }));
  expect(useLibraryStore.getState().imageFlags).toEqual({});
});

describe("restoring the last session", () => {
  it("says what it is opening when the volume takes long (macOS permission dialog)", async () => {
    vi.useFakeTimers();
    try {
      let finish: (value: SessionSummary) => void = () => {};
      vi.mocked(invoke).mockImplementation(async (command) =>
        command === "session_last"
          ? new Promise<SessionSummary>((resolve) => (finish = resolve))
          : "/Volumes/External/QA/startup-5000",
      );
      const navigate = vi.fn(async () => {});
      const restored = restoreLastSession(navigate, vi.fn());
      await act(async () => {
        vi.advanceTimersByTime(SLOW_RESTORE_MS + 10);
      });
      expect(toast.info).toHaveBeenCalledWith(
        expect.stringContaining("Opening startup-5000 (/Volumes/External/QA/startup-5000)"),
        expect.objectContaining({ toastId: "session-restore", autoClose: false }),
      );
      finish(summary);
      await act(async () => {
        await restored;
      });
      expect(toast.dismiss).toHaveBeenCalledWith("session-restore");
      expect(navigate).toHaveBeenCalledWith(summary);
    } finally {
      vi.useRealTimers();
    }
  });

  it("stays quiet when the session opens quickly", async () => {
    vi.useFakeTimers();
    try {
      vi.mocked(toast.info).mockClear();
      vi.mocked(invoke).mockImplementation(async () => summary);
      await act(async () => {
        await restoreLastSession(vi.fn(async () => {}), vi.fn());
        vi.advanceTimersByTime(SLOW_RESTORE_MS * 2);
      });
      expect(toast.info).not.toHaveBeenCalled();
    } finally {
      vi.useRealTimers();
    }
  });
});

it("flattens preset folders for the next-capture choice", () => {
  expect(
    flattenPresets([
      { preset: { id: "a", name: "Studio", adjustments: { contrast: 10 } } },
      { folder: { name: "Film", children: [{ id: "b", name: "Warm", adjustments: {} }] } },
    ]).map((p) => p.name),
  ).toEqual(["Studio", "Film / Warm"]);
});

it("starts a hot folder for the session with a preset as next capture adjustments", async () => {
  const user = userEvent.setup();
  const base = vi.mocked(invoke).getMockImplementation()!;
  vi.mocked(invoke).mockImplementation(async (command, args) =>
    command === "load_presets"
      ? [{ preset: { id: "p1", name: "Studio", adjustments: { contrast: 12 } } }]
      : command === "hot_folder_start"
        ? { folder: "/tether", session: "/jobs/Acme", imported: 0, nextCapture: { mode: "preset" } }
        : base(command, args),
  );
  render(<SessionWorkspace onNavigate={vi.fn(async () => {})} />);
  await user.click(screen.getByRole("button", { name: "Create a project" }));
  await user.type(screen.getByLabelText("Project name"), "Acme");
  await user.click(screen.getByRole("button", { name: "Location" }));
  await user.click(screen.getByRole("button", { name: "Create project" }));
  await waitFor(() => expect(screen.getByRole("button", { name: /Hot folder/ })).toHaveProperty("disabled", false));
  await user.click(screen.getByRole("button", { name: /Hot folder/ }));
  await user.type(screen.getByLabelText("Watched folder"), "/tether");
  await waitFor(() => expect(screen.getByRole("option", { name: "Preset: Studio" })).toBeTruthy());
  await user.selectOptions(screen.getByLabelText("Next capture adjustments"), "preset:p1");
  await user.click(screen.getByLabelText("Include subfolders"));
  await user.selectOptions(screen.getByLabelText("Next capture rating"), "3");
  await user.type(screen.getByLabelText("Next capture keywords"), "studio, Acme, studio");
  await user.click(screen.getByRole("button", { name: "Start watching" }));
  await waitFor(() =>
    expect(invoke).toHaveBeenCalledWith("hot_folder_start", {
      directory: "/jobs/Acme",
      folder: "/tether",
      template: "{orig}",
      nextCapture: { mode: "preset", name: "Studio", adjustments: { contrast: 12 } },
      metadata: { rating: 3, keywords: ["studio", "Acme"] },
      includeSubfolders: true,
    }),
  );
  expect(screen.getByRole("button", { name: /Hot folder · 0/ })).toBeTruthy();
});

it("resumes the hot folder a library keeps when it opens, and shows a missing folder", async () => {
  const user = userEvent.setup();
  const base = vi.mocked(invoke).getMockImplementation()!;
  vi.mocked(invoke).mockImplementation(async (command, args) =>
    command === "hot_folder_resume"
      ? {
          folder: "/Volumes/Tether/Hot",
          session: "/jobs/Acme",
          imported: 0,
          nextCapture: { mode: "defaults" },
          metadata: { rating: 2, keywords: ["studio"] },
          includeSubfolders: true,
          missing: true,
        }
      : command === "hot_folder_stop"
        ? null
        : base(command, args),
  );
  render(<SessionWorkspace onNavigate={vi.fn(async () => {})} />);
  await user.click(screen.getByRole("button", { name: "Create a project" }));
  await user.type(screen.getByLabelText("Project name"), "Acme");
  await user.click(screen.getByRole("button", { name: "Location" }));
  await user.click(screen.getByRole("button", { name: "Create project" }));
  await waitFor(() => expect(invoke).toHaveBeenCalledWith("hot_folder_resume", { directory: "/jobs/Acme" }));
  const button = await screen.findByRole("button", { name: /folder missing/ });
  expect(toast.info).toHaveBeenCalledWith(expect.stringContaining("Hot not found"));
  // The dialog shows what is watched; Stop forgets it for this library.
  await user.click(button);
  expect((screen.getByLabelText("Watched folder") as HTMLInputElement).value).toBe("/Volumes/Tether/Hot");
  expect((screen.getByLabelText("Include subfolders") as HTMLInputElement).checked).toBe(true);
  expect((screen.getByLabelText("Next capture keywords") as HTMLInputElement).value).toBe("studio");
  await user.click(screen.getByRole("button", { name: "Stop watching" }));
  await waitFor(() => expect(invoke).toHaveBeenCalledWith("hot_folder_stop", { directory: "/jobs/Acme" }));
  expect(screen.getByRole("button", { name: "Hot folder" })).toBeTruthy();
  expect(vi.mocked(invoke).mock.calls.filter(([c]) => c === "hot_folder_resume")).toHaveLength(1);
});

it("goes on counting a resumed hot folder's captures from the count the library keeps", async () => {
  const resumed = {
    folder: "/Volumes/Tether/Hot",
    session: "/jobs/Acme",
    imported: 12,
    nextCapture: { mode: "defaults" as const },
  };
  const base = vi.mocked(invoke).getMockImplementation()!;
  vi.mocked(invoke).mockImplementation(async (command, args) =>
    command === "hot_folder_resume" ? resumed : base(command, args),
  );
  const user = userEvent.setup();
  render(<SessionWorkspace onNavigate={vi.fn(async () => {})} />);
  await user.click(screen.getByRole("button", { name: "Create a project" }));
  await user.type(screen.getByLabelText("Project name"), "Acme");
  await user.click(screen.getByRole("button", { name: "Location" }));
  await user.click(screen.getByRole("button", { name: "Create project" }));
  // After a restart the button shows the captures from before it, not 0.
  expect(await screen.findByText("Hot folder · 12")).toBeTruthy();

  const file = { source: "/Volumes/Tether/Hot/A.ARW", skipped: false, error: null };
  expect(countHotFolderCapture(resumed, file)?.imported).toBe(13);
  // A verified duplicate or a failure is not a capture.
  expect(countHotFolderCapture(resumed, { ...file, skipped: true })?.imported).toBe(12);
  expect(countHotFolderCapture(resumed, { ...file, error: "disk full" })?.imported).toBe(12);
  expect(countHotFolderCapture(null, file)).toBeNull();
});

it("reads next-capture keywords from a comma-separated list", () => {
  expect(parseKeywords(" studio, Acme;; studio ,")).toEqual(["studio", "Acme"]);
});

it("previews and imports a Lightroom catalog into the open catalog", async () => {
  const user = userEvent.setup();
  const catalog = { ...summary, manifest: { ...summary.manifest, mode: "catalog" } };
  const base = vi.mocked(invoke).getMockImplementation()!;
  vi.mocked(invoke).mockImplementation(async (command, args) =>
    command === "session_create" || command === "session_summary"
      ? catalog
      : command === "library_import_preview"
        ? {
            name: "Lightroom Catalog",
            kind: "lightroom",
            photos: 12,
            copies: 1,
            edited: 5,
            albums: 2,
            roots: [{ folder: "/Volumes/Old/2026", photos: 12, found: 0 }],
            notRead: { smartAlbums: 3, localAdjustments: 0, unlocated: 0 },
          }
        : command === "library_import"
          ? { imported: 12, withSettings: 5, missing: 0, copiesSkipped: 1, albums: 2, errors: [] }
          : base(command, args),
  );
  render(
    <ContextMenuProvider>
      <SessionWorkspace onNavigate={vi.fn(async () => {})} />
    </ContextMenuProvider>,
  );
  await user.click(screen.getByRole("button", { name: "Create a project" }));
  await user.type(screen.getByLabelText("Project name"), "Acme");
  await user.click(screen.getByRole("button", { name: "Location" }));
  await user.click(screen.getByRole("button", { name: "Create project" }));
  await waitFor(() => expect(screen.getByTitle("/jobs/Acme")).toBeTruthy());
  await user.click(screen.getByTitle("/jobs/Acme"));
  await user.click(await screen.findByRole("menuitem", { name: /Import from Lightroom/ }));
  await user.type(screen.getByLabelText("Library to import"), "/LR/Catalog.lrcat");
  await user.click(screen.getByRole("button", { name: "Check" }));
  expect(await screen.findByText(/12 photographs, 5 with edits, 2 albums/)).toBeTruthy();
  await user.type(screen.getByLabelText("New location of /Volumes/Old/2026"), "/Volumes/New/2026");
  await user.click(screen.getByRole("button", { name: /Import into Acme/ }));
  await waitFor(() =>
    expect(invoke).toHaveBeenCalledWith("library_import", {
      source: "/LR/Catalog.lrcat",
      directory: "/jobs/Acme",
      relinks: [{ from: "/Volumes/Old/2026", to: "/Volumes/New/2026" }],
    }),
  );
  expect(toast.success).toHaveBeenCalledWith(
    expect.stringContaining("12 photographs imported (5 with edits, 2 albums)"),
    expect.anything(),
  );
});

it("starts from Lightroom or Capture One on the welcome screen: a catalog first, then the import", async () => {
  const user = userEvent.setup();
  const catalog = { ...summary, manifest: { ...summary.manifest, mode: "catalog" } };
  const base = vi.mocked(invoke).getMockImplementation()!;
  vi.mocked(invoke).mockImplementation(async (command, args) =>
    command === "session_create" || command === "session_summary" ? catalog : base(command, args),
  );
  render(
    <ContextMenuProvider>
      <SessionWorkspace onNavigate={vi.fn(async () => {})} />
    </ContextMenuProvider>,
  );
  await user.click(screen.getByRole("button", { name: "Import from Lightroom or Capture One…" }));
  // The import needs a catalog: no session/catalog choice to get wrong.
  expect(screen.queryByRole("radio")).toBeNull();
  await user.type(screen.getByLabelText("Catalog name"), "Acme");
  await user.click(screen.getByRole("button", { name: "Location" }));
  await user.click(screen.getByRole("button", { name: "Create catalog" }));
  await waitFor(() =>
    expect(invoke).toHaveBeenCalledWith("session_create", {
      directory: "/jobs/Acme.catalog",
      name: "Acme",
      catalog: true,
    }),
  );
  expect(await screen.findByLabelText("Library to import")).toBeTruthy();
});
