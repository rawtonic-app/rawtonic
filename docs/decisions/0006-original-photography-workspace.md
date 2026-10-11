# ADR 0006 — Original photography workspace
Status: accepted, 2026-10-01. Supersedes the UI direction in ADR 0001 and the initial DESIGN.md.

The user clarified that reference projects are engineering references, not interface sources. RapidRAW's rounded panel shell and control layout are rejected. Keep attributed decoding/rendering and state adapters, but build the primary application interface independently.

Use Lightroom's Library/Develop distinction, fixed viewer tools, histogram and familiar adjustment order; use Capture One's compact tool tabs and session orientation. Build new library/browser, filmstrip, navigation, adjustment rows, tool groups, histogram, history and viewer toolbar. Keep advanced engine integrations reachable during migration, with their remaining UI migration documented. No cosmetic rebranding is accepted as the redesign.

Pinned direction: neutral dark continuous panels, fine separators, platform sans at compact desktop sizes, tabular numeric controls, stable canvas. This is a task-oriented photography application; no decorative identity exercise is needed. Original instructions prohibit questions, and this clarification settles the direction.

Responsiveness means both layout and interaction: resize/collapse panels, bounded thumbnail DOM, independent panel scrolling, keyboard/typed/sliding/scrubbing numeric input, immediate local feedback and frame-coalesced updates. UI tests and desktop/narrow captures verify behavior; no unmeasured 60-fps or engine-parity claims.

Reference conventions were checked against Adobe's Develop-module/workspace documentation and Capture One's User-interface/Tool-tabs documentation (links in DESIGN.md after implementation).
