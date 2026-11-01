import { useSettingsStore } from "../../store/useSettingsStore";
import { useActivityStore } from "../../store/useActivityStore";
import { appCommand, runAppCommand } from "../../utils/appCommands";
import type { AppSettings } from "../ui/AppProperties";
import "../../i18n";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { render, screen, waitFor, cleanup, act, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import SessionWorkspace, { countHotFolderCapture, leavesTetheredLibrary, restoreLastSession, RESTORE_FAILED_NOTICE, SLOW_RESTORE_MS, type SessionSummary } from "./SessionWorkspace";
import { flattenPresets, parseKeywords } from "./nextCapture";
import { useLibraryStore } from "../../store/useLibraryStore";
import { invoke } from "@tauri-apps/api/core";
import { notify, notify as toast } from "../../activity/notify";
import { useUIStore } from "../../store/useUIStore";
import { ContextMenuProvider } from "../../context/ContextMenuContext";
import { PHOTO_WRITES_SETTLED } from "../../utils/photoWrites";
import { debouncedSave } from "../../hooks/useEditorActions";
import type { Adjustments } from "../../utils/adjustments";

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
vi.mock("../../activity/notify", () => ({
  notify: {
    error: vi.fn(),
    success: vi.fn(),
    info: vi.fn(),
    dismiss: vi.fn(),
    // The launch's restore failure is over once a library opens.
    isActive: vi.fn(() => false),
  },
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
// The last test's tree too: a menu's hover timer firing after the test
// environment is gone threw "window is not defined" (flaky check.sh runs).
afterEach(() => cleanup());
beforeEach(() => {
  cleanup();
  vi.clearAllMocks();
  localStorage.clear();
  useSettingsStore.setState({ appSettings: null });
  useUIStore.setState({ studioToolLayout: "tools-left" });
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
        : command === "session_set_flag" ? [] : command === "session_flags"
          ? {}
          : { path: "/jobs/Acme", children: [] },
  );
});

describe("Session workspace keyboard flows", () => {
  it("suggests today's local date in the project name", async () => {
    const user = userEvent.setup();
    render(<SessionWorkspace onNavigate={vi.fn(async () => {})} />);
    await user.click(screen.getByRole("button", { name: "Create a project" }));
    const now = new Date();
    const date = [now.getFullYear(), String(now.getMonth() + 1).padStart(2, "0"), String(now.getDate()).padStart(2, "0")].join("-");
    expect((screen.getByLabelText("Project name") as HTMLInputElement).placeholder).toBe(`${date}_Client_Job`);
  });

  it("keeps the chosen tools side visible after opening and cancelling a form", async () => {
    const user = userEvent.setup();
    render(<SessionWorkspace onNavigate={vi.fn(async () => {})} />);
    await user.click(screen.getByRole("radio", { name: "Tools on the right" }));
    await user.click(screen.getByRole("button", { name: "Create a project" }));
    await user.click(screen.getByRole("button", { name: "Close" }));
    expect((screen.getByRole("radio", { name: "Tools on the right" }) as HTMLInputElement).checked).toBe(true);
    expect(localStorage.getItem("rawtonic.studio.layout")).toBe("split-tools");
  });

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
    expect(within(screen.getByRole("dialog")).getAllByRole("radio")).toHaveLength(2);
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
    expect(localStorage.getItem("rawtonic.studio.layout")).toBe("tools-left");
  });
  it("keeps offline and pending-XMP status available as compact icons", async () => {
    const user = userEvent.setup();
    const base = vi.mocked(invoke).getMockImplementation()!;
    vi.mocked(invoke).mockImplementation(async (command, args) =>
      command === "session_create" || command === "session_summary"
        ? { ...summary, missing: 3, xmp_pending: 2 } : base(command, args));
    render(<SessionWorkspace onNavigate={vi.fn(async () => {})} />);
    await user.click(screen.getByRole("button", { name: "Create a project" }));
    await user.type(screen.getByLabelText("Project name"), "Acme");
    await user.click(screen.getByRole("button", { name: "Location" }));
    await user.click(screen.getByRole("button", { name: "Create project" }));
    expect(await screen.findByRole("img", { name: "3 offline" })).toBeTruthy();
    expect(screen.getByRole("button", { name: "2 XMP not updated" })).toBeTruthy();
  });

  it("re-reads the XMP count after a rating settles and after a Develop save is written", async () => {
    const user = userEvent.setup();
    const base = vi.mocked(invoke).getMockImplementation()!;
    let pending = 0;
    let finishSave: () => void = () => {};
    vi.mocked(invoke).mockImplementation(async (command, args) => {
      if (command === "session_create" || command === "session_summary") return { ...summary, xmp_pending: pending };
      // Develop's last save is still being written when the view changes;
      // its XMP fails.
      if (command === "save_metadata_and_update_thumbnail")
        return new Promise<void>((resolve) => { finishSave = () => { pending = 2; resolve(); }; });
      return base(command, args);
    });
    render(<SessionWorkspace onNavigate={vi.fn(async () => {})} />);
    await user.click(screen.getByRole("button", { name: "Create a project" }));
    await user.type(screen.getByLabelText("Project name"), "Acme");
    await user.click(screen.getByRole("button", { name: "Location" }));
    await user.click(screen.getByRole("button", { name: "Create project" }));
    await screen.findByText("Acme");
    expect(screen.queryByRole("button", { name: /XMP not updated/ })).toBeNull();
    // A rating's XMP could not be written: its write settles.
    pending = 1;
    act(() => {
      window.dispatchEvent(new CustomEvent(PHOTO_WRITES_SETTLED, {
        detail: { id: 1, kind: "set_rating_for_paths", paths: ["/a"], failed: [] },
      }));
    });
    expect(await screen.findByRole("button", { name: "1 XMP not updated" })).toBeTruthy();
    // Opening an album leaves Develop first and flushes its save after.
    act(() => useUIStore.setState({ activeView: "editor" }));
    debouncedSave("/jobs/Acme/Capture/a.ARW", {} as Adjustments);
    act(() => useUIStore.setState({ activeView: "library" }));
    debouncedSave.flush();
    await new Promise((resolve) => setTimeout(resolve, 1200));
    // Not counted before the save is written.
    expect(screen.getByRole("button", { name: "1 XMP not updated" })).toBeTruthy();
    act(() => finishSave());
    expect(await screen.findByRole("button", { name: "2 XMP not updated" }, { timeout: 3000 })).toBeTruthy();
  });

  it("counts pending XMP files again when a batch write settles, also one that failed", async () => {
    const user = userEvent.setup();
    const base = vi.mocked(invoke).getMockImplementation()!;
    let pendingXmp = 0;
    vi.mocked(invoke).mockImplementation(async (command, args) =>
      command === "session_create" || command === "session_summary"
        ? { ...summary, xmp_pending: pendingXmp } : base(command, args));
    render(<SessionWorkspace onNavigate={vi.fn(async () => {})} />);
    await user.click(screen.getByRole("button", { name: "Create a project" }));
    await user.type(screen.getByLabelText("Project name"), "Acme");
    await user.click(screen.getByRole("button", { name: "Location" }));
    await user.click(screen.getByRole("button", { name: "Create project" }));
    await screen.findByText("Acme");
    expect(screen.queryByRole("button", { name: /XMP not updated/ })).toBeNull();
    // A paste onto closed photographs whose XMP could not be written.
    pendingXmp = 2;
    act(() => {
      window.dispatchEvent(new CustomEvent(PHOTO_WRITES_SETTLED, {
        detail: { id: 1, kind: "paste", paths: ["/a", "/b"], failed: ["/a", "/b"] },
      }));
    });
    expect(await screen.findByRole("button", { name: "2 XMP not updated" })).toBeTruthy();
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

it("rejects with X and keeps ⇧X", async () => {
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
  await waitFor(() => expect(invoke).toHaveBeenCalledWith("session_set_flag", expect.objectContaining({ flag: -1 })));
  vi.mocked(invoke).mockClear();
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
  await act(async () => pending[0]([{ path, before: { kind: 'flag', value: 1 }, after: { kind: 'flag', value: -1 } }]));
  expect(useLibraryStore.getState().imageFlags[path]).toBe(-1);
  await waitFor(() => expect(pending).toHaveLength(2));
  await act(async () => pending[1]([{ path, before: { kind: 'flag', value: -1 }, after: { kind: 'flag', value: 1 } }]));
  await waitFor(() => expect(pending).toHaveLength(3));
  await act(async () => pending[2]([{ path, before: { kind: 'flag', value: 1 }, after: { kind: 'flag', value: 0 } }]));
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
  await act(async () => finish([{ path, before: { kind: 'flag', value: 1 }, after: { kind: 'flag', value: -1 } }]));
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
      // A running task of the activity centre, not a toast.
      const restoring = () => useActivityStore.getState().entries.find((entry) => entry.id === "session-restore");
      expect(restoring()).toMatchObject({ status: "running" });
      expect(restoring()?.title).toContain("Opening startup-5000 (/Volumes/External/QA/startup-5000)");
      finish(summary);
      await act(async () => {
        await restored;
      });
      expect(restoring()).toBeUndefined();
      expect(navigate).toHaveBeenCalledWith(summary);
    } finally {
      vi.useRealTimers();
    }
  });

  it("opens another library and takes the launch's failure notice away (UT3 2026-11-01)", async () => {
    useSettingsStore.setState({ appSettings: { recentLibraries: ["/jobs/Old"] } as AppSettings });
    const beta = { ...summary, directory: "/jobs/Beta", manifest: { ...summary.manifest, name: "Beta" } };
    const base = vi.mocked(invoke).getMockImplementation()!;
    vi.mocked(invoke).mockImplementation(async (command, args) =>
      command === "session_last"
        ? summary
        : command === "session_open"
          ? beta
          : command === "session_summary"
            ? ((args as { directory: string }).directory === "/jobs/Beta" ? beta : summary)
            : base(command, args),
    );
    // The last library failed at launch.
    act(() => notify.error("Last library could not open: The drive is away", { toastId: RESTORE_FAILED_NOTICE }));
    // Whether the notice still shows (the mocked notify has no entries).
    vi.mocked(notify.isActive).mockReturnValue(true);
    render(<SessionWorkspace onNavigate={vi.fn(async () => {})} />);
    act(() => {
      expect(runAppCommand("recent:/jobs/Old")).toBe(true);
    });
    await waitFor(() => expect(useLibraryStore.getState().sessionDirectory).toBe("/jobs/Beta"));
    // Opening another library made the launch's failure no longer so.
    await waitFor(() => expect(notify.dismiss).toHaveBeenCalledWith(RESTORE_FAILED_NOTICE));
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
  expect(await screen.findByText(/12 photos, 5 with edits, 2 albums/)).toBeTruthy();
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
    expect.stringContaining("12 photos imported (5 with edits, 2 albums)"),
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
  expect(within(screen.getByRole("dialog")).queryByRole("radio")).toBeNull();
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

it("keeps a tethered camera while the library is still restoring and disconnects it in another library", () => {
  // tether_status answered before session_last: no library known yet.
  expect(leavesTetheredLibrary("/jobs/Acme", null)).toBe(false);
  expect(leavesTetheredLibrary("/jobs/Acme/", "/jobs/Acme")).toBe(false);
  expect(leavesTetheredLibrary("/jobs/Acme", "/jobs/Other")).toBe(true);
  expect(leavesTetheredLibrary(null, "/jobs/Other")).toBe(false);
});

it("suspends a hot folder resumed for a library that was left meanwhile", async () => {
  const user = userEvent.setup();
  const base = vi.mocked(invoke).getMockImplementation()!;
  let answer: (status: unknown) => void = () => {};
  const beta = { ...summary, directory: "/jobs/Beta", manifest: { ...summary.manifest, name: "Beta" } };
  vi.mocked(invoke).mockImplementation(async (command, args) => {
    const directory = (args as { directory?: string } | undefined)?.directory;
    if (command === "hot_folder_resume")
      return directory === "/jobs/Acme" ? new Promise((resolve) => (answer = resolve)) : null;
    if (command === "session_open") return beta;
    if (command === "session_summary") return directory === "/jobs/Beta" ? beta : summary;
    return base(command, args);
  });
  render(
    <ContextMenuProvider>
      <SessionWorkspace onNavigate={vi.fn(async () => {})} />
    </ContextMenuProvider>,
  );
  await user.click(screen.getByRole("button", { name: "Create a project" }));
  await user.type(screen.getByLabelText("Project name"), "Acme");
  await user.click(screen.getByRole("button", { name: "Location" }));
  await user.click(screen.getByRole("button", { name: "Create project" }));
  await waitFor(() => expect(invoke).toHaveBeenCalledWith("hot_folder_resume", { directory: "/jobs/Acme" }));
  // Another library opens before Acme's hot folder has resumed.
  await user.click(screen.getByTitle("/jobs/Acme"));
  await user.click(await screen.findByRole("menuitem", { name: /^Open/ }));
  await waitFor(() => expect(screen.getByTitle("/jobs/Beta")).toBeTruthy());
  await act(async () =>
    answer({ folder: "/Volumes/Tether/Hot", session: "/jobs/Acme", imported: 0, nextCapture: { mode: "defaults" } }),
  );
  await waitFor(() => expect(invoke).toHaveBeenCalledWith("hot_folder_suspend", { directory: "/jobs/Acme" }));
  expect(screen.queryByRole("button", { name: /Hot folder ·/ })).toBeNull();
});


it("lets form controls receive keys that the dialog does not handle", async () => {
  const user = userEvent.setup();
  render(<SessionWorkspace onNavigate={vi.fn()} />);
  await user.click(screen.getByRole("button", { name: "Create a project" }));
  const field = screen.getByLabelText("Project name");
  const handler = vi.fn();
  field.addEventListener("keydown", handler);
  await user.keyboard("a{ArrowLeft}");
  expect(handler.mock.calls.map(([event]) => event.key)).toEqual(["a", "ArrowLeft"]);
  await user.keyboard("{Escape}");
  expect(handler).toHaveBeenCalledTimes(2);
  expect(document.querySelector("dialog[open]")).toBeNull();
});

it('opens recent libraries through the normal workflow, records success and clears only the menu', async () => {
  useSettingsStore.setState({ appSettings: { recentLibraries: ['/jobs/Old'] } as AppSettings });
  const base = vi.mocked(invoke).getMockImplementation()!;
  vi.mocked(invoke).mockImplementation(async (command, args) => command === 'session_open' ? summary : base(command, args));
  const navigate = vi.fn(async () => {});
  render(<SessionWorkspace onNavigate={navigate} />);
  act(() => { expect(runAppCommand('recent:/jobs/Old')).toBe(true); });
  await waitFor(() => expect(navigate).toHaveBeenCalledWith('/jobs/Acme/Capture', true));
  await waitFor(() => expect(useSettingsStore.getState().appSettings?.recentLibraries).toEqual(['/jobs/Acme', '/jobs/Old']));
  expect(invoke).toHaveBeenCalledWith('save_settings', { settings: expect.objectContaining({ recentLibraries: ['/jobs/Acme', '/jobs/Old'] }) });
  act(() => { expect(runAppCommand('clear_recent')).toBe(true); });
  expect(useSettingsStore.getState().appSettings?.recentLibraries).toEqual([]);
  expect(appCommand('recent:/jobs/Old')).toBeUndefined();
  expect(useLibraryStore.getState().sessionDirectory).toBe('/jobs/Acme');
});

it('keeps offline recents after a failed open and never records the failed attempt', async () => {
  useSettingsStore.setState({ appSettings: { recentLibraries: ['/offline/Session'] } as AppSettings });
  vi.mocked(invoke).mockRejectedValue(new Error('Drive is disconnected'));
  const navigate = vi.fn(async () => {});
  render(<SessionWorkspace onNavigate={navigate} />);
  act(() => { runAppCommand('recent:/offline/Session'); });
  await waitFor(() => expect(toast.error).toHaveBeenCalledWith('Error: Drive is disconnected'));
  expect(navigate).not.toHaveBeenCalled();
  expect(useSettingsStore.getState().appSettings?.recentLibraries).toEqual(['/offline/Session']);
});
