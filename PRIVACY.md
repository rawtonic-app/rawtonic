# Privacy

Rawtonic is a desktop app. Photos, catalogs, sessions and edits stay on
your computer and in the folders you choose. Rawtonic has no accounts,
analytics, telemetry or crash reporting, and its makers
receive no data from it.

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

Exported files carry the metadata you choose to keep. When you strip
metadata, a photo with areas made by a generative AI tool still records
that fact (IPTC Digital Source Type), as the EU AI Act requires; nothing
else is added.

## Sending reports to Rawtonic's makers (not in this version)

Rawtonic does not yet send problem reports (with an optional screenshot or
the photo's file), crash reports, camera-test results or usage statistics;
nothing of the kind exists in this version. They are planned as separate, optional ways to help (design:
`docs/decisions/0017-contribution-channels.md`): each would send only when you
click Send after seeing exactly what goes out, through Rawtonic's own small
relay service without an account, with Rawtonic trying to remove paths,
your user name and serial numbers (best effort: you see the exact text first), a report id that deletes your report, and reports deleted
after 90 days. Anonymous usage statistics would be off unless you switch them
on in Preferences. Each one will be described here, in full, in the version
that adds it.

Questions: open an issue at https://github.com/rawtonic-app/rawtonic.
