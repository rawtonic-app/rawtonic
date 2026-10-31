# QA control channel

Debug-only loopback channel for driving and inspecting a running app without screen-control tools. Idea from LightCraft's control protocol (MIT OR Apache-2.0); the code here is our own.

## Build and launch

```sh
VITE_RAWTONIC_QA=1 npm run tauri -- build --features qa-control --bundles app
open --env RAWTONIC_QA_PORT=7979 target/release/bundle/macos/Rawtonic.app
```

For latency runs add `VITE_RAWTONIC_PERF=1` and `performance-profiling`. Without the feature the native server is not compiled; without `VITE_RAWTONIC_QA=1` the webview bridge (`src/qa/qaControl.ts`) is dropped from the bundle. The server starts only when `RAWTONIC_QA_PORT` is set and listens on 127.0.0.1 only. Never ship a bundle built with either flag.

Run it against disposable sessions and a backed-up app configuration, as for every native QA run.

Linux profiling builds can compare preview transports without rebuilding:
`RAWTONIC_PREVIEW_TRANSPORT=bytes` keeps binary replies, `uri-reply` sends
large interactive JPEGs through the image protocol with a normal command reply,
and the unset default delivers their small frame IDs through a Tauri channel.
Use the same build and restored recipe for each run. Join `preview_reply_ready`
and frontend roundtrip records by `sequence`; image availability is recorded
separately by `browser_preview_load`. A quicker frame ID reply alone does not
prove faster image display or pointer-to-photon latency.

## Protocol

One JSON object per line: `{"id":1,"method":"…","params":{…}}` → `{"id":1,"result":…}` or `{"id":1,"error":"…"}`. Coordinates are CSS pixels from the webview's top left (the borderless window's content).

| Method | Side | What it does |
|---|---|---|
| `ping` | native | Version check. |
| `window.info` | native | Logical position/size, content-view size, scale, focus, macOS window number; `color`: the window's colour space and each Metal layer's colour space (`null` = none set), pixel format and EDR flag. |
| `window.capture` | native, macOS | PNG (base64) of the window as the window server composites it, including the GPU layer under the webview; `resolution` `nominal` (points) or `best` (backing pixels); `color_space` `srgb` (default) or `p3` (keeps colours beyond sRGB). |
| `input.mouse` | native, macOS | One `NSEvent` (`type` move/down/drag/up, `x`, `y`, `modifiers`, `button` left/right). |
| `input.click` | native, macOS | Hover, press, release (`count` 2 = double click; `button: "right"` opens context menus). |
| `input.drag` | native, macOS | Hover, press, one drag event per point, release; `interval_ms` (default 16.7) on an absolute schedule. |
| `ai.memory_pressure` | native | Runs what the app does when the system reports memory pressure (`ai_lifecycle`: AI models and their caches dropped, background mask runs cancelled); returns what was unloaded. The kernel's notification itself cannot be raised without root. |
| `ui.elements` / `ui.find` | webview | Visible interactive elements filtered by `name` (substring, or `exact`), `role`/tag or `selector`; name = aria-label, title, placeholder, alt or text; rectangles in CSS pixels. |
| `ui.click` / `ui.drag` | webview | Synthetic DOM pointer/mouse events (untrusted). For Linux until native injection exists there. `ui.click` with `button: "right"` sends a secondary press and `contextmenu` (the app's own context menus). |
| `ui.hover` | webview | Synthetic pointer moves without a pressed button over `points` (`interval_ms`, default 60 Hz): hover-driven UI (the picker loupe, the pointer readout) in a background window, where native moves are not delivered. No CSS `:hover`. |
| `ui.key` | webview | `keydown`/`keyup` on the focused element (`key`, `code`, `modifiers`; `phase` `down` or `up` sends one half, to hold a key). |
| `ui.type` | webview | Text into a field (`name`/`selector`, a matched label stands for its control; else the focused element): focus, value via the native setter, `input` event; `append` adds to the current value. Follow with `ui.key` Enter or a blur to commit. A `<select>` takes the option's value or text and fires `change`. |
| `ui.range_sequence` | webview | Bounded synthetic slider drag: `values` (1–600 finite numbers), `interval_ms` (4–100, default 60 Hz), range `name`/`selector`. Schedules inputs inside the webview, restores the original value before release, returns actual input times and epoch bounds. Avoids a separate QA roundtrip per input; still not physical pointer-to-display latency. |
| `ui.text` | webview | Visible text of the page or a named element. |
| `ui.viewport` | webview | Webview size, device pixel ratio, focus. |
| `state.get` | webview | Store value by path, e.g. `editor.selectedImage.path` (stores: activity, editor, library, process, settings, ui); functions dropped, arrays cut at `max_array`. |
| `state.wait` / `ui.wait` | webview | Poll until a store `path` is truthy or `equals` a value, or until an element matches; `timeout_ms`. |
| `dialog.answer` | webview | Queues what the next open dialogs return instead of showing the native panel: `path` (a path, a list of paths, or `null` = cancelled) or `paths` (one answer per dialog, in order). Covers dialogs opened through `openDialog` (`src/utils/fileDialog.ts`: Locate Photo, Locate Folder, Add folder, Move Folder…). |
| `dialog.state` / `dialog.clear` | webview | The answers still queued and the dialogs asked so far (title, default path, folder or file, what they returned; `native` = the panel was shown); `clear` drops both. |

Native input methods take `"activate": true` to bring the app to the front first. In an inactive window AppKit uses the first press only to activate the window, as with a real click, so a single click or a drag is lost without it (a double click still lands). It is off by default so that the channel does not take focus from someone at the Mac. Activation is asynchronous, and since macOS 14 an app cannot always activate itself while another app is frontmost: the client's `--activate` therefore waits until `window.info` reports focus and falls back to AppleScript activation. Events posted before the window is active are lost (one paired run opened nothing because the double click came too early).

Library tiles open with a native double click; the synthetic DOM double click does not open them.

Panels: `state.get` `ui.uiVisibility.modules` gives each module's layout (`library`/`develop` → `left`, `right`, `filmstrip`, `bars`; a missing entry means shown). Drive it as a user does: `ui.key` with `code` `Tab` (side panels), `Tab` + `shift` (everything but the photograph and its tool bar), `F7`/`F8`/`F6` (left, right, filmstrip), or `ui.click` on the edge strips (`Show the left panel`, `Show the right panel`, `Show the bars` at the top, `Show the bars` or `Show the bars and filmstrip` at the bottom; both bring the top bar and the status bar back). Tab acts only while the viewer, a photograph tile or nothing has the focus; on a field or button it stays focus traversal. A hidden side panel also comes in over the photograph 20 ms after the pointer reaches its 12 px strip (a pointer that leaves the window sooner does not show it) and goes 150 ms after the pointer leaves it (the stored layout stays hidden; the element carries `data-peek`). Test it with real pointer moves (`cliclick m:x,y` with the app active); synthetic `ui.click`/`ui.drag` do not hover.

`scripts/qa_control.py` wraps these (`call`, `click --name`, `drag`, `screenshot`, `--activate`). The app captures its own window, which needs no Screen Recording permission and works while it is in the background; `screencapture -l` from a shell does need that permission.

## What native input covers

macOS events are posted with `NSApplication.postEvent` into the app's own queue, so WebKit turns them into DOM events itself, with real time stamps, and `pointer_session` diagnostics record them like a mouse. They skip the HID driver and window server, and there is no physical display measurement: results are input-queue-to-GPU, not pointer-to-photon. Real-mouse runs remain the reference for event delivery.

First check (2026-10-03, M4 Pro, external 60 Hz display, profiling + QA build): from a background launch, a native double click opened FC3582 in Develop (first frame 206 ms after the click call returned), the capture showed the developed GPU image under the interface, and ten 1 s drags (60 points at 60 Hz) on the central line of one new linear layer each produced 59–60 live preview frames with input-to-GPU-complete medians of 17–25 ms (p95 23–39 ms). That was one layer, not the twenty-layer fixture of the real-mouse runs, and the drags rotated the gradient; it shows that the path works, not a comparable latency result.
