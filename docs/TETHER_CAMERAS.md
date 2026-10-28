# Tethering: camera compatibility

Which cameras tethered shooting was tried with and how well it works. The list's data is `src/components/session/tetherCameras.json`; the tether panel shows its entry for the connected camera. Keep this page and the file in step (a test checks that every entry is listed here with its level).

Support levels:

- **tested** — measured on a body by the Rawtonic project (method: screen recording of the shutter request to the new photo shown in Develop, series of shots, settings, autofocus, shots fired on the body, cable pulled and plugged in again).
- **reported** — a user's camera test report says it works; not measured by the project.
- **experimental — testers wanted** — Rawtonic connects it only when you turn it on in Preferences > Tethering; no body has been tested yet (see [Help test a Canon camera](#help-test-a-canon-camera)).
- **not supported** — Rawtonic does not connect it.

A camera that is not listed has not been tried yet. Every camera is handled the same way; the list only says what is known.

| Camera | Level | USB mode | Tested on | Photo on screen, median / p95 | Shots | Notes |
|---|---|---|---|---|---|---|
| Sony α7 III | tested | PC Remote | macOS 26, 2026-10-19 | 1487 / 1510 ms | 100/100 | Live view, autofocus, settings, body shots and reconnect work. In AF-S a shot that finds no focus is not exposed; the camera reports nothing for it. |
| Sony α7 IV | tested | PC Remote | macOS 26, 2026-10-19 | 1140 / 1214 ms | 100/100 | Live view, autofocus, settings, body shots and reconnect work. The ISO list shows the extended values as "Multi Frame Noise Reduction"; choosing one changes nothing. |
| Fujifilm X-H2S | tested | USB tether shooting | macOS 26, 2026-10-19 | 1140 / 1281 ms | 50/50 | Transfer queue freed after each verified commit; keeps its card copy. Live view shows about 3 frames a second and stops after a shot. With image review set to Continuous the camera refuses the next remote shot until the review is dismissed. |
| Fujifilm GFX 50R | tested | USB tether shooting | macOS 26, 2026-10-19 | 2409 / 2585 ms | 50/50 | Transfer queue freed after each verified commit. No live view. Refuses shots as busy for about 3 seconds after connecting. Writes no card copy in USB tether mode: the library copy is the only one. |
| Canon EOS cameras | experimental — testers wanted | — | — | — | — | Only with Preferences > Tethering > Experimental: Canon cameras on and a memory card in the camera; every photo stays on the card, nothing is freed on the camera. The camera library can lose the report of a photo on these bodies: a shot that does not arrive is reported, and it is on the card. |

No body has been tried on Linux or Windows yet.

## Report your camera

1. Connect the camera in the tether panel (session bar → Tether) and open a photo in Develop.
2. Click **Camera test…** and run each step (skip what your camera cannot do).
3. Each step ends as pass, fail, skipped or inconclusive (the camera reused a file name, so its RAW and JPEG files could not be matched to shots for sure). The report at the end holds the camera model and firmware, the system, the Rawtonic version, the camera driver and each step's result and timings — no serial number, file names, folders, user name or photos.
4. Click **Send result…**: Rawtonic shows exactly what will be sent and sends it to its relay only when you click Send (no account needed; see [PRIVACY.md](../PRIVACY.md)). The same dialog can instead copy the report or open a [camera report](https://github.com/rawtonic-app/rawtonic/issues/new?template=camera-report.yml) on GitHub with the report filled in.

When a step fails, the app log (macOS: `~/Library/Logs/cc.rawtonic.editor/app.log`) says more: each setting change with the value the camera reports back, each shot's request number, the camera's report of every photo, download and library times, shots reported as not delivered, and the camera library's errors. It names the camera's file names but no folders; look through it before you paste a part of it into a report. Starting Rawtonic with `RAWTONIC_TETHER_LOG=debug` also logs every step of the camera library (a large log).

A report that a camera works adds it as **reported**; a body the project measures becomes **tested**.

## Help test a Canon camera

No Canon camera has been tested with Rawtonic yet, so Canon bodies are off by default. If you own one, your test result is the most useful thing you can send:

1. Open **Preferences > Tethering** and turn on **Experimental: Canon cameras (help test)**.
2. Put a memory card in the camera and keep it in for the whole time. Rawtonic connects a Canon only with a card in and sets the camera to save every photo on it, so a photo that does not reach Rawtonic is still on the card. Rawtonic never deletes anything on the camera or the card.
3. Connect the camera with the USB cable, open the tether panel (session bar → Tether) and click **Connect**. The panel marks the camera as *experimental*.
4. Click **Camera test…** and follow the steps as described above. When a shot does not arrive, Rawtonic says so ("Shot N was not received … it is on the memory card") and the report counts it — that is a useful result too.
5. Click **Send result…** (or copy the report, or open a [camera report](https://github.com/rawtonic-app/rawtonic/issues/new?template=camera-report.yml) on GitHub). The report says it comes from the experimental Canon path.

Why it is experimental: the camera library Rawtonic uses can, on Canon bodies, discard the camera's notice of a new photo when the next shot is fired, and a Canon camera without a card silently keeps photos only in its memory. Rawtonic therefore keeps every photo on the card, frees nothing on the camera and tells you about every shot that did not arrive.

Camera makers and models are named here only to say what Rawtonic works with ([TRADEMARKS.md](../TRADEMARKS.md)).
