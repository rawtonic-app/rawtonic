import "../../i18n";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { act, cleanup, render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { invoke } from "@tauri-apps/api/core";
import { toast } from "react-toastify";
import SessionWorkspace, { type SessionSummary } from "./SessionWorkspace";
import { useLibraryStore } from "../../store/useLibraryStore";
import { useSettingsStore } from "../../store/useSettingsStore";
import { useUIStore } from "../../store/useUIStore";
import { ContextMenuProvider } from "../../context/ContextMenuContext";
import type { AppSettings } from "../ui/AppProperties";

type DragDropPayload =
  | { type: "enter"; paths: string[]; position: { x: number; y: number } }
  | { type: "drop"; paths: string[]; position: { x: number; y: number } }
  | { type: "leave" };
const drag = vi.hoisted(() => ({ handler: null as null | ((event: { payload: DragDropPayload }) => void) }));
const events = vi.hoisted(() => new Map<string, (event: { payload: unknown }) => void>());

vi.mock("@tauri-apps/api/core", () => ({ isTauri: () => true, invoke: vi.fn() }));
vi.mock("@tauri-apps/api/webview", () => ({
  getCurrentWebview: () => ({
    onDragDropEvent: async (handler: (event: { payload: DragDropPayload }) => void) => {
      drag.handler = handler;
      return () => {};
    },
  }),
}));
vi.mock("@tauri-apps/api/event", () => ({
  listen: vi.fn(async (name: string, callback: (event: { payload: unknown }) => void) => {
    events.set(name, callback);
    return () => {};
  }),
}));
vi.mock("@tauri-apps/plugin-dialog", () => ({ open: vi.fn(async () => null) }));
vi.mock("@tauri-apps/api/path", () => ({ join: async (...parts: string[]) => parts.join("/") }));
vi.mock("@tauri-apps/plugin-os", () => ({ platform: () => "macos" }));
vi.mock("react-toastify", () => ({
  toast: { error: vi.fn(), success: vi.fn(), info: vi.fn(), dismiss: vi.fn(), warning: vi.fn() },
}));

const summary: SessionSummary = {
  directory: "/jobs/Acme",
  manifest: { name: "Acme", mode: "session", capture: "Capture", output: "Output" },
  assets: 0,
  picked: 0,
  rated: 0,
  xmp_pending: 0,
};
const scanned = [
  { path: "/Volumes/CARD/DCIM/A.ARW", size: 20_000_000, duplicate: false },
  { path: "/Volumes/CARD/DCIM/B.ARW", size: 21_000_000, duplicate: true },
  { path: "/Volumes/CARD/DCIM/C.ARW", size: 22_000_000, duplicate: false },
];
let lastSession: SessionSummary | null = summary;

beforeEach(() => {
  vi.clearAllMocks();
  events.clear();
  drag.handler = null;
  lastSession = summary;
  document.body.innerHTML = "";
  useUIStore.setState({ activeView: "library" });
  useLibraryStore.setState({ rootPaths: [], imageList: [], multiSelectedPaths: [], imageFlags: {}, sessionDirectory: null });
  useSettingsStore.setState({
    appSettings: {} as AppSettings,
    handleSettingsChange: async () => {},
    supportedTypes: { raw: ["arw"], nonRaw: ["jpg"] },
  });
  HTMLDialogElement.prototype.showModal = function () {
    this.setAttribute("open", "");
  };
  HTMLDialogElement.prototype.close = function () {
    this.removeAttribute("open");
  };
  vi.mocked(invoke).mockImplementation(async (command, args) => {
    switch (command) {
      case "session_last":
        return lastSession;
      case "session_summary":
        return summary;
      case "session_flags":
        return {};
      case "session_ingest_scan":
        return (args as { paths: string[] }).paths[0] === "/Volumes/CARD2"
          ? { files: [scanned[0], { path: "/Volumes/CARD2/D.ARW", size: 1_000_000, duplicate: false }], truncated: false }
          : { files: scanned, truncated: false };
      case "session_ingest_thumbnail":
        return new ArrayBuffer(0);
      case "session_ingest_preview":
        return new ArrayBuffer(0);
      case "session_ingest_capture_info":
        return { iso: "ISO 200", exposure: "1/500 s", aperture: "f/4", focalLength: "85 mm" };
      case "session_ingest": {
        const { sources, folder } = args as { sources: string[]; folder: string | null };
        return sources.map((source) => ({
          source,
          destination: `${folder ?? "/jobs/Acme/Capture"}/${source.split("/").pop()}`,
          skipped: false,
          error: null,
        }));
      }
      default:
        return { path: "/jobs/Acme", children: [] };
    }
  });
});
afterEach(cleanup);

/** The workspace with the last session reopened and the window's drop handler attached. */
async function open(navigate = vi.fn(async () => {})) {
  render(
    <ContextMenuProvider>
      <SessionWorkspace onNavigate={navigate} />
    </ContextMenuProvider>,
  );
  if (lastSession) await waitFor(() => expect(useLibraryStore.getState().sessionDirectory).toBe("/jobs/Acme"));
  await waitFor(() => expect(drag.handler).not.toBeNull());
  return navigate;
}

/** Files dropped at a point over `element` (or nothing in particular). */
function drop(paths: string[], element: Element | null = null) {
  document.elementFromPoint = (() => element) as unknown as typeof document.elementFromPoint;
  act(() => {
    drag.handler!({ payload: { type: "enter", paths, position: { x: 10, y: 10 } } });
  });
  act(() => {
    drag.handler!({ payload: { type: "drop", paths, position: { x: 10, y: 10 } } });
  });
}

const ingestCall = () => vi.mocked(invoke).mock.calls.find(([command]) => command === "session_ingest");

describe("Dropping photographs onto the window", () => {
  it("opens the ingest dialog with the dropped files' previews, and copies only the checked ones", async () => {
    const user = userEvent.setup();
    const navigate = await open();
    drop(["/Volumes/CARD/DCIM"]);
    await waitFor(() => expect(screen.getAllByRole("checkbox")).toHaveLength(3));
    expect(invoke).toHaveBeenCalledWith("session_ingest_scan", {
      directory: "/jobs/Acme",
      paths: ["/Volumes/CARD/DCIM"],
    });
    // Nothing is copied before the user confirms.
    expect(ingestCall()).toBeUndefined();
    expect(screen.getByRole("checkbox", { name: "B.ARW" }).getAttribute("aria-checked")).toBe("false");
    await user.click(screen.getByRole("checkbox", { name: "C.ARW" }));
    await user.click(screen.getByRole("button", { name: "Copy 1 photograph" }));
    await waitFor(() => expect(ingestCall()).toBeTruthy());
    expect(ingestCall()![1]).toMatchObject({
      directory: "/jobs/Acme",
      sources: ["/Volumes/CARD/DCIM/A.ARW"],
      folder: null,
    });
    // The grid shows the capture folder, where the photographs appear as they are copied.
    expect(navigate).toHaveBeenLastCalledWith("/jobs/Acme/Capture");
    await waitFor(() => expect(toast.success).toHaveBeenCalled());
    expect(invoke).not.toHaveBeenCalledWith("session_album_add", expect.anything());
  });

  it("shows a photograph large inside the dialog; Escape returns to the grid and keeps the dialog open", async () => {
    const user = userEvent.setup();
    await open();
    drop(["/Volumes/CARD/DCIM"]);
    await waitFor(() => expect(screen.getAllByRole("checkbox")).toHaveLength(3));
    await user.dblClick(screen.getByRole("checkbox", { name: "A.ARW" }));
    const view = screen.getByRole("region", { name: "Large view" });
    expect(invoke).toHaveBeenCalledWith("session_ingest_preview", {
      path: "/Volumes/CARD/DCIM/A.ARW",
      maxSize: expect.any(Number),
    });
    await waitFor(() => expect(view.textContent).toContain("ISO 200 · 1/500 s · f/4 · 85 mm"));
    await user.keyboard("{ArrowRight}");
    expect(view.textContent).toContain("B.ARW");
    await user.keyboard("{Escape}");
    expect(screen.queryByRole("region", { name: "Large view" })).toBeNull();
    expect(document.querySelector("dialog")?.hasAttribute("open")).toBe(true);
    expect(screen.getAllByRole("checkbox")).toHaveLength(3);
    await waitFor(() => expect(document.activeElement?.getAttribute("aria-label")).toBe("B.ARW"));
    // The library's P/X/U flags do not act on the library behind the dialog.
    await user.keyboard("p");
    expect(invoke).not.toHaveBeenCalledWith("session_set_flag", expect.anything());
  });

  it("dropped onto an album: imports, then adds the photographs to that album", async () => {
    const user = userEvent.setup();
    await open();
    const row = document.createElement("button");
    row.dataset.albumDrop = "album-7";
    row.dataset.albumName = "Client picks";
    document.body.append(row);
    drop(["/Volumes/CARD/DCIM/A.ARW"], row);
    const label = await screen.findByLabelText("Add to album “Client picks” after import");
    expect((label as HTMLInputElement).checked).toBe(true);
    await user.click(screen.getByRole("button", { name: "Copy 2 photographs" }));
    await waitFor(() =>
      expect(invoke).toHaveBeenCalledWith("session_album_add", {
        directory: "/jobs/Acme",
        id: "album-7",
        paths: ["/jobs/Acme/Capture/A.ARW", "/jobs/Acme/Capture/C.ARW"],
      }),
    );
  });

  it("dropped onto a folder of the library: copies into that folder", async () => {
    const user = userEvent.setup();
    const navigate = await open();
    const row = document.createElement("div");
    row.dataset.folderDrop = "/jobs/Acme/Selects";
    document.body.append(row);
    drop(["/Volumes/CARD/DCIM"], row);
    await screen.findByRole("button", { name: "Use Capture instead" });
    expect(screen.getAllByText("Acme / Selects").length).toBeGreaterThan(0);
    await user.click(screen.getByRole("button", { name: "Copy 2 photographs" }));
    await waitFor(() => expect(ingestCall()).toBeTruthy());
    expect(ingestCall()![1]).toMatchObject({ folder: "/jobs/Acme/Selects" });
    expect(navigate).toHaveBeenLastCalledWith("/jobs/Acme/Selects");
  });

  it("a folder outside the library is not a destination: the drop goes to Capture", async () => {
    const user = userEvent.setup();
    await open();
    const row = document.createElement("div");
    row.dataset.folderDrop = "/Users/someone/Pictures";
    document.body.append(row);
    drop(["/Volumes/CARD/DCIM"], row);
    await waitFor(() => expect(screen.getAllByRole("checkbox")).toHaveLength(3));
    expect(screen.queryByRole("button", { name: "Use Capture instead" })).toBeNull();
    await user.click(screen.getByRole("button", { name: "Copy 2 photographs" }));
    await waitFor(() => expect(ingestCall()).toBeTruthy());
    expect(ingestCall()![1]).toMatchObject({ folder: null });
  });

  it("more files dropped onto the open dialog join its list, keeping the user's choices", async () => {
    const user = userEvent.setup();
    await open();
    drop(["/Volumes/CARD/DCIM"]);
    await waitFor(() => expect(screen.getAllByRole("checkbox")).toHaveLength(3));
    await user.click(screen.getByRole("checkbox", { name: "A.ARW" }));
    drop(["/Volumes/CARD2"]);
    await waitFor(() => expect(screen.getAllByRole("checkbox")).toHaveLength(4));
    // A.ARW was listed already: it stays unchecked; the new D.ARW is checked.
    expect(screen.getByRole("checkbox", { name: "A.ARW" }).getAttribute("aria-checked")).toBe("false");
    expect(screen.getByRole("checkbox", { name: "D.ARW" }).getAttribute("aria-checked")).toBe("true");
    await user.click(screen.getByRole("button", { name: "Copy 2 photographs" }));
    await waitFor(() => expect(ingestCall()).toBeTruthy());
    expect(ingestCall()![1]).toMatchObject({ sources: ["/Volumes/CARD/DCIM/C.ARW", "/Volumes/CARD2/D.ARW"] });
  });

  it("in a catalog, a folder of its list on another drive is a destination, shown with its full path and drive", async () => {
    const user = userEvent.setup();
    lastSession = { ...summary, directory: "/Users/me/Main.catalog", manifest: { ...summary.manifest, name: "Main", mode: "catalog" } };
    vi.mocked(invoke).mockImplementation(async (command, args) => {
      if (command === "session_last" || command === "session_summary") return lastSession;
      if (command === "session_flags") return {};
      if (command === "session_ingest_scan") return { files: scanned, truncated: false };
      if (command === "session_ingest") return [];
      return { path: "/Users/me/Main.catalog", children: [] };
    });
    render(
      <ContextMenuProvider>
        <SessionWorkspace onNavigate={vi.fn(async () => {})} />
      </ContextMenuProvider>,
    );
    await waitFor(() => expect(useLibraryStore.getState().sessionDirectory).toBe("/Users/me/Main.catalog"));
    await waitFor(() => expect(drag.handler).not.toBeNull());
    const row = document.createElement("div");
    row.dataset.folderDrop = "/Volumes/Shoots/2026/Day 2";
    document.body.append(row);
    drop(["/Volumes/CARD/DCIM"], row);
    await screen.findByText("/Volumes/Shoots/2026/Day 2");
    expect(screen.getByText("On the drive “Shoots”")).toBeTruthy();
    await user.click(screen.getByRole("button", { name: "Copy 2 photographs" }));
    await waitFor(() => expect(ingestCall()).toBeTruthy());
    expect(ingestCall()![1]).toMatchObject({ folder: "/Volumes/Shoots/2026/Day 2" });
  });

  it("“Import into this folder…” opens the dialog with that destination and no photographs yet", async () => {
    await open();
    act(() => {
      window.dispatchEvent(new CustomEvent("rawtonic-open-ingest", { detail: { folder: "/jobs/Acme/Selects" } }));
    });
    await screen.findByText("/jobs/Acme/Selects");
    expect(screen.getAllByText("Acme / Selects").length).toBeGreaterThan(0);
    expect(screen.queryAllByRole("checkbox")).toHaveLength(0);
    expect(invoke).not.toHaveBeenCalledWith("session_ingest_scan", expect.anything());
    expect(screen.getByText(/Drop photographs or folders here/)).toBeTruthy();
  });

  it("without an open library nothing opens and nothing is copied", async () => {
    lastSession = null;
    await open();
    drop(["/Volumes/CARD/DCIM"]);
    expect(toast.info).toHaveBeenCalled();
    expect(screen.queryAllByRole("checkbox")).toHaveLength(0);
    expect(invoke).not.toHaveBeenCalledWith("session_ingest_scan", expect.anything());
  });
});

describe("Copy or add where they are (catalogs)", () => {
  const catalog: SessionSummary = {
    ...summary,
    directory: "/Users/me/Main.catalog",
    manifest: { ...summary.manifest, name: "Main", mode: "catalog" },
  };
  const onCard = [
    { path: "/Volumes/EOS_DIGITAL/DCIM/100CANON/A.CR3", size: 30_000_000, duplicate: false, medium: "card" },
  ];
  const onDrive = [
    { path: "/Volumes/Archive/2026/B.CR3", size: 30_000_000, duplicate: false, medium: "disk" },
    { path: "/Volumes/Archive/2026/C.CR3", size: 30_000_000, duplicate: false, medium: "disk" },
  ];
  beforeEach(() => {
    lastSession = catalog;
    vi.mocked(invoke).mockImplementation(async (command, args) => {
      if (command === "session_last" || command === "session_summary") return lastSession;
      if (command === "session_flags") return {};
      if (command === "session_ingest_scan")
        return {
          files: (args as { paths: string[] }).paths[0].includes("DCIM") ? onCard : onDrive,
          truncated: false,
        };
      if (command === "session_ingest") return [];
      return { path: "/Users/me/Main.catalog", children: [] };
    });
  });
  const radio = (name: RegExp) => screen.getByRole("radio", { name }) as HTMLInputElement;
  async function openCatalog() {
    const navigate = vi.fn(async () => {});
    render(
      <ContextMenuProvider>
        <SessionWorkspace onNavigate={navigate} />
      </ContextMenuProvider>,
    );
    await waitFor(() => expect(useLibraryStore.getState().sessionDirectory).toBe("/Users/me/Main.catalog"));
    await waitFor(() => expect(drag.handler).not.toBeNull());
    return navigate;
  }

  it("photographs on a drive default to copy; adding externally is an explicit choice", async () => {
    const user = userEvent.setup();
    const navigate = await openCatalog();
    drop(["/Volumes/Archive/2026"]);
    await waitFor(() => expect(radio(/Copy to/).checked).toBe(true));
    await user.click(radio(/Add where they are/));
    expect(screen.getByText(/stay where they are/)).toBeTruthy();
    // No naming or second copy for photographs that stay where they are.
    expect(screen.queryByText("Second copy")).toBeNull();
    await user.click(screen.getByRole("button", { name: "Add 2 photographs" }));
    await waitFor(() => expect(ingestCall()).toBeTruthy());
    expect(ingestCall()![1]).toMatchObject({ mode: "add", folder: null, sources: onDrive.map((f) => f.path) });
    expect(navigate).toHaveBeenLastCalledWith("/Volumes/Archive/2026");
  });

  it("photographs on a card are copied; adding them is disabled with the reason", async () => {
    const user = userEvent.setup();
    await openCatalog();
    drop(["/Volumes/EOS_DIGITAL/DCIM"]);
    await waitFor(() => expect(screen.getAllByRole("checkbox")).toHaveLength(1));
    expect(radio(/Copy to/).checked).toBe(true);
    expect(radio(/Add where they are/).disabled).toBe(true);
    expect(screen.getAllByText(/must be copied off it/).length).toBeGreaterThan(0);
    await user.click(screen.getByRole("button", { name: "Copy 1 photograph" }));
    await waitFor(() => expect(ingestCall()).toBeTruthy());
    expect(ingestCall()![1]).toMatchObject({ mode: "copy" });
  });

  it("is recomputed when the sources change: a card added to drive photographs switches to copy", async () => {
    const user = userEvent.setup();
    await openCatalog();
    drop(["/Volumes/Archive/2026"]);
    await waitFor(() => expect(radio(/Copy to/).checked).toBe(true));
    await user.click(radio(/Add where they are/));
    drop(["/Volumes/EOS_DIGITAL/DCIM"]);
    await waitFor(() => expect(screen.getAllByRole("checkbox")).toHaveLength(3));
    expect(radio(/Copy to/).checked).toBe(true);
    expect(radio(/Add where they are/).disabled).toBe(true);
    // The user's own choice is not kept across a new dialog either.
    await user.click(screen.getByRole("button", { name: "Close" }));
    drop(["/Volumes/Archive/2026"]);
    await waitFor(() => expect(screen.getAllByRole("checkbox")).toHaveLength(2));
    expect(radio(/Copy to/).checked).toBe(true);
  });

  it("a drop onto a folder copies into it", async () => {
    await openCatalog();
    const row = document.createElement("div");
    row.dataset.folderDrop = "/Volumes/Shoots/2026";
    document.body.append(row);
    drop(["/Volumes/Archive/2026"], row);
    await waitFor(() => expect(screen.getAllByRole("checkbox")).toHaveLength(2));
    expect(radio(/Copy to/).checked).toBe(true);
    expect(radio(/Add where they are/).disabled).toBe(false);
  });

  it("a session has no switch: it always copies", async () => {
    lastSession = summary;
    vi.mocked(invoke).mockImplementation(async (command) => {
      if (command === "session_last" || command === "session_summary") return summary;
      if (command === "session_flags") return {};
      if (command === "session_ingest_scan") return { files: onDrive, truncated: false };
      return { path: "/jobs/Acme", children: [] };
    });
    await open();
    drop(["/Volumes/Archive/2026"]);
    await waitFor(() => expect(screen.getAllByRole("checkbox")).toHaveLength(2));
    expect(screen.queryAllByRole("radio")).toHaveLength(0);
    expect(screen.getByRole("button", { name: "Copy 2 photographs" })).toBeTruthy();
  });
});
