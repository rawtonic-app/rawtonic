# Privacy

Rawtonic is a desktop app. Photos, catalogs, sessions and edits stay on
your computer and in the folders you choose. Rawtonic has no accounts,
analytics, telemetry, crash reporting or update checks, and its makers
receive no data from it.

Rawtonic goes online only when you ask it to:

- **AI models.** The first time you use an AI tool (masks, depth-based Relight, AI Denoise,
  object removal, automatic keywords), Rawtonic downloads that tool's model
  from Rawtonic's model pack on GitHub (github.com/rawtonic-app/models)
  and checks its SHA-256. GitHub sees your IP address, as with any
  download. Adding a Relight light starts local depth analysis and downloads the
  depth and face models if they are not installed yet. No photo is uploaded; the models run on your computer, on its
  graphics card when it can (Windows, Linux: the WebGPU plugin ships with the
  app; using the GPU downloads nothing).
- **Map.** In the Library's EXIF tool, "Show on map" loads a map of the photo's
  GPS position from openstreetmap.org, which then learns that position and
  your IP address. Nothing is loaded until you click.
- **Generative AI server.** If you set up a generative AI server yourself
  (ComfyUI, in Preferences), Rawtonic sends the photo and mask you edit to
  that address. You control that server.
- **Links** in the app open in your browser.

Exported files carry the metadata you choose to keep. When you strip
metadata, a photo with areas made by a generative AI tool still records
that fact (IPTC Digital Source Type), as the EU AI Act requires; nothing
else is added.

Questions: open an issue at https://github.com/rawtonic-app/rawtonic.
