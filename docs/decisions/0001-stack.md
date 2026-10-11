# ADR 0001 — Locked stack and incremental extraction
Status: accepted, 2026-10-01.
Use Rust 2024, Tauri 2, React/TypeScript/zustand/Tailwind and wgpu. Reuse the audited RapidRAW snapshot, retaining its copyright and AGPL license, for the native renderer/editor. A rewrite of a mature renderer would discard working capabilities and cannot be validated without a camera corpus.
Blocking constraints: RapidRAW's engine modules depend on Tauri AppState; it is not a standalone engine crate, uses CPU RAW demosaic, and does not use linear ProPhoto throughout. Keep this adapter temporarily while extracting pure core crates. These are explicit architecture gaps, not alternate implementations claimed as complete.
Create standalone catalog, naming, ingest, metadata and CLI first. Do not add empty crates pretending the requested separation exists. Engine/gpu/rawdecode/color/lens/export/tether/ai remain documented extraction boundaries until each has a real standalone implementation.
Retain the native canvas and do not transport Develop pixels across IPC. Small thumbnails are permitted. Do not replace rendering with CSS, browser canvas filters, or fabricated previews.
