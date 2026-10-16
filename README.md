<p align="center">
  <picture>
    <source media="(prefers-color-scheme: dark)" srcset="docs/brand/rawtonic-horizontal-colour-on-dark.svg">
    <img src="docs/brand/rawtonic-horizontal-colour.svg" alt="Rawtonic" width="360">
  </picture>
</p>

<p align="center">
  A free, open-source <b>RAW developer</b> for photographers.<br>
  <b>Ingest → select → develop → retouch → deliver</b>, on your own Mac or Linux PC, with your originals left untouched.
</p>

<p align="center">
  <a href="https://github.com/rawtonic-app/rawtonic/releases"><img alt="Release" src="https://img.shields.io/github/v/release/rawtonic-app/rawtonic?include_prereleases&label=release&color=e8590c"></a>
  <img alt="Status: early alpha" src="https://img.shields.io/badge/status-early%20alpha-e8590c">
  <img alt="Platform: macOS (Apple Silicon) and Linux (x86-64)" src="https://img.shields.io/badge/platform-macOS%20Apple%20Silicon%20%C2%B7%20Linux%20x86--64-1f6feb">
  <img alt="License: AGPL-3.0" src="https://img.shields.io/badge/license-AGPL--3.0-blue">
</p>

<p align="center">
  <a href="#download"><b>Download</b></a> ·
  <a href="#from-card-to-client">Workflow</a> ·
  <a href="#what-it-does">Features</a> ·
  <a href="#keyboard">Keyboard</a> ·
  <a href="#bug-reports--feedback">Feedback</a> ·
  <a href="#license">License</a>
</p>

<p align="center">
  <a href="docs/images/readme-develop.jpg">
    <img src="docs/images/readme-develop.jpg" alt="Rawtonic Develop: a mountain lake in the workspace, with tone controls, histogram and filmstrip" width="860">
  </a>
</p>

> [!WARNING]
> **Rawtonic is an early alpha.** It is usable for real work, but features are still missing and you will run into bugs. Keep your originals backed up, as with any new software. Rawtonic never changes your RAW files: edits live beside them, in a project database and XMP sidecars.

## About

Most serious RAW software now comes with a subscription, a cloud account or both, and the free alternatives are capable but rarely feel at home in a paid shoot. I wanted a RAW developer that holds its own against the big professional editors in image quality, speed and workflow, and that I own outright. So I am building one, and sharing it.

Rawtonic is a one-person project that I work on in my spare time. Some weeks a lot happens, and sometimes it takes a while until the next release or a bug fix. Thanks for your patience, and for every bug report: they decide what gets fixed first.

## Download

Get the latest build from the [**Releases**](https://github.com/rawtonic-app/rawtonic/releases) page.

Rawtonic runs on **macOS** (Apple Silicon) and **Linux** (x86-64).

### macOS

Download `Rawtonic_<version>_aarch64.dmg` for Apple Silicon (M1 and later), macOS 13 or later. Open it and drag **Rawtonic** to **Applications**.

The app is not signed with an Apple Developer ID (yet), so macOS will refuse to open it at first, or claim it is "damaged". Clear the quarantine flag once in Terminal:

```sh
xattr -cr /Applications/Rawtonic.app
```

Or right-click (Control-click) the app → **Open** → **Open**; if macOS still refuses, go to **System Settings → Privacy & Security** and click **Open Anyway**.

### Linux

Download `Rawtonic_<version>_amd64.AppImage` for x86-64, make it executable and start it:

```sh
chmod +x Rawtonic_*_amd64.AppImage
./Rawtonic_*_amd64.AppImage
```

It runs on current distributions (Ubuntu 22.04 or later, Debian 12, Fedora and others with glibc 2.35 or later). If it does not start, install FUSE 2 (`sudo apt install libfuse2t64` on Ubuntu 24.04, `libfuse2` on older Ubuntu and Debian) or start it with `--appimage-extract-and-run`. The very first start can take a minute or two before the first photo appears while the graphics driver prepares Rawtonic's shaders; later starts are quick.

### Other systems

Intel Macs and Windows builds are not available yet; you can [build from source](docs/DEVELOPMENT.md) on them.

## From card to client

### 1 · Bring the shoot in

Start a **project** for a portrait sitting, a wedding or a product shoot. **Ingest** copies the photographs from the card, checks every copy against the original before it counts as safe, and can write a backup to a second drive at the same time. The card is never changed. You can also drag photographs from the Finder or your file manager straight onto the window. Shooting tethered into a folder? **Hot folder** picks up each new file as it arrives (optionally from subfolders too) and can apply a preset or the previous photo's settings, a rating and keywords straight away; it keeps watching when you reopen the project, and picks up what arrived while Rawtonic was closed.

Use a **catalog** for a growing photo collection. Imports copy into its own `Photos/year/date` folders by default; you can also keep photos in their existing folders. Albums organize photographs without moving them. Moved, renamed, added or deleted files outside Rawtonic? **Synchronize Folder** finds the changes, follows moved photos with their edits and lets you review everything before it is applied. The library menu can create a **portable copy** for another computer, make a **full verified backup**, or **restore a backup** into a new folder—including external originals and required edit resources.

Already have a Lightroom Classic catalog or a Capture One session? Rawtonic can import it, with ratings, labels, keywords, albums and translated edits.

### 2 · Find the keepers

<p align="center">
  <a href="docs/images/readme-library.jpg"><img src="docs/images/readme-library.jpg" alt="Library workspace with landscapes and studio portraits, ratings, picks, keywords and metadata" width="860"></a>
</p>

The **Library** shows the whole shoot. Rate with 1–5, pick and reject with P and ⇧X, add colour labels, then filter down to the frames that matter. Group keepers into albums and add keywords, captions and copyright while you are at it. Everything is written to standard XMP sidecars, so other software sees your ratings too.

### 3 · Develop

The **Develop** room gives the photograph the space: a filmstrip, a histogram and compact tool tabs. Work from the overall light and colour down to the details, compare with **Before/After** (Y), line up a series side by side in the **multi view** (N) and edit them all at once, go **full screen** (F), step back through **History**, copy settings across a series and save your own presets. Drag a slider to explore or type a number to be exact.

<table>
<tr>
<td width="50%" valign="top">
<a href="docs/images/readme-color.jpg"><img src="docs/images/readme-color.jpg" alt="Colour grading of a sunset landscape: cool shadows, warm highlights"></a>
<br><b>Colour.</b> White balance from the RAW, a colour editor, colour grading for shadows, midtones and highlights, black-and-white mixing and your own camera profiles or LUTs.
</td>
<td width="50%" valign="top">
<a href="docs/images/readme-masks.jpg"><img src="docs/images/readme-masks.jpg" alt="A subject mask on a portrait shown as a red overlay, loose hair strands included"></a>
<br><b>Local adjustments.</b> Brush, linear and radial masks, colour and luminance ranges, and one-click masks for the subject, sky, background, depth or people.
</td>
</tr>
<tr>
<td width="50%" valign="top">
<a href="docs/images/readme-multiview.jpg"><img src="docs/images/readme-multiview.jpg" alt="Multi view: four portraits side by side, the first one the reference"></a>
<br><b>Multi view.</b> Compare up to twelve selected photographs side by side, with a reference photo and a locked zoom, and edit them all at once.
</td>
<td width="50%" valign="top">
<a href="docs/images/readme-relight.jpg"><img src="docs/images/readme-relight.jpg" alt="Relight in a before/after split: left the portrait's original warm light, right a new cool light from the other side"></a>
<br><b>Relight</b> <i>(experimental)</i>. Light a photograph again after the shoot. Before/after split: on the left the original warm light, on the right a new cool light from the other side with the original turned down.
</td>
</tr>
</table>

### 4 · Retouch and merge

<p align="center">
  <a href="docs/images/readme-retouch.jpg"><img src="docs/images/readme-retouch.jpg" alt="Face retouch on a studio portrait: small blemishes healed and dark circles softened, skin texture kept" width="860"></a>
</p>

**Portrait retouch.** Rawtonic finds the faces and heals blemishes, evens out skin, softens dark circles and shine, each with one slider, for everyone in the photo or one person at a time, and keeps the skin's texture. Heal, clone and remove distractions by hand where you need to.

**Relight** _(experimental)_. Add up to four lights to a photograph after the fact. Rawtonic estimates the depth, the people's surfaces and the photo's own lighting, so a new light brightens a face's shadow side, the original light can be turned down and the subject casts its shadow on the background.

**Merge.** Combine brackets into an HDR, stitch panoramas (also from brackets), or focus-stack a macro series. The result is a new RAW (DNG) that you develop like any other photograph.

### 5 · Deliver

<p align="center">
  <a href="docs/images/readme-export.jpg"><img src="docs/images/readme-export.jpg" alt="Export dialog with three recipes ticked: high quality, web and a 16-bit Adobe RGB TIFF for print" width="860"></a>
</p>

Save **export recipes** for full-size delivery, web sizes, social media or TIFFs for retouching, and tick several to produce every version in one go, each with its own size, format, colour space, sharpening, watermark and folder. The **export queue** survives a restart: resume an interrupted job and retry failed photographs when you are ready.

## What it does

- **Originals stay yours.** Your RAW files are never modified or deleted. Ingest verifies every copy; edits live in a portable project and XMP sidecars.
- **Fast, on the graphics card.** The photograph is rendered on the GPU (on a Mac natively beneath the interface), so sliders, brushes and masks respond while you drag.
- **Light.** Exposure, highlights, shadows, whites and blacks, levels, point and region curves, clarity, structure, dehaze.
- **Colour.** RAW white balance, colour editor, colour grading, vibrance, black-and-white mix, camera profiles, CUBE LUTs, soft proofing for print and web colour spaces.
- **Detail.** Sharpening, noise reduction, AI Denoise\*, chromatic aberration removal and defringing.
- **Local adjustments.** Brush, linear and radial masks, colour and luminance ranges, subject, sky, background, depth and people masks\* (skin, hair, eyes, lips, clothes …), layer opacity.
- **Geometry.** Crop, straighten, automatic and guided perspective correction, automatic lens corrections.
- **Retouch.** Heal and clone with spots or brush strokes, object removal\*, automatic face retouch\*.
- **Relight\*** _(experimental)_. Up to four added lights with patterns; the photo's own light can be turned down.
- **Merge.** HDR, panorama, HDR panorama and focus stacking to a DNG; collages.
- **Film.** Convert scanned negatives; grain with five structures (even, soft, clumped, speckled, gritty) and vignette.
- **Output.** JPEG, TIFF, PNG, WebP, JPEG XL and AVIF in sRGB, Display P3, Adobe RGB or ProPhoto RGB; output sharpening for screen and print; watermarks and borders; metadata you choose to keep.
- **Private by design.** No account, no telemetry, no update checks. Rawtonic only goes online when you ask it to ([privacy](PRIVACY.md)).

<sub>\* Optional AI tools. They run on your computer, never in the cloud; each model is downloaded once, the first time you use the tool (see <a href="docs/MODELS.md">AI models</a>). Areas filled by a generative model are marked as such in exported files.</sub>

## Keyboard

| Keys                        | Action                         |
| :-------------------------- | :----------------------------- |
| **G** / **D**               | Library / Develop              |
| ← / →                       | previous / next photograph     |
| **1**–**5** / **0**         | star rating / clear it         |
| **P** / **⇧X** / **U**      | pick / reject / clear the flag |
| **Y**                       | before / after split           |
| hold **\\**                 | whole photograph before        |
| **N**                       | multi view of the selection    |
| **F**                       | full screen                    |
| **⇧Return** / **⌘Return**   | pin / clear the reference      |
| **C** / **R** / **B**       | crop / straighten / brush      |
| **M**                       | show or hide the mask overlay  |
| **J**                       | clipping warnings              |
| **X**                       | proof margin                   |
| **Tab** (on the photograph) | hide or show the panels        |
| ⌘Z / ⇧⌘Z                    | undo / redo                    |

The before / after split shows in the single view only: turning on the multi view turns the split off, and turning on the split leaves the multi view.

On Linux, **Ctrl** takes the place of **⌘** (Ctrl+Z, Ctrl+Return …). The keys follow what photographers already know from the big editors, and every shortcut can be changed in Settings.

## Status and known limitations

Rawtonic's goal is simple to state and hard to reach: every tool should be at least as good as the better of the leading professional editors, measured on real photographs. Some tools are there, others are not yet. In this alpha:

- **macOS on Apple Silicon and Linux (x86-64 AppImage).** Intel Macs and Windows builds come later. On Linux, Develop shows its preview through the web view rather than directly on the GPU, so it is not quite as immediate as on a Mac, and it has been tested on fewer systems (NVIDIA on KDE Wayland; exports also on Ubuntu, Debian and Fedora).
- **Cameras.** The focus is on professional and popular cameras and phones of the last ten years (Canon, Nikon, Sony, Fujifilm, Panasonic, OM System/Olympus, Leica, DJI, iPhone ProRAW and other DNGs). Older or rarer cameras may open with less accurate colour, or not at all. Nikon _High Efficiency_ NEFs are not supported yet; use lossless compressed RAW for now.
- **Importing from Lightroom or Capture One** brings over metadata, albums and most edits, but not everything: some adjustments, virtual copies and smart collections don't transfer, and photos can look different.
- **Relight is experimental.** The original light's own cast shadows stay in the photo, and the subject's shadow on the background does not touch the ground.
- **No tethering.** Direct camera control is not available; the hot folder works with your camera maker's tethering app.
- Expect rough edges in the interface, missing shortcuts and the occasional crash. Please [report them](#bug-reports--feedback).

## Bug reports & feedback

Please [**open a GitHub issue**](https://github.com/rawtonic-app/rawtonic/issues/new) for bugs, questions and feature ideas, rather than an e-mail. Issues keep the details, screenshots and follow-ups in one place, other photographers with the same problem can find them, and you are notified when a fix ships.

A useful bug report says which Rawtonic version you run (Settings → About), your system (macOS version and Mac model, or Linux distribution and graphics card), and, for photos that don't open or look wrong, the camera model and file type (for example Sony `.ARW`).

## Support

Rawtonic is free and open source, built in my spare time. If it helps you on a shoot and you would like to keep it going, you can buy me a coffee. Every bit is appreciated. ☕

[![Support me on Ko-fi](https://img.shields.io/badge/Support%20me-Ko--fi-FF5E5B?logo=kofi&logoColor=white)](https://ko-fi.com/nielsfranke)

## For developers

Rawtonic is written in Rust (Tauri, wgpu) with a React interface. See the [development guide](docs/DEVELOPMENT.md) to build and run it from source, the [architecture](docs/ARCHITECTURE.md) and [contributing](CONTRIBUTING.md). The `rawtonic` command-line tool creates sessions, ingests, rates and exports with the same renderer as the app.

## License

[GNU AGPL-3.0-or-later](LICENSE). Rawtonic is free software: use, study, modify and share it. If you distribute a modified version, you must release your changes under the same license and make the source available.

Contributions are welcome under a simple [Contributor License Agreement](CLA.md).

Rawtonic's editor started from [RapidRAW](https://github.com/CyberTimon/RapidRAW) by Timon Käch (commit `a82251f`), whose copyright and license notices are kept; see [NOTICE](NOTICE) and [third-party notices](THIRD_PARTY_NOTICES.md). This product includes DNG technology under license by Adobe.

Rawtonic is an independent project and is not affiliated with, endorsed or sponsored by Adobe, Capture One, Fujifilm or any camera maker. Their names are trademarks of their owners and appear only to say what Rawtonic imports from or works with ([trademarks](TRADEMARKS.md)).

Screenshots show a demo project with photographs from Pexels ([photography credits](docs/images/CREDITS.md)).
