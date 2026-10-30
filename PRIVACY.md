# Privacy

Rawtonic is a desktop app. Photos, catalogs, sessions and edits stay on
your computer and in the folders you choose. Rawtonic has no accounts or
telemetry, and its makers receive no data from it unless you send a report
yourself or switch on anonymous usage statistics (both below).

## Update checks

At startup, Rawtonic checks for new releases at most once every 24 hours.
This is on by default and is its only automatic network access, apart from
the usage statistics if you switch them on (below). Turn off
“Check for updates at startup” in Preferences → General to stop it. “Check now”
requests a check even when the startup check is off.

Each check sends one GET to GitHub's public Releases API for
`rawtonic-app/rawtonic`, considering the ten most recent releases. GitHub receives your IP address and a User-Agent of
`Rawtonic/<version>`; no cookies, identifiers, photos, paths or settings are sent.
Requests time out after 10 seconds; failures count toward the 24-hour interval.
Startup failures are silent; a failed manual check shows a short message. Alpha/beta builds include pre-releases; stable builds ignore
them by default. “Download” opens the installer for your computer in your
browser, which downloads it from GitHub; Rawtonic picks it from the file names in
the same response, using only its own build, processor and, on Linux,
`/etc/os-release`, and opens the release page when no file fits. “What's new”
shows the release notes from the same response; pictures and other remote
content in them are not loaded, and links open in your browser. Rawtonic never
downloads or installs an update itself. The “What's new” shown once after an
update comes from the notes bundled with the app and needs no network access.

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
- **Links** in the app open in your browser.

Tethered shooting talks to the camera over its USB cable only; Rawtonic's
copy of the camera library is built without its network (Wi-Fi/PTP-IP)
driver. The camera test in the tether panel sends its report only when you
choose to (below).

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

Three optional ways to send reports exist in this version. Each sends only
when you click Send after seeing exactly what goes out ("Review what will be
sent"; you can go back and change or remove parts), through Rawtonic's own
small relay service (`relay.rawtonic.cc`, run on Cloudflare; design:
`docs/decisions/0017-contribution-channels.md`), without an account. Rawtonic
tries to remove paths, your user and computer name, serial numbers, e-mail
and IP addresses and GPS positions from the text first; this is best effort,
so check the text you see. The relay files each report privately for
Rawtonic's makers (an issue in a private GitHub repository, a screenshot or
photo file in private storage), keeps no IP address, and deletes reports
with their files after 90 days.
After sending you see a report id: "Delete this report now", or an e-mail
with the id to hello@rawtonic.cc, deletes the report at any time. Cloudflare
sees your IP address like any web host. Builds made without the relay cannot
send at all; their dialogs offer only the two ways below.

Every report dialog can also "Copy report" (to paste where you like; nothing
is sent) or "Open on GitHub": it first shows the exact title and text, then
your browser opens GitHub's public issue form with them in its link (no
screenshot or photo file), so GitHub receives that text when the page loads. Nothing is
filed until you submit it there with your own GitHub account.

- **Report a problem** (Help menu on macOS, the "…" menu in Library and
  Develop): the text you write, plus, each switchable: system details
  (graphics card), the last 200 lines of Rawtonic's log (editable), the
  camera make, model and file type of the open photo, a screenshot of
  Rawtonic's window (macOS; off unless you tick it), and the photo's file
  itself (off unless you tick "Attach the photo file"). An attached file is
  uploaded unchanged, with all its metadata — the date, the camera's serial
  number, a GPS position if one was recorded — and may show people; it is
  used only to find the problem, never published, and deleted with its
  report. Rawtonic's
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
- **Camera test result** (tether panel → Camera test… → Send result…): the
  test's report — camera model and firmware, system, Rawtonic version,
  camera driver and each step's result and timings; no serial number, file
  names, folders, user name or photos — with Rawtonic's version, your
  operating system and its version and the processor type. Its GitHub form is
  the public camera report form, with the report filled in.

**Help support this camera.** When Rawtonic cannot develop a RAW file (an
unsupported camera or RAW mode), it offers a short guide to sharing a sample
on raw.pixls.us, a public collection of RAW samples that open-source decoders
learn new cameras from. Rawtonic sends nothing there: its buttons open
raw.pixls.us's pages in your browser, and you upload the file there yourself
under that site's terms (samples there are public, CC0). A RAW file keeps its
metadata, such as the date, the camera's serial number and a GPS position if
one was recorded. The same guide can open "Report a problem" for that file
instead.

## Anonymous usage statistics

Off unless you switch on "Send anonymous usage statistics once a week" in
Preferences → General. While it is on, Rawtonic keeps on your computer (in its
settings folder, `usage-stats.json`), per calendar week (ISO week, UTC), only:
the camera makes and models of the photos you opened, each once (no counts),
only for bodies on the camera list of Rawtonic's RAW decoder and by the name
that list gives (a name a file's metadata was edited to carry is not kept);
which tools from a fixed list you used at all (yes or no — no counts, values,
times or photos); Rawtonic's version, your operating system and its version,
the processor type and the maker of your graphics card (Apple, AMD, Intel,
NVIDIA, Qualcomm or other). At the first start of a later week Rawtonic sends
each finished week at most once to its relay (`relay.rawtonic.cc`), about
30 seconds after the start (a week that could not reach the relay at all is
tried at the next start); a week not sent within two weeks of its end is
deleted unsent. A send carries no id, no session or install number, and no time finer
than the week. The relay only adds one to a counter per value (for example
"camera Sony ILCE-7M3, week 43") and keeps neither the send itself nor your IP
address, and its makers see a value only when at least ten sends counted it
that week. This limits what is kept; it does not make a send anonymous in
transit: Cloudflare, which runs the relay, sees your IP address like any web
host, and an unusual combination of cameras and system could stand out
there. "Show what is sent" in Preferences
shows every week waiting to be sent at any time, in plain words or as its
exact data, and the last one sent. Switching it off deletes everything kept and drops a send under
way (data already handed to the network at that moment may still arrive);
nothing more is sent, and switching it on again starts from nothing kept. If
the choice or the deletion cannot be saved, Preferences says so. Statistics cannot be deleted one by one afterwards, as they are only
counters.

RAW sample uploads to Rawtonic are not part of this version.

Questions: open an issue at https://github.com/rawtonic-app/rawtonic.
