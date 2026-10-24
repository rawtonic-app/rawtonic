import "../../i18n";
import i18n from "i18next";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import {
  cleanup,
  fireEvent,
  render,
  screen,
  within,
} from "@testing-library/react";
import { invoke } from "@tauri-apps/api/core";
import StudioExport, { describeExportJob } from "./StudioExport";
import { useEditorStore } from "../../store/useEditorStore";
import { useLibraryStore } from "../../store/useLibraryStore";
import { useProcessStore, type ExportJob } from "../../store/useProcessStore";
import { Status } from "../ui/ExportImportProperties";
import { useSettingsStore } from "../../store/useSettingsStore";
import {
  DEFAULT_RECIPE_DRAFT,
  presetFromDraft,
} from "../../utils/exportRecipes";
import type { AppSettings } from "../ui/AppProperties";

vi.mock("@tauri-apps/api/core", () => ({
  invoke: vi.fn(() => Promise.resolve()),
}));
vi.mock("@tauri-apps/plugin-dialog", () => ({
  open: vi.fn(() => Promise.resolve("/out")),
  save: vi.fn(() => Promise.resolve("/out/a.jpg")),
}));
vi.mock("react-toastify", () => ({
  toast: { error: vi.fn(), success: vi.fn() },
}));

/** The exports the form queued: one request per recipe, queued together. */
const exported = () =>
  vi
    .mocked(invoke)
    .mock.calls.filter((c) => c[0] === "export_images_batch")
    .flatMap((c) => (c[1] as { requests: Record<string, unknown>[] }).requests);

const job = (part: Partial<ExportJob>): ExportJob => ({
  id: "j1",
  label: "30 photos as JPG",
  state: "interrupted",
  createdAt: "",
  total: 30,
  done: 12,
  failed: 0,
  failures: [],
  ...part,
});

beforeEach(() => {
  vi.clearAllMocks();
  localStorage.removeItem("rawtonic.export.recipes");
  useSettingsStore.setState({ appSettings: null });
  useLibraryStore.setState({
    multiSelectedPaths: ["/p/a.RAF", "/p/b.RAF"],
    sessionDirectory: null,
  });
  useProcessStore.setState({
    exportQueue: [],
    exportQueueHeld: false,
    exportItems: {},
    exportPausing: [],
    exportState: {
      status: Status.Idle,
      errorMessage: "",
      progress: { current: 0, total: 0 },
    },
  });
});
afterEach(() => cleanup());

it("describes what each queued export is waiting for", () => {
  expect(describeExportJob(job({}))).toBe("Interrupted (12 of 30 exported)");
  expect(describeExportJob(job({ state: "failed", failed: 3 }))).toBe(
    "3 of 30 failed",
  );
  expect(describeExportJob(job({ state: "queued", done: 0 }))).toBe("Waiting");
  expect(describeExportJob(job({ state: "cancelled" }))).toBe(
    "Cancelled (12 of 30 exported)",
  );
});

it("lists why each photo of a failed export did not export", async () => {
  useProcessStore.setState({
    exportQueue: [
      job({
        state: "failed",
        failed: 2,
        failures: [
          {
            path: "/p/big.RAF?vc=2",
            error:
              "The image (40000 × 9000 px) is larger than this graphics card can render (16384 px per side).",
          },
          { path: "/p/b.RAF", error: "Decoder said no" },
        ],
      }),
    ],
  });
  render(<StudioExport />);
  const failures = screen.getByRole("list", {
    name: "Photos that did not export",
  });
  const items = within(failures).getAllByRole("listitem");
  expect(items.map((item) => item.textContent)).toEqual([
    "big.RAFThe image (40000 × 9000 px) is larger than this graphics card can render (16384 px per side).",
    "b.RAFDecoder said no",
  ]);
  const i18n = (await import("i18next")).default;
  await i18n.changeLanguage("de");
  try {
    await vi.waitFor(() =>
      expect(within(failures).getAllByRole("listitem")[0].textContent).toBe(
        "big.RAFDas Bild (40000 × 9000 px) ist größer, als diese Grafikkarte rendern kann (16384 px pro Seite).",
      ),
    );
  } finally {
    await i18n.changeLanguage("en");
  }
});

it("resumes and removes interrupted exports", () => {
  useProcessStore.setState({
    exportQueue: [job({}), job({ id: "j2", state: "done" })],
  });
  render(<StudioExport />);
  const queue = screen.getByRole("region", { name: "Export queue" });
  expect(within(queue).getAllByRole("listitem")).toHaveLength(1);
  fireEvent.click(screen.getByRole("button", { name: "Resume" }));
  expect(invoke).toHaveBeenCalledWith("export_queue_resume", { job: "j1" });
  fireEvent.click(screen.getByRole("button", { name: "Remove" }));
  expect(invoke).toHaveBeenCalledWith("export_queue_remove", { job: "j1" });
});

it("queues another export while one runs instead of refusing it", async () => {
  useProcessStore.setState({
    exportState: {
      status: Status.Exporting,
      errorMessage: "",
      progress: { current: 3, total: 10 },
    },
  });
  render(<StudioExport />);
  expect(screen.getByRole("button", { name: "Cancel export" })).toBeTruthy();
  fireEvent.click(screen.getByRole("button", { name: "Add to queue" }));
  await vi.waitFor(() =>
    expect(exported()).toContainEqual(expect.objectContaining({ paths: ["/p/a.RAF", "/p/b.RAF"] }),
    ),
  );
  // The running export's progress stays on screen.
  expect(useProcessStore.getState().exportState.progress).toEqual({
    current: 3,
    total: 10,
  });
});

it("exports a session into its Output folder without asking, like Capture One", async () => {
  useLibraryStore.setState({ sessionDirectory: "/job" });
  render(<StudioExport />);
  fireEvent.click(screen.getByRole("button", { name: "Export" }));
  await vi.waitFor(() =>
    expect(exported()).toContainEqual(expect.objectContaining({
        outputFolderOrFile: "/job/Output",
        isExplicitFilePath: false,
      }),
    ),
  );
  const { open, save } = await import("@tauri-apps/plugin-dialog");
  expect(open).not.toHaveBeenCalled();
  expect(save).not.toHaveBeenCalled();
});

it("sends the open photograph's recipe only once it is that photograph's", async () => {
  // Switching from a to b: b is not ready yet and the store still holds a's adjustments.
  const previous = useEditorStore.getState();
  useEditorStore.setState({
    selectedImage: { path: "/p/b.RAF", isReady: false } as never,
    adjustments: { exposure: 1.5 } as never,
  });
  useLibraryStore.setState({ sessionDirectory: "/job" });
  try {
    render(<StudioExport />);
    fireEvent.click(document.querySelector(".studio-export-submit") as HTMLButtonElement);
    await vi.waitFor(() =>
      expect(exported()).toContainEqual(expect.objectContaining({ currentEditPath: null, currentEditAdjustments: null }),
      ),
    );
    cleanup();
    vi.mocked(invoke).mockClear();
    useEditorStore.setState({ selectedImage: { path: "/p/b.RAF", isReady: true } as never });
    render(<StudioExport />);
    fireEvent.click(document.querySelector(".studio-export-submit") as HTMLButtonElement);
    await vi.waitFor(() =>
      expect(exported()).toContainEqual(expect.objectContaining({ currentEditPath: "/p/b.RAF", currentEditAdjustments: { exposure: 1.5 } }),
      ),
    );
  } finally {
    useEditorStore.setState({ selectedImage: previous.selectedImage, adjustments: previous.adjustments });
  }
});

it("closes once the export is queued, the progress staying on the top bar", async () => {
  useLibraryStore.setState({ sessionDirectory: "/job" });
  const onStarted = vi.fn();
  render(<StudioExport onStarted={onStarted} />);
  fireEvent.click(screen.getByRole("button", { name: "Export" }));
  await vi.waitFor(() => expect(onStarted).toHaveBeenCalledTimes(1));
  expect(exported()).not.toHaveLength(0);
});

it("stays open when no destination was chosen", async () => {
  const { open } = await import("@tauri-apps/plugin-dialog");
  vi.mocked(open).mockImplementation(() => Promise.resolve(null));
  try {
    const onStarted = vi.fn();
    render(<StudioExport onStarted={onStarted} />);
    const submit = document.querySelector(".studio-export-submit") as HTMLButtonElement;
    fireEvent.click(submit);
    await vi.waitFor(() => expect(open).toHaveBeenCalled());
    await new Promise((r) => setTimeout(r, 0));
    expect(onStarted).not.toHaveBeenCalled();
    expect(exported()).toHaveLength(0);
  } finally {
    vi.mocked(open).mockImplementation(() => Promise.resolve("/out"));
  }
});

it("starts a waiting export that a cancel held", () => {
  useProcessStore.setState({
    exportQueue: [job({ state: "queued", done: 0 })],
    exportQueueHeld: false,
  });
  const { unmount } = render(<StudioExport />);
  // Not held: it starts by itself after the running one.
  expect(screen.queryByRole("button", { name: "Start" })).toBeNull();
  unmount();
  useProcessStore.setState({ exportQueueHeld: true });
  render(<StudioExport />);
  fireEvent.click(screen.getByRole("button", { name: "Start" }));
  expect(invoke).toHaveBeenCalledWith("export_queue_resume", { job: "j1" });
});

it("exports once per ticked recipe, each with its own settings and place", async () => {
  const web = presetFromDraft(
    {
      ...DEFAULT_RECIPE_DRAFT,
      format: "jpg",
      quality: 70,
      depth: 16,
      resize: true,
      resizeValue: 2048,
      metadata: true,
      stripGps: true,
      template: "{original_filename}_web",
      where: "output",
    },
    "web",
    "Web",
  );
  const print = presetFromDraft(
    {
      ...DEFAULT_RECIPE_DRAFT,
      format: "tiff",
      quality: 90,
      depth: 16,
      resize: false,
      resizeValue: 3000,
      metadata: true,
      stripGps: false,
      template: "{original_filename}",
      where: "choose",
      folder: "/print",
    },
    "print",
    "Print",
  );
  const handleSettingsChange = vi.fn(async () => {});
  useSettingsStore.setState({
    appSettings: { exportPresets: [web, print] } as unknown as AppSettings,
    handleSettingsChange,
  });
  useLibraryStore.setState({ sessionDirectory: "/job" });
  localStorage.removeItem("rawtonic.export.recipes");
  render(<StudioExport />);
  fireEvent.click(screen.getByRole("checkbox", { name: "Export with Web" }));
  fireEvent.click(screen.getByRole("checkbox", { name: "Export with Print" }));
  fireEvent.click(
    screen.getByRole("button", { name: "Export with 2 recipes" }),
  );
  await vi.waitFor(() =>
    expect(
      exported(),
    ).toHaveLength(2),
  );
  // Queued in one call, so they run together: each photo developed once.
  expect(
    vi.mocked(invoke).mock.calls.filter((c) => c[0] === "export_images_batch"),
  ).toHaveLength(1);
  const calls = exported();
  expect(calls[0]).toMatchObject({
    outputFolderOrFile: "/job/Output",
    outputFormat: "jpg",
  });
  expect((calls[0].exportSettings as { resize: unknown }).resize).toEqual({
    mode: "longEdge",
    value: 2048,
    dontEnlarge: true,
  });
  expect(calls[1]).toMatchObject({
    outputFolderOrFile: "/print",
    outputFormat: "tiff",
  });
});

it("remembers the folder chosen for each of several recipes", async () => {
  const { open } = await import("@tauri-apps/plugin-dialog");
  vi.mocked(open)
    .mockResolvedValueOnce("/out/web")
    .mockResolvedValueOnce("/out/print");
  const recipe = (id: string, name: string) =>
    presetFromDraft(
      { ...DEFAULT_RECIPE_DRAFT, where: "choose", folder: undefined },
      id,
      name,
    );
  // Like the real store: the new settings are there at once.
  const handleSettingsChange = vi.fn(async (settings: AppSettings) =>
    useSettingsStore.setState({ appSettings: settings }),
  );
  useSettingsStore.setState({
    appSettings: {
      exportPresets: [recipe("web", "Web"), recipe("print", "Print")],
    } as unknown as AppSettings,
    handleSettingsChange,
  });
  render(<StudioExport />);
  fireEvent.click(screen.getByRole("checkbox", { name: "Export with Web" }));
  fireEvent.click(screen.getByRole("checkbox", { name: "Export with Print" }));
  fireEvent.click(
    screen.getByRole("button", { name: "Export with 2 recipes" }),
  );
  await vi.waitFor(() =>
    expect(
      exported(),
    ).toHaveLength(2),
  );
  const saved = useSettingsStore.getState().appSettings?.exportPresets ?? [];
  expect(
    Object.fromEntries(saved.map((p) => [p.id, p.lastExportPath])),
  ).toEqual({ web: "/out/web", print: "/out/print" });
});

it("saves the form as a new recipe", () => {
  const handleSettingsChange = vi.fn(async () => {});
  useSettingsStore.setState({
    appSettings: { exportPresets: [] } as unknown as AppSettings,
    handleSettingsChange,
  });
  render(<StudioExport />);
  fireEvent.change(screen.getByLabelText("New recipe name"), {
    target: { value: "Client JPEG" },
  });
  fireEvent.click(screen.getByRole("button", { name: "Save as recipe" }));
  expect(handleSettingsChange).toHaveBeenCalledWith(
    expect.objectContaining({
      exportPresets: [
        expect.objectContaining({ name: "Client JPEG", fileFormat: "jpeg" }),
      ],
    }),
  );
});

it("saves the recipe on Return in its name field instead of exporting", () => {
  const handleSettingsChange = vi.fn(async () => {});
  useSettingsStore.setState({
    appSettings: { exportPresets: [] } as unknown as AppSettings,
    handleSettingsChange,
  });
  render(<StudioExport />);
  const name = screen.getByLabelText("New recipe name");
  fireEvent.change(name, { target: { value: "Client JPEG" } });
  // false: the key's default (submitting the form, i.e. exporting) was prevented.
  expect(fireEvent.keyDown(name, { key: "Enter", code: "Enter" })).toBe(false);
  expect(handleSettingsChange).toHaveBeenCalledWith(
    expect.objectContaining({
      exportPresets: [expect.objectContaining({ name: "Client JPEG" })],
    }),
  );
  expect(exported()).toHaveLength(0);
});

it("does not capitalise or correct a typed subfolder name", () => {
  render(<StudioExport />);
  const subfolder = screen.getByLabelText("Subfolder");
  expect(subfolder.getAttribute("autocapitalize")).toBe("off");
  expect(subfolder.getAttribute("autocorrect")).toBe("off");
  expect(subfolder.getAttribute("spellcheck")).toBe("false");
});

/** Sets every export setting the inherited panel had through the Studio form. */
async function fillEverything() {
  const { open } = await import("@tauri-apps/plugin-dialog");
  vi.mocked(open).mockResolvedValueOnce("/marks/logo.png");
  const change = (label: string, value: string) =>
    fireEvent.change(screen.getByLabelText(label), { target: { value } });
  const tick = (label: string) => fireEvent.click(screen.getByLabelText(label));
  change("Format", "webp");
  change("WebP quality", "72");
  change("Subfolder", "Web/small");
  tick("Keep the folder structure");
  tick("Date files with the capture time");
  tick("Resize to fit");
  change("Resize mode", "megapixels");
  change("Megapixels (MP)", "2.5");
  tick("Don’t enlarge");
  tick("Add a watermark");
  fireEvent.click(
    screen.getByRole("button", { name: "Choose watermark image" }),
  );
  await screen.findByText("logo.png");
  fireEvent.click(screen.getByRole("radio", { name: "Top left" }));
  change("Width (% of short edge)", "15");
  change("Inset (% of short edge)", "2");
  change("Opacity (%)", "50");
  fireEvent.click(screen.getByText("More"));
  tick("Add a border");
  change("Border measured on", "eachEdge");
  change("Left and right (%)", "3");
  change("Top and bottom (%)", "1.5");
  change("Border colour", "#000000");
  tick("Pad to an aspect ratio");
  change("Ratio width", "4");
  change("Ratio height", "5");
  change("Canvas colour", "#eeeeee");
  tick("Also export each mask");
}

const everyExportSetting = {
  jpegQuality: 72,
  preserveTimestamps: false,
  resize: { mode: "megapixels", value: 2.5, dontEnlarge: false },
  border: {
    basis: "eachEdge",
    horizontalPercent: 3,
    verticalPercent: 1.5,
    color: "#000000",
  },
  pad: { ratioWidth: 4, ratioHeight: 5, color: "#eeeeee" },
  watermark: {
    path: "/marks/logo.png",
    anchor: "topLeft",
    scale: 15,
    spacing: 2,
    opacity: 50,
  },
  exportMasks: true,
  preserveFolders: true,
  destinationType: "customFolder",
  subfolder: "Web/small",
};

it("exports with every setting of the former advanced panel", async () => {
  render(<StudioExport />);
  await fillEverything();
  fireEvent.click(
    screen.getByRole("button", { name: "Choose destination & export" }),
  );
  await vi.waitFor(() =>
    expect(exported()).toContainEqual(expect.objectContaining({
        outputFolderOrFile: "/out",
        isExplicitFilePath: false,
        outputFormat: "webp",
        exportSettings: expect.objectContaining(everyExportSetting),
      }),
    ),
  );
});

it("saves every setting in a recipe and exports it again from the recipe", async () => {
  const handleSettingsChange = vi.fn(async () => {});
  useSettingsStore.setState({
    appSettings: { exportPresets: [] } as unknown as AppSettings,
    handleSettingsChange,
  });
  const { unmount } = render(<StudioExport />);
  await fillEverything();
  fireEvent.change(screen.getByLabelText("New recipe name"), {
    target: { value: "Web" },
  });
  fireEvent.click(screen.getByRole("button", { name: "Save as recipe" }));
  const saved = (
    handleSettingsChange.mock.calls[0] as unknown as [AppSettings]
  )[0].exportPresets![0];
  expect(saved).toMatchObject({
    fileFormat: "webp",
    jpegQuality: 72,
    resizeMode: "megapixels",
    resizeValue: 2.5,
    dontEnlarge: false,
    subfolder: "Web/small",
    preserveFolders: true,
    preserveTimestamps: false,
    exportMasks: true,
    enableWatermark: true,
    watermarkPath: "/marks/logo.png",
    watermarkAnchor: "topLeft",
    watermarkScale: 15,
    watermarkSpacing: 2,
    watermarkOpacity: 50,
    enableBorder: true,
    borderBasis: "eachEdge",
    borderHorizontalPercent: 3,
    borderVerticalPercent: 1.5,
    borderColor: "#000000",
    enablePad: true,
    padRatioWidth: 4,
    padRatioHeight: 5,
    padColor: "#eeeeee",
  });
  unmount();
  // A fresh form, the recipe ticked: its settings reach the export.
  useSettingsStore.setState({
    appSettings: { exportPresets: [saved] } as unknown as AppSettings,
  });
  localStorage.removeItem("rawtonic.export.recipes");
  render(<StudioExport />);
  fireEvent.click(screen.getByRole("checkbox", { name: "Export with Web" }));
  fireEvent.click(screen.getByRole("button", { name: "Export with Web" }));
  await vi.waitFor(() =>
    expect(exported()).toContainEqual(expect.objectContaining({
        outputFormat: "webp",
        exportSettings: expect.objectContaining(everyExportSetting),
      }),
    ),
  );
  // Showing the recipe fills the form with it.
  fireEvent.click(screen.getByRole("button", { name: "Web" }));
  expect((screen.getByLabelText("Format") as HTMLSelectElement).value).toBe(
    "webp",
  );
  expect(
    (screen.getByLabelText("Megapixels (MP)") as HTMLInputElement).value,
  ).toBe("2.5");
  expect(
    (screen.getByRole("radio", { name: "Top left" }) as HTMLInputElement)
      .checked,
  ).toBe(true);
});

it("saves a shown recipe's changes to it, so a ticked recipe exports what it shows", async () => {
  const web = presetFromDraft(
    { ...DEFAULT_RECIPE_DRAFT, quality: 80 },
    "web",
    "Web",
  );
  const handleSettingsChange = vi.fn(async (settings: AppSettings) => {
    useSettingsStore.setState({ appSettings: settings });
  });
  useSettingsStore.setState({
    appSettings: { exportPresets: [web] } as unknown as AppSettings,
    handleSettingsChange,
  });
  render(<StudioExport />);
  fireEvent.change(screen.getByLabelText("JPEG quality"), {
    target: { value: "70" },
  });
  fireEvent.click(screen.getByRole("checkbox", { name: "Export with Web" }));
  // The unsaved settings are named as such, and not exported now.
  expect(
    screen.getByText(
      "Current settings. The ticked recipes export with their own settings.",
    ),
  ).toBeTruthy();
  const plan = screen.getByRole("region", { name: "This export" });
  expect(plan.textContent).toContain("Web");
  expect(plan.textContent).toContain("quality 80");
  // Showing the recipe and changing it changes the plan at once and the
  // saved recipe shortly after; no separate update step.
  fireEvent.click(screen.getByRole("button", { name: "Web" }));
  expect(screen.getByText("Editing the recipe Web. Changes are saved to it.")).toBeTruthy();
  fireEvent.change(screen.getByLabelText("JPEG quality"), {
    target: { value: "100" },
  });
  expect(plan.textContent).toContain("quality 100");
  await vi.waitFor(() =>
    expect(
      useSettingsStore.getState().appSettings?.exportPresets?.[0].jpegQuality,
    ).toBe(100),
  );
  // Current settings come back as they were left.
  fireEvent.click(screen.getByRole("button", { name: "Current settings" }));
  expect((screen.getByLabelText("JPEG quality") as HTMLInputElement).value).toBe(
    "70",
  );
});

it("offers every image format the export writes", () => {
  render(<StudioExport />);
  const formats = Array.from(
    (screen.getByLabelText("Format") as HTMLSelectElement).options,
  ).map((o) => o.value);
  expect(formats).toEqual([
    "jpg",
    "tiff",
    "png",
    "webp",
    "jxl",
    "avif",
    "cube",
  ]);
  fireEvent.change(screen.getByLabelText("Format"), {
    target: { value: "jxl" },
  });
  expect(screen.getByText("Quality 100 is lossless.")).toBeTruthy();
  fireEvent.change(screen.getByLabelText("Format"), {
    target: { value: "avif" },
  });
  expect(screen.getByLabelText("AVIF quality")).toBeTruthy();
});

it("resizes by long edge, short edge, width, height, megapixels or percentage", async () => {
  render(<StudioExport />);
  fireEvent.click(screen.getByLabelText("Resize to fit"));
  const modes = Array.from(
    (screen.getByLabelText("Resize mode") as HTMLSelectElement).options,
  ).map((o) => o.value);
  expect(modes).toEqual([
    "longEdge",
    "shortEdge",
    "width",
    "height",
    "dimensions",
    "megapixels",
    "percentage",
  ]);
  fireEvent.change(screen.getByLabelText("Resize mode"), {
    target: { value: "percentage" },
  });
  // Another unit starts from a sensible value instead of 3000 %.
  expect(
    (screen.getByLabelText("Percentage (%)") as HTMLInputElement).value,
  ).toBe("50");
  fireEvent.click(
    screen.getByRole("button", { name: "Choose destination & export" }),
  );
  await vi.waitFor(() =>
    expect(exported()).toContainEqual(expect.objectContaining({
        exportSettings: expect.objectContaining({
          resize: { mode: "percentage", value: 50, dontEnlarge: true },
        }),
      }),
    ),
  );
});

it("holds the export until a watermark image is chosen or a subfolder is valid", () => {
  render(<StudioExport />);
  const submit = () =>
    screen.getByRole("button", {
      name: "Choose destination & export",
    }) as HTMLButtonElement;
  fireEvent.click(screen.getByLabelText("Add a watermark"));
  expect(screen.getByRole("alert").textContent).toMatch(
    /Choose a watermark image/,
  );
  expect(submit().disabled).toBe(true);
  fireEvent.click(screen.getByLabelText("Add a watermark"));
  expect(submit().disabled).toBe(false);
  fireEvent.change(screen.getByLabelText("Subfolder"), {
    target: { value: "../elsewhere" },
  });
  expect(screen.getByRole("alert").textContent).toMatch(
    /inside the destination/,
  );
  expect(submit().disabled).toBe(true);
});

it("shows the estimated file size for the current settings", async () => {
  vi.mocked(invoke).mockImplementation(
    (command: string) =>
      Promise.resolve(
        command === "estimate_export_sizes" ? 12_400_000 : undefined,
      ) as never,
  );
  render(<StudioExport />);
  expect(
    await screen.findByText(
      "Estimated size: about 12.4 MB (6.2 MB per file)",
      {},
      { timeout: 2000 },
    ),
  ).toBeTruthy();
  expect(invoke).toHaveBeenCalledWith(
    "estimate_export_sizes",
    expect.objectContaining({
      paths: ["/p/a.RAF", "/p/b.RAF"],
      outputFormat: "jpg",
    }),
  );
  vi.mocked(invoke).mockImplementation(() => Promise.resolve() as never);
});

it("offers output sharpening for screen and for print", () => {
  render(<StudioExport />);
  expect(screen.queryByLabelText("Sharpening amount")).toBeNull();
  fireEvent.change(screen.getByLabelText("Sharpen for"), {
    target: { value: "screen" },
  });
  expect(
    (screen.getByLabelText("Sharpening radius") as HTMLInputElement).value,
  ).toBe("0.8");
  fireEvent.change(screen.getByLabelText("Sharpen for"), {
    target: { value: "print" },
  });
  expect(screen.queryByLabelText("Sharpening radius")).toBeNull();
  fireEvent.change(screen.getByLabelText("Viewing distance (cm)"), {
    target: { value: "100" },
  });
  expect(screen.getByText(/Radius 3\.4 px/)).toBeTruthy();
});

it("resizes to a box turned to each photo", () => {
  render(<StudioExport />);
  fireEvent.click(screen.getByLabelText("Resize to fit"));
  fireEvent.change(screen.getByLabelText("Resize mode"), {
    target: { value: "dimensions" },
  });
  expect(
    (screen.getByLabelText("Dimensions (px)") as HTMLInputElement).value,
  ).toBe("3000");
  expect(
    (screen.getByLabelText("Dimensions other side (px)") as HTMLInputElement)
      .value,
  ).toBe("2000");
  expect(screen.getByText(/turned to each photo's orientation/)).toBeTruthy();
});

it("makes a text watermark in an installed font", async () => {
  vi.mocked(invoke).mockImplementation(
    (name: string) =>
      Promise.resolve(
        name === "list_font_families"
          ? ["Avenir Next", "Helvetica Neue"]
          : undefined,
      ) as never,
  );
  render(<StudioExport />);
  fireEvent.click(screen.getByLabelText("Add a watermark"));
  fireEvent.change(screen.getByLabelText("Watermark kind"), {
    target: { value: "text" },
  });
  const font = (
    await screen.findByRole("option", { name: "Avenir Next" })
  ).closest("select")!;
  fireEvent.change(font, { target: { value: "Avenir Next" } });
  fireEvent.change(screen.getByLabelText("Watermark text"), {
    target: { value: "" },
  });
  expect(
    screen.getByText("Enter the watermark text or turn the watermark off."),
  ).toBeTruthy();
  fireEvent.change(screen.getByLabelText("Watermark text"), {
    target: { value: "© Studio" },
  });
  expect(
    screen.queryByText("Enter the watermark text or turn the watermark off."),
  ).toBeNull();
});

it("exports into a wide colour space in every image format", async () => {
  useLibraryStore.setState({ sessionDirectory: "/job" });
  render(<StudioExport />);
  const space = screen.getByLabelText("Colour space") as HTMLSelectElement;
  expect(space.value).toBe("srgb");
  fireEvent.change(space, { target: { value: "proPhoto" } });
  expect(screen.getByText(/ProPhoto RGB needs 16-bit TIFF/)).toBeTruthy();
  fireEvent.click(screen.getByRole("button", { name: "Export" }));
  await vi.waitFor(() =>
    expect(exported()).toContainEqual(expect.objectContaining({
        exportSettings: expect.objectContaining({ colorSpace: "proPhoto" }),
      }),
    ),
  );
  fireEvent.change(screen.getByLabelText("Format"), {
    target: { value: "avif" },
  });
  expect(space.disabled).toBe(false);
  expect(space.value).toBe("proPhoto");
});

it("resolves date naming presets through the export engine even for a single photo", async () => {
  useLibraryStore.setState({ multiSelectedPaths: ["/p/a.RAF"] });
  render(<StudioExport />);
  fireEvent.change(screen.getByLabelText("Naming preset"), { target: { value: "builtin:1" } });
  fireEvent.click(screen.getByRole("button", { name: "Choose destination & export" }));
  await vi.waitFor(() => expect(exported()).toContainEqual(expect.objectContaining({
    isExplicitFilePath: false,
    exportSettings: expect.objectContaining({ filenameTemplate: "{YYYY}{MM}{DD}_{original_filename}" }),
  })));
  const { save } = await import("@tauri-apps/plugin-dialog");
  expect(save).not.toHaveBeenCalled();
});

it("sends a custom counter start to batch exports and uses it in the single-file save dialog", async () => {
  render(<StudioExport />);
  fireEvent.change(screen.getByLabelText("Filename template"), { target: { value: "Image_{sequence}" } });
  fireEvent.change(screen.getByLabelText("Counter starts at"), { target: { value: "98" } });
  fireEvent.click(screen.getByRole("button", { name: "Choose destination & export" }));
  await vi.waitFor(() => expect(exported()).toContainEqual(expect.objectContaining({
    exportSettings: expect.objectContaining({ sequenceStart: 98 }),
  })));
  cleanup();
  useProcessStore.setState({ exportState: { status: Status.Idle, errorMessage: "", progress: { current: 0, total: 0 } } });
  useLibraryStore.setState({ multiSelectedPaths: ["/p/a.RAF"] });
  render(<StudioExport />);
  fireEvent.change(screen.getByLabelText("Filename template"), { target: { value: "Image_{sequence}" } });
  fireEvent.change(screen.getByLabelText("Counter starts at"), { target: { value: "0" } });
  fireEvent.click(screen.getByRole("button", { name: "Choose destination & export" }));
  const { save } = await import("@tauri-apps/plugin-dialog");
  await vi.waitFor(() => expect(save).toHaveBeenCalledWith(expect.objectContaining({ defaultPath: "Image_0.jpg" })));
});

it("shows a translated summary with backend export details as secondary text", () => {
  useProcessStore.setState({ exportState: { status: Status.Error, errorMessage: "EIO: cannot write", progress: { current: 0, total: 0 } } });
  render(<StudioExport />);
  const alert = screen.getByRole("alert");
  expect(within(alert).getByText("The operation could not be completed.")).toBeTruthy();
  expect(within(alert).getByText("EIO: cannot write").tagName).toBe("SMALL");
});


it("retains specific German translations for known export failures", async () => {
  await i18n.changeLanguage("de");
  try {
    useProcessStore.setState({ exportState: { status: Status.Error, errorMessage: "3 of 12 exports failed", progress: { current: 0, total: 0 } } });
    render(<StudioExport />);
    const alert = screen.getByRole("alert");
    expect(alert.textContent).toContain("Der Vorgang konnte nicht abgeschlossen werden.");
    expect(within(alert).getByText("3 von 12 Exporten fehlgeschlagen").tagName).toBe("SMALL");
  } finally {
    await i18n.changeLanguage("en");
  }
});

it("discloses retained contacts and offers explicit contact removal when stripping metadata", () => {
  useSettingsStore.setState({ appSettings: { exportPresets: [] } as unknown as AppSettings });
  render(<StudioExport />);
  fireEvent.click(screen.getByLabelText("Include metadata"));
  expect(screen.getByText(/Stripped exports retain creator, copyright/).textContent).toContain("postal address");
  const remove = screen.getByLabelText("Remove creator contact details") as HTMLInputElement;
  expect(remove.disabled).toBe(false);
  expect(remove.checked).toBe(false);
  fireEvent.click(remove);
  expect(remove.checked).toBe(true);
});
