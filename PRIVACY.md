# Privacy

Rawtonic is a desktop app. Photos, catalogs, sessions and edits stay on
your computer and in the folders you choose. Rawtonic has no accounts,
analytics or telemetry, and its makers receive no data from it unless you
send a problem or crash report yourself (below).

## Update checks

At startup, Rawtonic checks for new releases at most once every 24 hours.
This is on by default and is its only automatic network access. Turn off
“Check for updates at startup” in Preferences → General to stop it. “Check now”
requests a check even when the startup check is off.

Each check sends one GET to GitHub's public Releases API for
`rawtonic-app/rawtonic`, considering the ten most recent releases. GitHub receives your IP address and a User-Agent of
`Rawtonic/<version>`; no cookies, identifiers, photos, paths or settings are sent.
Requests time out after 10 seconds; failures count toward the 24-hour interval.
Startup failures are silent; a failed manual check shows a short message. Alpha/beta builds include pre-releases; stable builds ignore
them by default. “Download” opens the release page in your browser. Rawtonic
never downloads or installs an update itself.

## Other network access

Rawtonic otherwise goes online only when you ask it to:

- **AI models.** The first time you use an AI tool (masks, depth-based Relight, AI Denoise,
  object removal, automatic keywords), Rawtonic downloads that tool's model
  from Rawtonic's model pack on GitHub (github.com/rawtonic-app/models)
  and checks its SHA-256. GitHub sees your IP address, as with any
  download. Adding a Relight light starts local depth analysis and downloads the
  depth, face, subject and sky models if they are not installed yet, and for
  photos with people the people-normal model. No photo is uploaded; the models run on your computer, on its
  graphics card when it can (Windows, Linux: the WebGPU plugin ships with the
  app; using the GPU downloads nothing).
- **Map.** In the Library's EXIF tool, "Show on map" loads a map of the photo's
  GPS position from openstreetmap.org, which then learns that position and
  your IP address. Nothing is loaded until you click.
- **Generative AI server.** If you set up a generative AI server yourself
  (ComfyUI, in Preferences), Rawtonic sends the photo and mask you edit to
  that address. You control that server.
- **Links** in the app open in your browser.

Tethered shooting talks to the camera over its USB cable only; Rawtonic's
copy of the camera library is built without its network (Wi-Fi/PTP-IP)
driver. The camera test in the tether panel sends nothing: it shows its
report (camera model and firmware, system, Rawtonic version, camera driver
and the test's results and timings — no serial number, file names, folders,
user name or photos) for you to copy yourself; its GitHub button only opens
the camera report form in your browser.

Exported files carry the metadata you choose to keep. Under the explicit
[maintainer decision](docs/decisions/0018-export-rights-retention.md), stripped
exports, including existing strip recipes, retain creator, copyright and usage
terms. Creator contact details (email, phone, website, street address, city,
region, postal code and country) are omitted by default. In Export > Metadata,
"Include creator contact details" explicitly retains them when other metadata
is stripped; both CLIs offer `--keep-creator-contact`. GPS, camera and serial
fields are stripped. Rawtonic's creating-tool name is written. A photo with
areas made by a generative AI tool also records that fact (IPTC Digital Source
Type), as the EU AI Act requires.

## Sending reports to Rawtonic's makers

Two optional ways to report problems exist in this version. Each sends only
when you click Send after seeing exactly what goes out ("Review what will be
sent"; you can go back and change or remove parts), through Rawtonic's own
small relay service (`relay.rawtonic.cc`, run on Cloudflare; design:
`docs/decisions/0017-contribution-channels.md`), without an account. Rawtonic
tries to remove paths, your user and computer name, serial numbers, e-mail
and IP addresses and GPS positions from the text first; this is best effort,
so check the text you see. The relay files each report privately for
Rawtonic's makers (an issue in a private GitHub repository, a screenshot in
private storage), keeps no IP address, and deletes reports after 90 days.
After sending you see a report id: "Delete this report now", or an e-mail
with the id to hello@rawtonic.cc, deletes the report at any time. Cloudflare
sees your IP address like any web host. Builds made without the relay cannot
send at all; their dialogs offer only the two ways below.

Every report dialog can also "Copy report" (to paste where you like; nothing
is sent) or "Open on GitHub": it first shows the exact title and text, then
your browser opens GitHub's public issue form with them in its link (no
screenshot), so GitHub receives that text when the page loads. Nothing is
filed until you submit it there with your own GitHub account.

- **Report a problem** (Help menu on macOS, the "…" menu in Library and
  Develop): the text you write, plus, each switchable: system details
  (graphics card), the last 200 lines of Rawtonic's log (editable), the
  camera make, model and file type of the open photo (not the photo), and a
  screenshot of Rawtonic's window (macOS; off unless you tick it). Rawtonic's
  version, your operating system and its version, and the processor type are
  always included. The screenshot is taken when the dialog opens and is kept
  only in memory.
- **Crash report on the next start.** When Rawtonic stops because of an error
  in its own code (a Rust panic) or a fatal error signal, it keeps a crash
  record on your computer, in its settings folder (`crash-reports`): the
  version, operating system, graphics card, the error message and the place
  in the source code where it happened (the message can be corrected before
  sending), the names of the functions that were
  running with their source file names (no folders), and the last 200 lines
  of the log of that run, with the same removal of personal data. A quit
  forced from outside or a power loss leaves no record. At the next start
  Rawtonic asks once per crash whether to send it. "Not now" keeps the record
  (you can still send it from "Report a problem"), "Never" deletes all
  records and stops recording at once (Preferences → General turns it back
  on).
  Records are deleted after they are sent or after 30 days.

Camera-test results, RAW samples and anonymous usage statistics are not sent
by this version; each will be described here, in full, in the version that
adds it.

Questions: open an issue at https://github.com/rawtonic-app/rawtonic.
