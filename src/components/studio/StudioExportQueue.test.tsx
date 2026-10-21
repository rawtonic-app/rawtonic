import "../../i18n";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import { act, cleanup, fireEvent, render, screen, within } from "@testing-library/react";
import { invoke } from "@tauri-apps/api/core";
import { ExportQueuePanel, describeExportItem } from "./StudioExportQueue";
import { useProcessStore, type ExportItem, type ExportJob } from "../../store/useProcessStore";

let items: ExportItem[] = [];
vi.mock("@tauri-apps/api/core", () => ({
  invoke: vi.fn((command: string) => Promise.resolve(command === "export_queue_items" ? items : undefined)),
}));
vi.mock("react-toastify", () => ({ toast: { error: vi.fn() } }));

const job = (part: Partial<ExportJob>): ExportJob => ({
  id: "j1",
  label: "4 photos as JPG",
  state: "running",
  createdAt: "",
  total: 4,
  done: 1,
  failed: 0,
  failures: [],
  ...part,
});
const item = (index: number, part: Partial<ExportItem>): ExportItem => ({
  index,
  path: `/p/${index}.RAF`,
  state: "pending",
  output: null,
  error: null,
  attempts: 0,
  startedMs: null,
  finishedMs: null,
  ...part,
});

beforeEach(() => {
  vi.clearAllMocks();
  items = [];
  useProcessStore.setState({ exportQueue: [], exportQueueHeld: false, exportItems: {}, exportPausing: [] });
});
afterEach(() => {
  cleanup();
  vi.useRealTimers();
});

it("says what each photo is doing, AI Denoise with its share done", () => {
  expect(describeExportItem({ ...item(0, { state: "running" }), stage: "denoising", fraction: 0.423 })).toBe(
    "AI Denoise 42 %",
  );
  expect(describeExportItem({ ...item(0, { state: "running" }), stage: "loading" })).toBe("Reading the photo…");
  expect(describeExportItem(item(0, { state: "done", startedMs: 1000, finishedMs: 4250 }))).toBe("Done in 3.3 s");
  expect(describeExportItem(item(0, { state: "failed", error: "Decoder said no" }))).toBe("Decoder said no");
  expect(describeExportItem(item(0, {}))).toBe("Waiting");
});

it("lists every job newest first, empty or not, and acts on single jobs", () => {
  const { unmount } = render(<ExportQueuePanel onClose={vi.fn()} />);
  expect(screen.getByText(/No exports yet/)).toBeTruthy();
  unmount();
  useProcessStore.setState({
    exportQueue: [
      job({ id: "old", label: "Old", state: "done", done: 4 }),
      job({}),
      job({ id: "next", label: "Next", state: "queued", done: 0 }),
      job({ id: "held", label: "Held", state: "paused", done: 2 }),
    ],
  });
  render(<ExportQueuePanel onClose={vi.fn()} />);
  const rows = within(screen.getByRole("region", { name: "Export queue" })).getAllByRole("listitem");
  expect(rows.map((row) => row.querySelector("span")?.firstChild?.textContent)).toEqual([
    "Held",
    "Next",
    "4 photos as JPG",
    "Old",
  ]);
  const running = within(rows[2]);
  // A running job is paused or cancelled, never removed.
  expect(running.queryByRole("button", { name: "Remove" })).toBeNull();
  fireEvent.click(running.getByRole("button", { name: "Pause" }));
  expect(invoke).toHaveBeenCalledWith("export_queue_pause", { job: "j1" });
  fireEvent.click(running.getByRole("button", { name: "Cancel" }));
  expect(invoke).toHaveBeenCalledWith("export_queue_cancel", { job: "j1" });
  fireEvent.click(within(rows[0]).getByRole("button", { name: "Resume" }));
  expect(invoke).toHaveBeenCalledWith("export_queue_resume", { job: "held" });
  expect(within(rows[0]).getByText("Paused (2 of 4 exported)")).toBeTruthy();
  // A waiting job starts by itself: no Start unless the queue is held.
  expect(within(rows[1]).queryByRole("button", { name: "Start" })).toBeNull();
  fireEvent.click(screen.getByRole("button", { name: "Hold queue" }));
  expect(invoke).toHaveBeenCalledWith("export_queue_hold", { held: true });
  fireEvent.click(screen.getByRole("button", { name: "Clear finished" }));
  expect(invoke).toHaveBeenCalledWith("export_queue_remove", { job: "old" });
  expect(invoke).not.toHaveBeenCalledWith("export_queue_remove", { job: "j1" });
});

it("offers Start on waiting jobs and the queue once it is held", () => {
  useProcessStore.setState({ exportQueue: [job({ state: "queued", done: 0 })], exportQueueHeld: true });
  render(<ExportQueuePanel onClose={vi.fn()} />);
  expect(screen.getByText(/The queue is held/)).toBeTruthy();
  fireEvent.click(screen.getByRole("button", { name: "Start" }));
  expect(invoke).toHaveBeenCalledWith("export_queue_resume", { job: "j1" });
  fireEvent.click(screen.getByRole("button", { name: "Start queue" }));
  expect(invoke).toHaveBeenCalledWith("export_queue_hold", { held: false });
});

it("shows a job's photos live, with time left, reasons and their files", async () => {
  vi.useFakeTimers({ now: 20_000, toFake: ["Date", "setInterval", "clearInterval"] });
  items = [
    item(0, { state: "done", startedMs: 0, finishedMs: 10_000, output: "/out/0.jpg" }),
    item(1, { state: "failed", error: "Decoder said no", startedMs: 0, finishedMs: 100 }),
    item(2, { state: "running", startedMs: 15_000 }),
    item(3, {}),
  ];
  useProcessStore.setState({ exportQueue: [job({ done: 0, failed: 1, failures: [{ path: "/p/1.RAF", error: "Decoder said no" }] })] });
  render(<ExportQueuePanel onClose={vi.fn()} />);
  expect(screen.getByText(/estimating time left/)).toBeTruthy();
  fireEvent.click(screen.getByRole("button", { name: "4 photos" }));
  expect(invoke).toHaveBeenCalledWith("export_queue_items", { job: "j1" });
  const photos = await screen.findByRole("list", { name: "Photos of 4 photos as JPG" });
  expect(within(photos).getAllByRole("listitem").map((row) => row.textContent)).toEqual([
    "0.RAFDone in 10.0 sShow",
    "1.RAFDecoder said no",
    "2.RAFReading the photo…",
    "3.RAFWaiting",
  ]);
  // Its exported files can be revealed while the rest run.
  expect(screen.getAllByRole("button", { name: "Show" }).length).toBe(2);
  // Counted from the photos, not the job's count from before the run.
  expect(screen.getByText(/Exporting \(1 of 4 exported\)/)).toBeTruthy();
  // (5 s of photo 2 + 10 s for photo 3) on one worker.
  expect(screen.getByText(/about 15 s left/)).toBeTruthy();
  act(() =>
    useProcessStore.getState().applyExportItemProgress({
      job: "j1",
      item: items[2],
      stage: "denoising",
      fraction: 0.5,
    }),
  );
  expect(within(photos).getByText("AI Denoise 50 %")).toBeTruthy();
  expect(within(photos).getByRole("progressbar", { name: "AI Denoise progress" })).toBeTruthy();
  act(() => vi.advanceTimersByTime(5000));
  expect(screen.getByText(/about 10 s left/)).toBeTruthy();
  fireEvent.click(within(photos).getByRole("button", { name: "Show" }));
  expect(invoke).toHaveBeenCalledWith("show_in_finder", { path: "/out/0.jpg" });
});

it("reveals a finished job's files and closes with Escape", async () => {
  items = [item(0, { state: "failed", error: "x" }), item(1, { state: "done", output: "/out/1.jpg" })];
  const onClose = vi.fn();
  useProcessStore.setState({ exportQueue: [job({ state: "done", done: 1 })] });
  render(<ExportQueuePanel onClose={onClose} />);
  fireEvent.click(screen.getByRole("button", { name: "Show" }));
  await vi.waitFor(() => expect(invoke).toHaveBeenCalledWith("show_in_finder", { path: "/out/1.jpg" }));
  fireEvent.keyDown(screen.getByRole("button", { name: "Show" }), { key: "Escape" });
  expect(onClose).toHaveBeenCalled();
});
