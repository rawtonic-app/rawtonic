import "../../i18n";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { act, cleanup, fireEvent, render, screen } from "@testing-library/react";
import { invoke } from "@tauri-apps/api/core";
import {
  StudioExif,
  exifLabel,
  gpsDegrees,
  StudioKeywords,
  StudioMetadataTool,
  useTextMetadataCache,
  keywordCounts,
  parseKeywords,
  sharedValue,
  formatBytes,
} from "./StudioMetadata";
import { useLibraryStore } from "../../store/useLibraryStore";
import { toast } from "react-toastify";
import { useEditorStore } from "../../store/useEditorStore";
import { PHOTO_WRITES_SETTLED } from "../../utils/photoWrites";
import { useSettingsStore } from "../../store/useSettingsStore";
import type { AppSettings, ImageFile } from "../ui/AppProperties";

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn() }));
vi.mock("react-toastify", () => ({ toast: { error: vi.fn() } }));
vi.mock("@tauri-apps/plugin-shell", () => ({ open: vi.fn() }));
const mockInvoke = vi.mocked(invoke);

const file = (path: string, tags: string[] | null): ImageFile => ({
  path,
  tags,
  rating: 0,
  modified: 0,
  is_edited: false,
  exif: null,
  is_virtual_copy: false,
  is_cloud_placeholder: false,
  is_raw: true,
  group_id: null,
});
const empty = { title: "", description: "", creator: "", copyright: "" };

beforeEach(() => {
  mockInvoke.mockReset();
  useTextMetadataCache.setState({ session: null, loaded: false, byPath: {} });
  useEditorStore.setState({ selectedImage: null });
  useLibraryStore.setState({
    sessionDirectory: "/job",
    imageList: [
      file("/job/Capture/a.arw", ["user:beach", "dog", "color:red"]),
      file("/job/Capture/b.arw", ["user:beach"]),
      file("/job/Capture/c.arw", null),
    ],
    multiSelectedPaths: ["/job/Capture/a.arw", "/job/Capture/b.arw"],
    libraryActivePath: "/job/Capture/a.arw",
  });
});
afterEach(cleanup);

describe("keyword helpers", () => {
  it("counts keywords per photograph without labels or the typed-keyword marker", () => {
    expect(
      keywordCounts([["user:beach", "dog", "color:red", "user:Dog"], ["user:beach"]]).map(
        ({ keyword, count }) => [keyword, count],
      ),
    ).toEqual([
      ["beach", 2],
      ["dog", 1],
    ]);
  });
  it("splits typed keywords at commas and drops duplicates", () => {
    expect(parseKeywords(" harbour, Pier ,, harbour ,pier")).toEqual(["harbour", "Pier"]);
  });
  it("reports mixed values as null", () => {
    expect(sharedValue([{ ...empty, title: "A" }, { ...empty, title: "A" }], "title")).toBe("A");
    expect(sharedValue([{ ...empty, title: "A" }, undefined], "title")).toBeNull();
  });
});

describe("Keywords tool", () => {
  it("marks keywords on part of the selection and adds typed keywords to each photograph", async () => {
    mockInvoke.mockResolvedValue(undefined);
    render(<StudioKeywords />);
    expect(screen.getByText("beach").className).not.toContain("is-partial");
    expect(screen.getByText("dog").closest(".studio-keyword")?.className).toContain("is-partial");
    const input = screen.getByLabelText("Add keywords");
    fireEvent.change(input, { target: { value: "harbour, Dog" } });
    await act(async () => {
      fireEvent.keyDown(input, { key: "Enter" });
    });
    const paths = ["/job/Capture/a.arw", "/job/Capture/b.arw"];
    expect(mockInvoke).toHaveBeenCalledWith("add_tag_for_paths", { paths, tag: "user:harbour" });
    // Sent again for the whole selection: a photograph of a group may have
    // saved without its variant, so the library names the failed ones among
    // all of it (adding is idempotent).
    expect(mockInvoke).toHaveBeenCalledWith("add_tag_for_paths", { paths, tag: "user:Dog" });
    const tags = Object.fromEntries(useLibraryStore.getState().imageList.map((f) => [f.path, f.tags]));
    // Each photograph keeps its own keywords; an existing keyword is not doubled.
    expect(tags["/job/Capture/a.arw"]).toEqual(["color:red", "dog", "user:beach", "user:harbour"]);
    expect(tags["/job/Capture/b.arw"]).toEqual(["user:Dog", "user:beach", "user:harbour"]);
    expect(tags["/job/Capture/c.arw"]).toBeNull();
  });

  it("shows a keyword on the photographs that were saved when the batch partly failed, also in a group", async () => {
    // RAW+JPEG grouped as one tile (RAW preferred, as the library shows them):
    // selecting it acts on both, and only the RAW fails.
    useLibraryStore.setState({
      imageList: [
        { ...file("/job/Capture/a.arw", ["dog"]), group_id: "g1" },
        { ...file("/job/Capture/a.jpg", ["dog"]), group_id: "g1", is_raw: false },
        file("/job/Capture/c.arw", null),
      ],
      multiSelectedPaths: ["/job/Capture/a.arw"],
    });
    useSettingsStore.setState({ appSettings: { grouping: "raw" } as unknown as AppSettings });
    mockInvoke.mockRejectedValue({ message: "1 of 2 photos not saved", failed: ["/job/Capture/a.arw"] });
    render(<StudioKeywords />);
    const input = screen.getByLabelText("Add keywords");
    fireEvent.change(input, { target: { value: "harbour" } });
    await act(async () => {
      fireEvent.keyDown(input, { key: "Enter" });
    });
    expect(mockInvoke).toHaveBeenCalledWith("add_tag_for_paths", {
      paths: ["/job/Capture/a.arw", "/job/Capture/a.jpg"],
      tag: "user:harbour",
    });
    await vi.waitFor(() => expect(toast.error).toHaveBeenCalled());
    // The saved JPEG has it, the failed RAW keeps its own keywords (read at
    // once: unmounting puts back what the file header says of the group).
    const tags = Object.fromEntries(useLibraryStore.getState().imageList.map((f) => [f.path, f.tags]));
    expect(tags["/job/Capture/a.arw"]).toEqual(["dog"]);
    expect(tags["/job/Capture/a.jpg"]).toEqual(["dog", "user:harbour"]);
  });

  it("removes every stored form of a keyword", async () => {
    useLibraryStore.setState({
      imageList: [file("/job/Capture/a.arw", ["dog"]), file("/job/Capture/b.arw", ["user:dog"])],
    });
    mockInvoke.mockResolvedValue(undefined);
    render(<StudioKeywords />);
    await act(async () => {
      fireEvent.click(screen.getByLabelText("Remove keyword dog"));
    });
    const paths = ["/job/Capture/a.arw", "/job/Capture/b.arw"];
    expect(mockInvoke).toHaveBeenCalledWith("remove_tag_for_paths", { paths, tag: "dog" });
    expect(mockInvoke).toHaveBeenCalledWith("remove_tag_for_paths", { paths, tag: "user:dog" });
    expect(useLibraryStore.getState().imageList.every((f) => f.tags === null)).toBe(true);
  });
});

describe("Metadata tool", () => {
  const records = {
    "/job/Capture/a.arw": { ...empty, title: "Pier", creator: "N. F." },
    "/job/Capture/b.arw": { ...empty, title: "Harbour", creator: "N. F." },
  };

  it("shows shared values, marks mixed ones and writes only the edited field", async () => {
    mockInvoke.mockImplementation(async (command, args) =>
      command === "session_all_text_metadata"
        ? { ...records, "/job/Capture/c.arw": null }
        : command === "read_exif_for_paths"
          ? {}
          : Object.fromEntries(
            (args as { paths: string[] }).paths.map((p) => [p, { ...records["/job/Capture/a.arw"], title: "Dock" }]),
          ),
    );
    render(<StudioMetadataTool />);
    await act(async () => {});
    const title = screen.getByLabelText("Title") as HTMLInputElement;
    const creator = screen.getByLabelText("Creator") as HTMLInputElement;
    expect(title.value).toBe("");
    expect(title.placeholder).toBe("Mixed");
    expect(creator.value).toBe("N. F.");
    // Visiting a mixed field without typing keeps each photograph's title.
    fireEvent.focus(title);
    fireEvent.blur(title);
    expect(mockInvoke.mock.calls.some(([command]) => command === "session_set_text_metadata")).toBe(false);
    fireEvent.focus(title);
    fireEvent.change(title, { target: { value: "Dock" } });
    await act(async () => {
      fireEvent.keyDown(title, { key: "Enter" });
      fireEvent.blur(title);
    });
    expect(mockInvoke).toHaveBeenLastCalledWith("session_set_text_metadata", {
      directory: "/job",
      paths: ["/job/Capture/a.arw", "/job/Capture/b.arw"],
      update: { title: "Dock" },
    });
    expect(title.value).toBe("Dock");
  });

  it("edits IPTC fields for the selection like the core fields", async () => {
    const withCity = { ...empty, city: "Ascona", creatorEmail: "n@example.org" };
    mockInvoke.mockImplementation(async (command, args) =>
      command === "session_all_text_metadata"
        ? { "/job/Capture/a.arw": withCity, "/job/Capture/b.arw": { ...empty, city: "Locarno", creatorEmail: "n@example.org" } }
        : command === "read_exif_for_paths"
          ? {}
          : Object.fromEntries((args as { paths: string[] }).paths.map((p) => [p, { ...withCity, country: "Switzerland" }])),
    );
    render(<StudioMetadataTool />);
    await act(async () => {});
    const city = screen.getByLabelText("City") as HTMLInputElement;
    expect(city.placeholder).toBe("Mixed");
    expect((screen.getByLabelText("Email") as HTMLInputElement).value).toBe("n@example.org");
    const country = screen.getByLabelText("Country") as HTMLInputElement;
    fireEvent.focus(country);
    fireEvent.change(country, { target: { value: "Switzerland" } });
    await act(async () => {
      fireEvent.blur(country);
    });
    expect(mockInvoke).toHaveBeenLastCalledWith("session_set_text_metadata", {
      directory: "/job",
      paths: ["/job/Capture/a.arw", "/job/Capture/b.arw"],
      update: { country: "Switzerland" },
    });
    expect(country.value).toBe("Switzerland");
  });

  it("reads the session's metadata again when a photograph is renamed", async () => {
    const catalog: Record<string, typeof empty> = { ...records, "/job/Capture/c.arw": empty };
    mockInvoke.mockImplementation(async (command) =>
      command === "session_all_text_metadata" ? { ...catalog } : command === "read_exif_for_paths" ? {} : undefined,
    );
    render(<StudioMetadataTool />);
    await act(async () => {});
    // The rename keeps the number of photographs; the catalog has the new path.
    delete catalog["/job/Capture/a.arw"];
    catalog["/job/Capture/pier.arw"] = records["/job/Capture/a.arw"];
    await act(async () => {
      useLibraryStore.setState({
        imageList: [
          file("/job/Capture/pier.arw", null),
          file("/job/Capture/b.arw", null),
          file("/job/Capture/c.arw", null),
        ],
        multiSelectedPaths: ["/job/Capture/pier.arw"],
        libraryActivePath: "/job/Capture/pier.arw",
      });
    });
    expect(mockInvoke.mock.calls.filter(([command]) => command === "session_all_text_metadata")).toHaveLength(2);
    expect(screen.queryByText(/not part of this session/)).toBeNull();
    const title = screen.getByLabelText("Title") as HTMLInputElement;
    expect(title.disabled).toBe(false);
    expect(title.value).toBe("Pier");
  });

  it("renders without a selection or a shown photograph", () => {
    useLibraryStore.setState({ multiSelectedPaths: [], libraryActivePath: null });
    render(
      <>
        <StudioKeywords />
        <StudioMetadataTool />
      </>,
    );
    expect(screen.getByText(/see their metadata/)).toBeTruthy();
    expect(screen.getByText(/to add keywords/)).toBeTruthy();
  });

  it("edits description, creator and copyright as EXIF outside sessions; title and IPTC need one", async () => {
    useLibraryStore.setState({
      sessionDirectory: null,
      imageList: [
        { ...file("/p/a.arw", null), exif: { Artist: '"N. F."', Copyright: "default", ImageDescription: "Pier" } },
        { ...file("/p/b.arw", null), exif: { Artist: "N. F.", ImageDescription: "Dock" } },
      ],
      multiSelectedPaths: ["/p/a.arw", "/p/b.arw"],
    });
    mockInvoke.mockResolvedValue(undefined);
    render(<StudioMetadataTool />);
    expect((screen.getByLabelText("Title") as HTMLInputElement).disabled).toBe(true);
    expect(screen.getByText(/title and IPTC fields are saved in projects/)).toBeTruthy();
    const creator = screen.getByLabelText("Creator") as HTMLInputElement;
    // Quotes and a camera's "default" placeholder are not shown.
    expect(creator.value).toBe("N. F.");
    expect((screen.getByLabelText("Copyright") as HTMLInputElement).value).toBe("");
    expect((screen.getByLabelText("Description") as HTMLTextAreaElement).placeholder).toBe("Mixed");
    fireEvent.focus(creator);
    fireEvent.change(creator, { target: { value: "Alex Example" } });
    await act(async () => {
      fireEvent.blur(creator);
    });
    expect(mockInvoke).toHaveBeenCalledWith("update_exif_fields", {
      paths: ["/p/a.arw", "/p/b.arw"],
      updates: { Artist: "Alex Example" },
    });
    expect(useLibraryStore.getState().imageList.every((image) => image.exif?.Artist === "Alex Example")).toBe(true);
    expect(mockInvoke.mock.calls.some(([command]) => String(command).startsWith("session_"))).toBe(false);
  });

  it("keeps the comment as EXIF UserComment in sessions too", async () => {
    useLibraryStore.setState({
      imageList: [{ ...file("/job/Capture/a.arw", null), exif: { UserComment: "Note" } }],
      multiSelectedPaths: ["/job/Capture/a.arw"],
    });
    mockInvoke.mockImplementation(async (command) =>
      command === "session_all_text_metadata" ? { "/job/Capture/a.arw": empty } : undefined,
    );
    render(<StudioMetadataTool />);
    await act(async () => {});
    const comment = screen.getByLabelText("Comment") as HTMLTextAreaElement;
    expect(comment.value).toBe("Note");
    fireEvent.focus(comment);
    fireEvent.change(comment, { target: { value: "Second take" } });
    await act(async () => {
      fireEvent.blur(comment);
    });
    expect(mockInvoke).toHaveBeenCalledWith("update_exif_fields", {
      paths: ["/job/Capture/a.arw"],
      updates: { UserComment: "Second take" },
    });
    // The creator stays with the session's Metadata tool.
    expect(mockInvoke.mock.calls.some(([command, args]) => command === "update_exif_fields" && "Artist" in (args as { updates: object }).updates)).toBe(false);
  });

  it("reads missing EXIF of the selection before its fields can be edited", async () => {
    useLibraryStore.setState({ sessionDirectory: null });
    mockInvoke.mockImplementation(async (command, args) =>
      command === "read_exif_for_paths"
        ? Object.fromEntries((args as { paths: string[] }).paths.map((p) => [p, { Artist: "A" }]))
        : undefined,
    );
    render(<StudioMetadataTool />);
    expect((screen.getByLabelText("Creator") as HTMLInputElement).disabled).toBe(true);
    await act(async () => {});
    expect(mockInvoke).toHaveBeenCalledWith("read_exif_for_paths", { paths: ["/job/Capture/a.arw", "/job/Capture/b.arw"] });
    expect((screen.getByLabelText("Creator") as HTMLInputElement).disabled).toBe(false);
    expect((screen.getByLabelText("Creator") as HTMLInputElement).value).toBe("A");
  });

  it("sets the colour label of the selection and shows the shared one", async () => {
    mockInvoke.mockResolvedValue(undefined);
    useLibraryStore.setState({
      imageList: [file("/job/Capture/a.arw", ["color:red"]), file("/job/Capture/b.arw", ["color:red", "user:x"])],
    });
    render(<StudioMetadataTool />);
    expect(screen.getByLabelText("Red label").getAttribute("aria-pressed")).toBe("true");
    await act(async () => {
      fireEvent.click(screen.getByLabelText("Blue label"));
    });
    expect(mockInvoke).toHaveBeenCalledWith("set_color_label_for_paths", {
      paths: ["/job/Capture/a.arw", "/job/Capture/b.arw"],
      color: "blue",
      directory: "/job",
    });
    expect(useLibraryStore.getState().imageList.map((image) => image.tags)).toEqual([
      ["color:blue"],
      ["user:x", "color:blue"],
    ]);
    expect(screen.getByLabelText("Blue label").getAttribute("aria-pressed")).toBe("true");
  });

  it("shows the file's format, size, pixel size and megapixels, and virtual copies", async () => {
    useLibraryStore.setState({ multiSelectedPaths: [] });
    useEditorStore.setState({
      selectedImage: { path: "/job/Capture/a.arw?vc=2", width: 6000, height: 4000, exif: {} } as never,
    });
    mockInvoke.mockImplementation(async (command) => (command === "file_size" ? 24_576_000 : undefined));
    render(<StudioMetadataTool />);
    await act(async () => {});
    expect(mockInvoke).toHaveBeenCalledWith("file_size", { path: "/job/Capture/a.arw" });
    const details = document.querySelector(".studio-metadata-details")!.textContent;
    expect(details).toContain("ARW · virtual copy");
    expect(details).toContain("24.6 MB");
    expect(details).toContain("6000 × 4000 · 24.0 MP");
  });
});

describe("file sizes", () => {
  it("formats bytes in decimal units", () => {
    expect(formatBytes(900)).toBe("900 B");
    expect(formatBytes(1_500)).toBe("1.5 kB");
    expect(formatBytes(48_200_000)).toBe("48.2 MB");
    expect(formatBytes(312_000_000)).toBe("312 MB");
    expect(formatBytes(2_100_000_000)).toBe("2.1 GB");
  });
});

describe("EXIF tool", () => {
  it("converts GPS positions and labels", () => {
    expect(gpsDegrees("47 deg 33 min 15.54 sec", "N")).toBeCloseTo(47.554317, 5);
    expect(gpsDegrees("7 deg 33 min 44 sec", "W")).toBeCloseTo(-7.562222, 5);
    expect(gpsDegrees("47.5", "N")).toBeNull();
    expect(exifLabel("ExposureBiasValue")).toBe("Exposure Bias Value");
    expect(exifLabel("GPSLatitude")).toBe("GPS Latitude");
  });

  it("shows the position on a map only when asked for", () => {
    useEditorStore.setState({
      selectedImage: {
        path: "/job/Capture/a.arw",
        exif: { GPSLatitude: "47 deg 33 min 15.54 sec", GPSLatitudeRef: "N", GPSLongitude: "7 deg 33 min 44 sec", GPSLongitudeRef: "E" },
      } as never,
    });
    render(<StudioExif />);
    // Nothing is loaded from openstreetmap.org until the click.
    expect(document.querySelector("iframe")).toBeNull();
    fireEvent.click(screen.getByText("Show on map"));
    const src = document.querySelector("iframe")?.getAttribute("src") ?? "";
    expect(src.startsWith("https://www.openstreetmap.org/export/embed.html?bbox=")).toBe(true);
    expect(src).toMatch(/marker=47\.5543\d*%2C7\.5622\d*$/);
    expect(screen.queryByText("Show on map")).toBeNull();
  });

  it("lists the shown photograph's fields, filterable, with its position", () => {
    useEditorStore.setState({
      selectedImage: {
        path: "/job/Capture/a.arw",
        exif: {
          Model: "ILCE-7M3",
          ExposureBiasValue: "0 EV",
          GPSLatitude: "47 deg 33 min 15.54 sec",
          GPSLatitudeRef: "N",
          GPSLongitude: "7 deg 33 min 44 sec",
          GPSLongitudeRef: "E",
        },
      } as never,
    });
    render(<StudioExif />);
    expect(screen.getByText("47.554317, 7.562222")).toBeTruthy();
    expect(screen.getByText("Exposure Bias Value")).toBeTruthy();
    fireEvent.change(screen.getByLabelText("Find EXIF fields"), { target: { value: "ilce" } });
    expect(screen.queryByText("Exposure Bias Value")).toBeNull();
    expect(screen.getByText("ILCE-7M3")).toBeTruthy();
  });
});

describe("instant details", () => {
  it("reads the shown photo's EXIF from the file header with its neighbours", async () => {
    mockInvoke.mockImplementation(async (command, args) =>
      command === "read_exif_for_paths"
        ? Object.fromEntries((args as { paths: string[] }).paths.map((p) => [p, { Model: `M-${p.slice(-5)}` }]))
        : {},
    );
    render(<StudioExif />);
    await act(async () => {});
    expect(mockInvoke).toHaveBeenCalledWith("read_exif_for_paths", {
      paths: ["/job/Capture/a.arw", "/job/Capture/b.arw", "/job/Capture/c.arw"],
    });
    expect(screen.getByText("M-a.arw")).toBeTruthy();
    // The neighbours are ready for the next photo.
    expect(useLibraryStore.getState().imageList[1].exif).toEqual({ Model: "M-b.arw" });
  });
});

describe("Keywords tool and the XMP count", () => {
  // The session bar recounts its pending XMP on every settled photo write
  // (PHOTO_WRITES_SETTLED): a keyword batch must announce itself through the
  // photograph's write queue, also when it failed, or the bar never counts
  // the sidecars after it.
  const announced: string[] = [];
  const settled = (event: Event) =>
    announced.push((event as CustomEvent<{ kind: string }>).detail.kind);
  beforeEach(() => {
    announced.length = 0;
    window.addEventListener(PHOTO_WRITES_SETTLED, settled);
  });
  afterEach(() => window.removeEventListener(PHOTO_WRITES_SETTLED, settled));

  it("announces a keyword batch that failed once it settled, so the bar counts again", async () => {
    mockInvoke.mockRejectedValue({ message: "Could not save 1 of 2 photos", failed: ["/job/Capture/b.arw"] });
    render(<StudioKeywords />);
    const input = screen.getByLabelText("Add keywords");
    fireEvent.change(input, { target: { value: "harbour" } });
    await act(async () => {
      fireEvent.keyDown(input, { key: "Enter" });
    });
    expect(mockInvoke).toHaveBeenCalledWith("add_tag_for_paths", {
      paths: ["/job/Capture/a.arw", "/job/Capture/b.arw"],
      tag: "user:harbour",
    });
    expect(toast.error).toHaveBeenCalled();
    await vi.waitFor(() => expect(announced).toEqual(["keyword"]));
  });

  it("keeps the old keywords of only the photographs the library could not save", async () => {
    // The library saved the keyword for a and c, not for b.
    mockInvoke.mockRejectedValue({ message: "Could not save 1 of 2 photos", failed: ["/job/Capture/b.arw"] });
    render(<StudioKeywords />);
    const input = screen.getByLabelText("Add keywords");
    fireEvent.change(input, { target: { value: "harbour" } });
    await act(async () => {
      fireEvent.keyDown(input, { key: "Enter" });
    });
    await vi.waitFor(() => expect(toast.error).toHaveBeenCalled());
    const tags = Object.fromEntries(useLibraryStore.getState().imageList.map((f) => [f.path, f.tags]));
    // Written: the keyword is shown; not written: the word kept its own.
    expect(tags["/job/Capture/a.arw"]).toEqual(["color:red", "dog", "user:beach", "user:harbour"]);
    expect(tags["/job/Capture/b.arw"]).toEqual(["user:beach"]);
    // The toast says why, not "[object Object]".
    expect(toast.error).toHaveBeenCalledWith(expect.stringContaining("Could not save 1 of 2 photos"));
  });

  it("announces a successful keyword batch, and one waiting photograph is written in order", async () => {
    let write: (value?: unknown) => void = () => {};
    mockInvoke.mockImplementation(() => new Promise((resolve) => (write = resolve)));
    render(<StudioKeywords />);
    const input = screen.getByLabelText("Add keywords");
    fireEvent.change(input, { target: { value: "pier, harbour" } });
    await act(async () => {
      fireEvent.keyDown(input, { key: "Enter" });
    });
    // The second keyword waits for the first: one queue, not two paths.
    await vi.waitFor(() => expect(mockInvoke).toHaveBeenCalledTimes(1));
    await act(async () => write(undefined));
    await vi.waitFor(() => expect(mockInvoke).toHaveBeenCalledTimes(2));
    await act(async () => write(undefined));
    await vi.waitFor(() => expect(announced).toEqual(["keyword", "keyword"]));
    expect(toast.error).not.toHaveBeenCalled();
  });
});
